//! Kotlin grammar that emits parser-core events into the SIL pipeline.
//!
//! Design constraints:
//!
//! 1. **Lossless.** Every token from the lexer (including trivia —
//!    whitespace, line/block/doc comments, newlines) is `bump`ed and
//!    therefore appears as a leaf in the tree. Concatenating all
//!    leaves reproduces the source.
//! 2. **Permissive.** On a grammar mismatch we mark the offending
//!    region with `Marker::complete(p, ERROR_ELEMENT)` and keep
//!    going. The SIL roundtrip succeeds even when the file is
//!    syntactically malformed — there's no requirement to bail.
//! 3. **Mirrors kotlinc PSI.** Composite nesting matches kotlinc's
//!    `IElementType` layout so YAML diffs against the reference
//!    `psi.yaml` are stable.
//!
//! The grammar covers what `CliktTesting.kt` needs (~80% of real-world
//! Kotlin idioms): packages, imports, classes (data/normal/sealed/enum
//! /interface/object), primary + secondary constructors, properties,
//! functions (with receiver types and default params), modifier
//! lists, annotation entries, KDoc, type expressions (user types,
//! nullable, function types, generics), expressions through full
//! precedence (elvis through postfix), lambdas, string templates,
//! when/if/for/while/try, returns, throws, labels.
//!
//! What's deliberately *not* covered (returns ERROR_ELEMENT or
//! best-effort consume):
//!  - Contracts blocks (`contract { ... }`).
//!  - `expect`/`actual` declaration headers.
//!  - Some edge-case modifier orders.
//!
//! These are TODOs for Phase 6 in the master plan.

use skotch_parser_core::{CompletedMarker, Parser};
use skotch_syntax::SyntaxKind as S;
use skotch_syntax::SyntaxKind;

pub fn parse_file_root(p: &mut Parser<'_, '_>) {
    let file = p.start();
    // Leading trivia at the very top of the file sits under FILE.
    // This INCLUDES line + block comments (a license header is a
    // common case). Only consume them HERE — between-decl trivia is
    // WS-only, so comments later attach to the following decl.
    consume_leading_file_trivia(p);
    if !p.at(S::EOF) {
        parse_optional_kdoc_then_file_annotations(p);
    }
    // kotlinc PSI always emits a PACKAGE_DIRECTIVE (empty when there
    // is no `package` keyword) and IMPORT_LIST as the first two
    // children of FILE — even if both are empty.
    if next_non_trivia_is(p, S::KW_PACKAGE) {
        consume_list_level_trivia(p);
        parse_package_directive(p);
    } else {
        let m = p.start();
        m.complete(p, S::PACKAGE_DIRECTIVE);
    }
    // Only consume trivia between PACKAGE_DIRECTIVE and IMPORT_LIST
    // when an actual `import` follows; otherwise the empty
    // IMPORT_LIST attaches immediately and the trivia sits after it.
    if next_non_trivia_is(p, S::KW_IMPORT) {
        consume_list_level_trivia(p);
    }
    parse_import_list(p);
    consume_list_level_trivia(p);
    // Top-level declarations. Each one absorbs its own leading KDoc
    // / annotation / modifier list, so we only skip WS-style trivia
    // at the FILE level here.
    while !p.at(S::EOF) {
        parse_top_level_decl(p);
        consume_list_level_trivia(p);
    }
    file.complete(p, S::FILE);
}

// ─── trivia handling ─────────────────────────────────────────────────────────

/// Consume any trivia tokens (whitespace, comments, doc comments,
/// newlines) by `bump`ing each. Every trivia token becomes a leaf.
fn skip_trivia(p: &mut Parser<'_, '_>) {
    loop {
        let k = p.current();
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            p.bump();
        } else {
            break;
        }
    }
}

/// Consume only inline whitespace — `WHITE_SPACE` tokens whose text
/// contains no newline. Used inside composites that must respect
/// Kotlin's newline-sensitivity: a newline after a complete
/// expression ends the expression, so a dotted name parser must not
/// eagerly swallow the newline that follows the last segment.
///
/// Important: kotlinc PSI uses a unified `WHITE_SPACE` token that may
/// contain newlines (the SIL-mode lexer emits the same), so we have
/// to peek the text to differentiate.
fn skip_ws(p: &mut Parser<'_, '_>) {
    while p.at(S::WHITE_SPACE) && !p.current_text().contains('\n') {
        p.bump();
    }
}

// ─── file-level pieces ───────────────────────────────────────────────────────

/// File-level annotations like `@file:JvmName("...")`. Rarely seen in
/// real-world code but supported for completeness — at file root,
/// before any package directive.
fn parse_optional_kdoc_then_file_annotations(p: &mut Parser<'_, '_>) {
    if !(p.at(S::AT) && next_non_trivia_is_file(p)) {
        return;
    }
    let list = p.start();
    while p.at(S::AT) && next_non_trivia_is_file(p) {
        let m = p.start();
        p.bump(); // @
                  // The `file` token is lexed as an IDENTIFIER (soft keyword);
                  // we re-classify it as `KW_FILE` inside the annotation-target
                  // composite so the YAML output emits `file` rather than `IDENTIFIER`.
        if p.at(S::IDENTIFIER) && p.current_text() == "file" {
            let tgt = p.start();
            p.bump_as(S::KW_FILE);
            tgt.complete(p, S::ANNOTATION_USE_SITE_TARGET);
        }
        if p.at(S::COLON) {
            p.bump();
        }
        // Annotation type reference wrapped in CONSTRUCTOR_CALLEE.
        if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
            let callee = p.start();
            let tref = p.start();
            let ut = p.start();
            parse_user_type_segment(p);
            ut.complete(p, S::USER_TYPE);
            tref.complete(p, S::TYPE_REFERENCE);
            callee.complete(p, S::CONSTRUCTOR_CALLEE);
        }
        if p.at(S::LPAR) {
            parse_value_argument_list(p);
        }
        m.complete(p, S::ANNOTATION_ENTRY);
        // Skip trivia between consecutive file annotations only when
        // another `@file:` follows; else leave for the outer caller.
        if !next_non_trivia_at_starts_file_annotation(p) {
            break;
        }
        skip_trivia(p);
    }
    list.complete(p, S::FILE_ANNOTATION_LIST);
}

fn next_non_trivia_at_starts_file_annotation(p: &Parser<'_, '_>) -> bool {
    let mut i = 0;
    loop {
        let k = p.nth(i);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            i += 1;
            continue;
        }
        if k != S::AT {
            return false;
        }
        // Peek past `@` for `file`.
        let mut j = i + 1;
        loop {
            let k2 = p.nth(j);
            if matches!(
                k2,
                S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT
            ) {
                j += 1;
                continue;
            }
            return k2 == S::KW_FILE;
        }
    }
}

fn next_non_trivia_is_file(p: &Parser<'_, '_>) -> bool {
    // `file` is a soft keyword that the lexer surfaces as an
    // IDENTIFIER. Detect by token text. The token AFTER must be
    // `:` for it to be a file-level annotation target.
    let mut i = 1;
    loop {
        let k = p.nth(i);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            i += 1;
            continue;
        }
        if k != S::IDENTIFIER || p.text_at(i) != "file" {
            return false;
        }
        let mut j = i + 1;
        loop {
            let k2 = p.nth(j);
            if matches!(
                k2,
                S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT
            ) {
                j += 1;
                continue;
            }
            return k2 == S::COLON;
        }
    }
}

fn parse_package_directive(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // package
    skip_trivia(p);
    if !p.at(S::NEWLINE) && !p.at(S::EOF) {
        parse_qualified_name(p);
    }
    m.complete(p, S::PACKAGE_DIRECTIVE);
}

fn parse_import_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    while p.at(S::KW_IMPORT) {
        parse_import_directive(p);
        // Trivia BETWEEN consecutive import directives is part of
        // IMPORT_LIST; trivia AFTER the last directive belongs to the
        // file-level caller (so the `\n\n` separating imports from
        // the next decl sits at FILE level, matching kotlinc PSI).
        if !next_non_trivia_is(p, S::KW_IMPORT) {
            break;
        }
        skip_trivia(p);
    }
    m.complete(p, S::IMPORT_LIST);
}

/// True iff the next non-trivia token has the given kind. Used at
/// composite boundaries to decide whether trailing trivia belongs to
/// the current composite or to the outer caller.
fn next_non_trivia_is(p: &Parser<'_, '_>, kind: SyntaxKind) -> bool {
    let mut i = 0;
    loop {
        let k = p.nth(i);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            i += 1;
            continue;
        }
        return k == kind;
    }
}

fn parse_import_directive(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // import
    skip_ws(p);
    // The dotted name is its own DOT_QUALIFIED_EXPRESSION chain. The
    // optional trailing `.*` lives at the IMPORT_DIRECTIVE level —
    // kotlinc PSI does NOT wrap `*` inside the dot-qualified chain.
    parse_qualified_name(p);
    skip_ws(p);
    if p.at(S::DOT) {
        // `.*` is `DOT` + `MUL` as direct children of IMPORT_DIRECTIVE.
        p.bump();
        skip_ws(p);
        if p.at(S::MUL) {
            p.bump();
        }
        skip_ws(p);
    }
    if p.at(S::KW_AS) {
        let alias = p.start();
        p.bump();
        skip_ws(p);
        if p.at(S::IDENTIFIER) {
            p.bump();
        }
        alias.complete(p, S::IMPORT_ALIAS);
    }
    m.complete(p, S::IMPORT_DIRECTIVE);
}

/// `foo.bar.baz` — pure dotted-identifier chain, no `*` or aliases.
/// Stops cleanly at the LAST identifier; the caller handles any
/// trailing `.*` or `as alias` if applicable. Critical: we peek past
/// the `.` for an identifier before committing — otherwise we'd
/// swallow the `.` of a `.*` import and leave an empty
/// REFERENCE_EXPRESSION behind.
///
/// kotlinc accepts ANY keyword as a name segment after a `.` (the
/// "any-keyword-as-identifier" rule). The lexer surfaces these as
/// their `KW_*` kinds, so we `bump_as(IDENTIFIER)` to reclassify.
fn parse_qualified_name(p: &mut Parser<'_, '_>) {
    let mut lhs = parse_reference_expression(p);
    loop {
        // Don't skip whitespace — Kotlin's dotted names are not
        // newline-tolerant. A space after a name terminates the chain.
        if !p.at(S::DOT) {
            break;
        }
        let after_dot = next_non_trivia(p, 1);
        if !can_be_name_in_qualified_chain(after_dot) {
            break;
        }
        let dot_m = lhs.precede(p);
        p.bump(); // .
        skip_ws(p);
        let r = p.start();
        // Reclassify any keyword token as IDENTIFIER so the YAML
        // matches kotlinc PSI (which always emits `IDENTIFIER` here).
        if p.current() == S::IDENTIFIER {
            p.bump();
        } else {
            p.bump_as(S::IDENTIFIER);
        }
        r.complete(p, S::REFERENCE_EXPRESSION);
        lhs = dot_m.complete(p, S::DOT_QUALIFIED_EXPRESSION);
    }
}

/// `true` if `k` can serve as an identifier-like name segment after
/// a `.` in a qualified name. This includes plain `IDENTIFIER` and
/// every soft AND hard keyword — kotlinc accepts `foo.import.bar`,
/// `kotlin.internal.X`, and so on.
fn can_be_name_in_qualified_chain(k: SyntaxKind) -> bool {
    k == S::IDENTIFIER
        || is_soft_keyword(k)
        || is_modifier_keyword(k)
        || is_soft_modifier_keyword(k)
        || matches!(
            k,
            // Hard keywords that kotlinc lets you use as names after `.`.
            S::KW_FUN
                | S::KW_VAL
                | S::KW_VAR
                | S::KW_IF
                | S::KW_ELSE
                | S::KW_RETURN
                | S::KW_TRUE
                | S::KW_FALSE
                | S::KW_NULL
                | S::KW_WHILE
                | S::KW_DO
                | S::KW_WHEN
                | S::KW_FOR
                | S::KW_IN
                | S::KW_BREAK
                | S::KW_CONTINUE
                | S::KW_CLASS
                | S::KW_OBJECT
                | S::KW_PACKAGE
                | S::KW_IMPORT
                | S::KW_THROW
                | S::KW_TRY
                | S::KW_CATCH
                | S::KW_FINALLY
                | S::KW_IS
                | S::KW_AS
                | S::KW_SUPER
                | S::KW_INIT
                | S::KW_CONSTRUCTOR
                | S::KW_THIS
                | S::KW_TYPEALIAS
        )
}

fn parse_reference_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
        p.bump();
    } else {
        p.error("expected identifier");
        // Don't bump — let outer logic decide.
    }
    m.complete(p, S::REFERENCE_EXPRESSION)
}

// ─── declarations ────────────────────────────────────────────────────────────

fn parse_top_level_decl(p: &mut Parser<'_, '_>) {
    if p.at(S::EOF) {
        return;
    }

    // Modifier list + annotations are common decl prefix. A leading
    // KDoc / EOL_COMMENT / BLOCK_COMMENT belongs to the decl, NOT to
    // the FILE (matches kotlinc PSI which places these under FUN/CLASS).
    let m = p.start();
    while matches!(p.current(), S::KDOC | S::LINE_COMMENT | S::BLOCK_COMMENT) {
        p.bump();
        if p.at(S::WHITE_SPACE) {
            p.bump();
        }
    }
    let has_modifiers = parse_modifier_list_opt(p);
    skip_trivia(p);

    // `typealias` lexes as IDENT in our lexer; reclassify when it
    // appears at a decl introducer position.
    if p.at(S::IDENTIFIER) && p.current_text() == "typealias" {
        p.bump_as(S::KW_TYPEALIAS);
        parse_typealias_body_after_kw(p);
        m.complete(p, S::TYPEALIAS);
        return;
    }
    // `enum class` triggers the enum-entry-aware class body parser.
    // Detect it here so that `enum` consumed as a soft modifier above
    // doesn't lose its `entries-allowed` signal.
    let is_enum_class = p.at(S::KW_CLASS) && was_enum_modifier(p);
    let kind = match p.current() {
        S::KW_CLASS if is_enum_class => {
            parse_enum_class_body(p);
            S::CLASS
        }
        S::KW_CLASS | S::KW_INTERFACE => {
            parse_class_or_interface_body(p);
            S::CLASS
        }
        S::KW_OBJECT => {
            parse_object_decl_body(p);
            S::OBJECT_DECLARATION
        }
        S::KW_FUN => {
            parse_fun_body(p);
            S::FUN
        }
        S::KW_VAL | S::KW_VAR => {
            parse_property_body(p);
            S::PROPERTY
        }
        S::KW_TYPEALIAS => {
            parse_typealias_body(p);
            S::TYPEALIAS
        }
        S::KW_ENUM => {
            // `enum class Foo`
            p.bump();
            skip_trivia(p);
            if p.at(S::KW_CLASS) {
                parse_enum_class_body(p);
                S::CLASS
            } else {
                m.complete(p, S::ERROR_ELEMENT);
                return;
            }
        }
        _ => {
            if has_modifiers {
                p.error("expected declaration after modifiers");
                m.complete(p, S::ERROR_ELEMENT);
            } else {
                // Unknown — consume one token to make progress.
                let err = p.start();
                p.bump();
                err.complete(p, S::ERROR_ELEMENT);
                m.abandon(p);
            }
            return;
        }
    };
    m.complete(p, kind);
}

fn parse_modifier_list_opt(p: &mut Parser<'_, '_>) -> bool {
    // Trivia BETWEEN modifiers stays inside MODIFIER_LIST; trivia
    // AFTER the last modifier is left for the outer composite, since
    // kotlinc places that whitespace between `MODIFIER_LIST` and the
    // following decl keyword (`class`/`fun`/etc.) at the *parent*
    // level. We achieve this by only skipping trivia when the next
    // non-trivia token is itself a modifier kind.
    if !looks_like_modifier(p.current()) && !looks_like_modifier_after_trivia(p) {
        return false;
    }
    let m = p.start();
    let mut emitted_any = false;
    loop {
        let k = p.current();
        // Annotation entry (`@Foo(args)`).
        if k == S::AT {
            parse_annotation_entry(p);
            emitted_any = true;
            if !looks_like_modifier_after_trivia(p) {
                break;
            }
            skip_trivia(p);
            continue;
        }
        if is_modifier_keyword(k) {
            p.bump();
            emitted_any = true;
            if !looks_like_modifier_after_trivia(p) {
                break;
            }
            skip_trivia(p);
            continue;
        }
        // `public` / `inner` are soft keywords that the lexer
        // surfaces as IDENTIFIER. Reclassify the token via `bump_as`
        // when followed by a decl keyword.
        if let Some(reclass) = ident_text_as_soft_modifier(p) {
            if soft_modifier_followed_by_decl(p) {
                p.bump_as(reclass);
                emitted_any = true;
                if !looks_like_modifier_after_trivia(p) {
                    break;
                }
                skip_trivia(p);
                continue;
            }
        }
        if is_soft_modifier_keyword(k) && soft_modifier_followed_by_decl(p) {
            p.bump();
            emitted_any = true;
            if !looks_like_modifier_after_trivia(p) {
                break;
            }
            skip_trivia(p);
            continue;
        }
        break;
    }
    if emitted_any {
        m.complete(p, S::MODIFIER_LIST);
        true
    } else {
        m.abandon(p);
        false
    }
}

fn looks_like_modifier(k: SyntaxKind) -> bool {
    k == S::AT || is_modifier_keyword(k) || is_soft_modifier_keyword(k)
}

/// If the current token is an IDENTIFIER whose text is one of the
/// soft-keyword modifiers (`public`, `inner`) the lexer doesn't
/// recognize as a hard keyword, return the SIL kind we should
/// reclassify it as. Otherwise None.
fn ident_text_as_soft_modifier(p: &Parser<'_, '_>) -> Option<SyntaxKind> {
    if !p.at(S::IDENTIFIER) {
        return None;
    }
    match p.current_text() {
        "public" => Some(S::KW_PUBLIC),
        "inner" => Some(S::KW_INNER),
        "companion" => Some(S::KW_COMPANION),
        "data" => Some(S::KW_DATA),
        "sealed" => Some(S::KW_SEALED),
        "annotation" => Some(S::KW_ANNOTATION),
        "open" => Some(S::KW_OPEN),
        "abstract" => Some(S::KW_ABSTRACT),
        "override" => Some(S::KW_OVERRIDE),
        "operator" => Some(S::KW_OPERATOR),
        "infix" => Some(S::KW_INFIX),
        "lateinit" => Some(S::KW_LATEINIT),
        "tailrec" => Some(S::KW_TAILREC),
        "suspend" => Some(S::KW_SUSPEND),
        "external" => None,
        "expect" => Some(S::KW_EXPECT),
        "actual" => Some(S::KW_ACTUAL),
        "crossinline" => Some(S::KW_CROSSINLINE),
        "noinline" => Some(S::KW_NOINLINE),
        "reified" => Some(S::KW_REIFIED),
        "value" => Some(S::KW_VALUE),
        "vararg" => Some(S::KW_VARARG),
        "const" => Some(S::KW_CONST),
        _ => None,
    }
}

/// Peek past any trivia for a modifier-like token. Used to decide
/// whether the current modifier-list scan should keep consuming.
fn looks_like_modifier_after_trivia(p: &Parser<'_, '_>) -> bool {
    let mut i = 0;
    loop {
        let k = p.nth(i);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            i += 1;
            continue;
        }
        if k == S::AT || is_modifier_keyword(k) {
            return true;
        }
        // Identifier-as-soft-modifier (`public`, `inner`, `companion`,
        // `data`, `sealed`). Use the same "must be followed by a decl
        // introducer" check as the built-in soft modifiers.
        if k == S::IDENTIFIER
            && matches!(
                p.text_at(i),
                "public"
                | "inner"
                | "companion"
                | "data"
                | "sealed"
                | "annotation"
                | "open"
                | "abstract"
                | "override"
                | "operator"
                | "infix"
                | "lateinit"
                | "tailrec"
                | "suspend"
                | "expect"
                | "actual"
                | "crossinline"
                | "noinline"
                | "reified"
                | "value"
                | "vararg"
                | "const"
            )
        {
            let mut j = i + 1;
            loop {
                let kk = p.nth(j);
                if matches!(
                    kk,
                    S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
                ) {
                    j += 1;
                    continue;
                }
                return matches!(
                    kk,
                    S::KW_CLASS
                        | S::KW_OBJECT
                        | S::KW_INTERFACE
                        | S::KW_FUN
                        | S::KW_VAL
                        | S::KW_VAR
                        | S::KW_CONSTRUCTOR
                        | S::KW_INIT
                        | S::KW_TYPEALIAS
                ) || is_modifier_keyword(kk)
                    || is_soft_modifier_keyword(kk)
                    || kk == S::AT
                    || (kk == S::IDENTIFIER
                        && matches!(
                            p.text_at(j),
                            "public"
                | "inner"
                | "companion"
                | "data"
                | "sealed"
                | "annotation"
                | "open"
                | "abstract"
                | "override"
                | "operator"
                | "infix"
                | "lateinit"
                | "tailrec"
                | "suspend"
                | "expect"
                | "actual"
                | "crossinline"
                | "noinline"
                | "reified"
                | "value"
                | "vararg"
                | "const"
                        ));
            }
        }
        if is_soft_modifier_keyword(k) {
            // The soft modifier must also be followed by an actual
            // decl token to count as a modifier (not an identifier).
            let mut j = i + 1;
            loop {
                let kk = p.nth(j);
                if matches!(
                    kk,
                    S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
                ) {
                    j += 1;
                    continue;
                }
                return matches!(
                    kk,
                    S::KW_CLASS
                        | S::KW_OBJECT
                        | S::KW_INTERFACE
                        | S::KW_FUN
                        | S::KW_VAL
                        | S::KW_VAR
                ) || is_modifier_keyword(kk)
                    || is_soft_modifier_keyword(kk)
                    || kk == S::AT;
            }
        }
        return false;
    }
}

fn is_modifier_keyword(k: SyntaxKind) -> bool {
    matches!(
        k,
        S::KW_PRIVATE
            | S::KW_PROTECTED
            | S::KW_INTERNAL
            | S::KW_OVERRIDE
            | S::KW_OPEN
            | S::KW_ABSTRACT
            | S::KW_INLINE
            | S::KW_INFIX
            | S::KW_OPERATOR
            | S::KW_VARARG
            | S::KW_LATEINIT
            | S::KW_SUSPEND
            | S::KW_TAILREC
            | S::KW_CONST
            | S::KW_CROSSINLINE
            | S::KW_NOINLINE
            | S::KW_REIFIED
            | S::KW_ACTUAL
            | S::KW_EXPECT
            | S::KW_ANNOTATION
    )
}

fn is_soft_modifier_keyword(k: SyntaxKind) -> bool {
    matches!(k, S::KW_DATA | S::KW_SEALED | S::KW_COMPANION | S::KW_ENUM)
}

fn soft_modifier_followed_by_decl(p: &Parser<'_, '_>) -> bool {
    // Look ahead past trivia for a decl-introducer.
    let mut i = 1;
    loop {
        let k = p.nth(i);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            i += 1;
            continue;
        }
        return matches!(
            k,
            S::KW_CLASS
                | S::KW_OBJECT
                | S::KW_INTERFACE
                | S::KW_FUN
                | S::KW_VAL
                | S::KW_VAR
                | S::KW_CONSTRUCTOR
                | S::KW_INIT
                | S::KW_TYPEALIAS
        ) || is_modifier_keyword(k)
            || is_soft_modifier_keyword(k)
            || k == S::AT
            || (k == S::IDENTIFIER
                && matches!(
                    p.text_at(i),
                    "public"
                | "inner"
                | "companion"
                | "data"
                | "sealed"
                | "annotation"
                | "open"
                | "abstract"
                | "override"
                | "operator"
                | "infix"
                | "lateinit"
                | "tailrec"
                | "suspend"
                | "expect"
                | "actual"
                | "crossinline"
                | "noinline"
                | "reified"
                | "value"
                | "vararg"
                | "const"
                ));
    }
}

fn is_soft_keyword(k: SyntaxKind) -> bool {
    matches!(
        k,
        S::KW_DATA
            | S::KW_SEALED
            | S::KW_COMPANION
            | S::KW_ENUM
            | S::KW_BY
            | S::KW_GET
            | S::KW_SET
            | S::KW_FIELD
            | S::KW_PROPERTY
            | S::KW_PARAM
            | S::KW_RECEIVER
            | S::KW_FILE
            | S::KW_WHERE
            | S::KW_OUT
            | S::KW_REIFIED
            | S::KW_ACTUAL
            | S::KW_EXPECT
            | S::KW_OPEN
            | S::KW_OVERRIDE
            | S::KW_OPERATOR
            | S::KW_INFIX
            | S::KW_LATEINIT
            | S::KW_TAILREC
            | S::KW_SUSPEND
            | S::KW_CROSSINLINE
            | S::KW_NOINLINE
            | S::KW_VARARG
            | S::KW_CONST
            | S::KW_INNER
            | S::KW_PUBLIC
            | S::KW_ABSTRACT
            | S::KW_INLINE
            | S::KW_PRIVATE
            | S::KW_PROTECTED
            | S::KW_INTERNAL
            | S::KW_ANNOTATION
            | S::KW_VALUE
            | S::KW_INIT
    )
}

fn parse_annotation_entry(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // @
    if matches!(
        p.current(),
        S::KW_FILE
            | S::KW_FIELD
            | S::KW_GET
            | S::KW_SET
            | S::KW_PARAM
            | S::KW_PROPERTY
            | S::KW_RECEIVER
    ) {
        let tgt = p.start();
        p.bump();
        tgt.complete(p, S::ANNOTATION_USE_SITE_TARGET);
        if p.at(S::COLON) {
            p.bump();
        }
    }
    if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
        // kotlinc PSI wraps an annotation's type in a CONSTRUCTOR_CALLEE.
        let callee = p.start();
        let tref = p.start();
        let ut = p.start();
        parse_user_type_segment(p);
        ut.complete(p, S::USER_TYPE);
        tref.complete(p, S::TYPE_REFERENCE);
        callee.complete(p, S::CONSTRUCTOR_CALLEE);
    }
    if p.at(S::LPAR) {
        parse_value_argument_list(p);
    }
    m.complete(p, S::ANNOTATION_ENTRY);
}

// ─── class / object / interface ──────────────────────────────────────────────

fn parse_class_or_interface_body(p: &mut Parser<'_, '_>) {
    // The class/interface keyword is the current token.
    p.bump(); // class / interface
    skip_trivia(p);
    if p.at(S::IDENTIFIER) {
        p.bump();
    }
    skip_trivia_if_class_continues(p);
    if p.at(S::LT) {
        parse_type_parameter_list(p);
        skip_trivia_if_class_continues(p);
    }
    // Primary constructor — can be either the bare `(...)` form or
    // the explicit `[modifiers] constructor (...)` form (annotations
    // like `@Inject constructor` or visibility like `private
    // constructor` always require the explicit `constructor` token).
    if p.at(S::LPAR) {
        let pc = p.start();
        parse_value_parameter_list(p);
        pc.complete(p, S::PRIMARY_CONSTRUCTOR);
        skip_trivia_if_class_continues(p);
    } else if looks_like_primary_constructor_with_modifiers(p) {
        // Push any leading trivia OUT of the PRIMARY_CONSTRUCTOR
        // composite — kotlinc PSI keeps that WS as a sibling of
        // PRIMARY_CONSTRUCTOR inside the CLASS.
        while matches!(
            p.current(),
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            p.bump();
        }
        let pc = p.start();
        parse_modifier_list_opt(p);
        skip_trivia(p);
        if p.at(S::KW_CONSTRUCTOR) {
            p.bump();
            skip_trivia(p);
        }
        if p.at(S::LPAR) {
            parse_value_parameter_list(p);
        }
        pc.complete(p, S::PRIMARY_CONSTRUCTOR);
        skip_trivia_if_class_continues(p);
    }
    if p.at(S::COLON) {
        // kotlinc PSI: COLON + WHITE_SPACE sit as direct children of
        // CLASS; only the SUPER_TYPE_ENTRY items go inside SUPER_TYPE_LIST.
        p.bump(); // :
        skip_trivia(p);
        let stl = p.start();
        loop {
            parse_super_type_entry(p);
            // Only consume trivia inside SUPER_TYPE_LIST when a comma
            // (another entry) follows. Trailing trivia belongs to the
            // outer CLASS composite.
            if next_non_trivia(p, 0) != S::COMMA {
                break;
            }
            skip_trivia(p);
            p.bump();
            skip_trivia(p);
        }
        stl.complete(p, S::SUPER_TYPE_LIST);
        skip_trivia_if_class_continues(p);
    }
    if p.at(S::KW_WHERE) {
        parse_type_constraint_list(p);
        skip_trivia_if_class_continues(p);
    }
    if p.at(S::LBRACE) {
        parse_class_body(p);
    }
}

/// Skip trivia (WS, line/block comments) only if the next real token
/// continues the class declaration. Stops at KDoc (which means the
/// next declaration is starting), at file-level decl keywords, etc.
fn skip_trivia_if_class_continues(p: &mut Parser<'_, '_>) {
    let mut i = 0;
    loop {
        let k = p.nth(i);
        if matches!(k, S::WHITE_SPACE | S::LINE_COMMENT | S::BLOCK_COMMENT) {
            i += 1;
            continue;
        }
        let class_continues = matches!(
            k,
            S::COLON | S::KW_WHERE | S::LBRACE | S::LT | S::LPAR | S::IDENTIFIER
        );
        if class_continues {
            for _ in 0..i {
                p.bump();
            }
        }
        return;
    }
}

fn parse_super_type_entry(p: &mut Parser<'_, '_>) {
    let m = p.start();
    let tref = parse_type_ref(p);
    let next = next_non_trivia(p, 0);
    let by_at_next = next == S::KW_BY
        || (next == S::IDENTIFIER && next_non_trivia_text_eq(p, 0, "by"));
    if next == S::LPAR {
        // SUPER_TYPE_CALL_ENTRY: wrap the type-ref in CONSTRUCTOR_CALLEE.
        let callee = tref.precede(p);
        callee.complete(p, S::CONSTRUCTOR_CALLEE);
        skip_trivia(p);
        parse_value_argument_list(p);
        m.complete(p, S::SUPER_TYPE_CALL_ENTRY);
    } else if by_at_next {
        skip_trivia(p);
        if p.at(S::IDENTIFIER) && p.current_text() == "by" {
            p.bump_as(S::KW_BY);
        } else {
            p.bump();
        }
        skip_trivia(p);
        parse_expression(p);
        m.complete(p, S::DELEGATED_SUPER_TYPE_ENTRY);
    } else {
        m.complete(p, S::SUPER_TYPE_ENTRY);
    }
}

/// Peek past trivia at `offset` and return the raw source text of the
/// first non-trivia token. Returns `""` at EOF.
fn next_non_trivia_text<'src>(p: &Parser<'src, '_>, mut offset: usize) -> &'src str {
    loop {
        let k = p.nth(offset);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT
        ) {
            offset += 1;
            continue;
        }
        return p.text_at(offset);
    }
}

/// Peek past trivia at `offset` and check whether the next non-trivia
/// token is an IDENT whose text matches `expected`. Cheap, used by
/// soft-keyword detectors.
fn next_non_trivia_text_eq(p: &Parser<'_, '_>, mut offset: usize, expected: &str) -> bool {
    loop {
        let k = p.nth(offset);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT
        ) {
            offset += 1;
            continue;
        }
        return k == S::IDENTIFIER && p.text_at(offset) == expected;
    }
}

fn parse_type_constraint_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // where
    skip_trivia(p);
    loop {
        parse_type_constraint(p);
        skip_trivia(p);
        if !p.at(S::COMMA) {
            break;
        }
        p.bump();
        skip_trivia(p);
    }
    m.complete(p, S::TYPE_CONSTRAINT_LIST);
}

fn parse_type_constraint(p: &mut Parser<'_, '_>) {
    let m = p.start();
    if p.at(S::IDENTIFIER) {
        let r = p.start();
        p.bump();
        r.complete(p, S::REFERENCE_EXPRESSION);
    }
    skip_trivia(p);
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
    }
    m.complete(p, S::TYPE_CONSTRAINT);
}

fn parse_class_body(p: &mut Parser<'_, '_>) {
    parse_class_body_impl(p, /* enum_entries */ false);
}

fn parse_class_body_impl(p: &mut Parser<'_, '_>, enum_entries: bool) {
    let m = p.start();
    p.bump(); // {
              // Only WS sits at CLASS_BODY level; KDoc / line / block comments
              // belong to the upcoming member.
    while p.at(S::WHITE_SPACE) {
        p.bump();
    }
    // Enum class body starts with a comma-separated list of enum
    // entries, optionally terminated by `;`, then plain class members.
    let mut entries_done = !enum_entries;
    while !p.at(S::RBRACE) && !p.at(S::EOF) {
        if !entries_done {
            // Try to parse an enum entry.
            if looks_like_enum_entry_in_enum_body(p) {
                parse_enum_entry_at_top(p);
                // Between entries: comma + WS, or end-of-entries (`;`
                // or `}` or another non-entry member).
                while p.at(S::WHITE_SPACE) {
                    p.bump();
                }
                if p.at(S::COMMA) {
                    p.bump();
                    while p.at(S::WHITE_SPACE) {
                        p.bump();
                    }
                    continue;
                }
                if p.at(S::SEMICOLON) {
                    p.bump();
                    entries_done = true;
                    while p.at(S::WHITE_SPACE) {
                        p.bump();
                    }
                    continue;
                }
                entries_done = true;
                continue;
            }
            // No entries (e.g. enum body containing only methods).
            entries_done = true;
        }
        parse_class_member(p);
        // Between members: only WS at CLASS_BODY level.
        while p.at(S::WHITE_SPACE) {
            p.bump();
        }
    }
    if p.at(S::RBRACE) {
        p.bump();
    }
    m.complete(p, S::CLASS_BODY);
}

/// Wraps `parse_enum_entry_body` in an ENUM_ENTRY composite.
fn parse_enum_entry_at_top(p: &mut Parser<'_, '_>) {
    let m = p.start();
    parse_modifier_list_opt(p);
    skip_trivia(p);
    parse_enum_entry_body(p);
    m.complete(p, S::ENUM_ENTRY);
}

/// Heuristic: inside an enum class body, an IDENT followed by `,`,
/// `;`, `(`, `{`, or `}` (across trivia) starts an enum entry. An
/// IDENT followed by `:`/`<`/`=`/`fun`/`val`/etc. is a regular
/// class member instead.
fn looks_like_enum_entry_in_enum_body(p: &Parser<'_, '_>) -> bool {
    // Walk modifiers/annotations.
    let mut i = 0;
    loop {
        let k = p.nth(i);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            i += 1;
            continue;
        }
        if k == S::AT {
            // Skip annotation: `@Name` optionally `(args)`.
            i += 1;
            while matches!(
                p.nth(i),
                S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
            ) {
                i += 1;
            }
            if p.nth(i) == S::IDENTIFIER || is_soft_keyword(p.nth(i)) {
                i += 1;
            }
            while matches!(
                p.nth(i),
                S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
            ) {
                i += 1;
            }
            if p.nth(i) == S::LPAR {
                let mut depth = 1;
                i += 1;
                while depth > 0 && i < 64 {
                    match p.nth(i) {
                        S::LPAR => depth += 1,
                        S::RPAR => depth -= 1,
                        S::EOF => return false,
                        _ => {}
                    }
                    i += 1;
                }
            }
            continue;
        }
        if is_modifier_keyword(k) {
            i += 1;
            continue;
        }
        // Now we expect either IDENT (entry name) or a real decl
        // keyword (`fun`, `val`, etc.). If it's a decl keyword, this
        // is NOT an enum entry.
        if matches!(
            k,
            S::KW_FUN
                | S::KW_VAL
                | S::KW_VAR
                | S::KW_CLASS
                | S::KW_OBJECT
                | S::KW_INTERFACE
                | S::KW_INIT
                | S::KW_CONSTRUCTOR
                | S::KW_TYPEALIAS
                | S::KW_COMPANION
        ) {
            return false;
        }
        if k != S::IDENTIFIER && !is_soft_keyword(k) {
            return false;
        }
        // Look past the ident for a follower that identifies an enum
        // entry shape.
        let mut j = i + 1;
        while matches!(
            p.nth(j),
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            j += 1;
        }
        return matches!(
            p.nth(j),
            S::COMMA | S::SEMICOLON | S::LPAR | S::LBRACE | S::RBRACE
        );
    }
}

/// Walk the parser's recently-emitted token events backwards to find
/// out whether the just-finished modifier list contained `enum`.
/// Stops at the first non-modifier-shaped token (so we don't bleed
/// past the current decl's modifier list into earlier siblings).
fn was_enum_modifier(p: &Parser<'_, '_>) -> bool {
    for k in p.recent_token_kinds() {
        match k {
            S::KW_ENUM => return true,
            S::WHITE_SPACE
            | S::NEWLINE
            | S::LINE_COMMENT
            | S::BLOCK_COMMENT
            | S::KDOC
            | S::AT
            | S::IDENTIFIER
            | S::LPAR
            | S::RPAR
            | S::COMMA
            | S::DOT => {}
            k if is_modifier_keyword(k) || is_soft_modifier_keyword(k) => {}
            k if matches!(
                k,
                S::KW_PUBLIC
                    | S::KW_PROTECTED
                    | S::KW_PRIVATE
                    | S::KW_INTERNAL
                    | S::KW_OPEN
                    | S::KW_ABSTRACT
                    | S::KW_FINALLY
                    | S::KW_OVERRIDE
                    | S::KW_INLINE
                    | S::KW_INFIX
                    | S::KW_OPERATOR
                    | S::KW_VARARG
                    | S::KW_LATEINIT
                    | S::KW_SUSPEND
                    | S::KW_TAILREC
                    | S::KW_CONST
                    | S::KW_CROSSINLINE
                    | S::KW_NOINLINE
                    | S::KW_REIFIED
                    | S::KW_ACTUAL
                    | S::KW_EXPECT
                    | S::KW_ANNOTATION
                    | S::KW_DATA
                    | S::KW_SEALED
                    | S::KW_COMPANION
                    | S::KW_INNER
            ) =>
            {
                // still inside the modifier list
            }
            _ => return false,
        }
    }
    false
}

/// Same as `parse_class_or_interface_body` but tags the class body as
/// an enum-class body so the body parser recognizes entries.
fn parse_enum_class_body(p: &mut Parser<'_, '_>) {
    // Reuses the standard class parser but flips the body-mode flag.
    // The structure is otherwise identical (modifiers/typeparams/
    // primary-ctor/super-types/where/body) — the only difference is
    // how the body iterates entries.
    p.bump(); // class
    skip_trivia(p);
    if p.at(S::IDENTIFIER) {
        p.bump();
    }
    skip_trivia_if_class_continues(p);
    if p.at(S::LT) {
        parse_type_parameter_list(p);
        skip_trivia_if_class_continues(p);
    }
    if p.at(S::LPAR) {
        let pc = p.start();
        parse_value_parameter_list(p);
        pc.complete(p, S::PRIMARY_CONSTRUCTOR);
        skip_trivia_if_class_continues(p);
    }
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        let stl = p.start();
        loop {
            parse_super_type_entry(p);
            if next_non_trivia(p, 0) != S::COMMA {
                break;
            }
            skip_trivia(p);
            p.bump();
            skip_trivia(p);
        }
        stl.complete(p, S::SUPER_TYPE_LIST);
        skip_trivia_if_class_continues(p);
    }
    if p.at(S::KW_WHERE) {
        parse_type_constraint_list(p);
        skip_trivia_if_class_continues(p);
    }
    if p.at(S::LBRACE) {
        parse_class_body_impl(p, /* enum_entries */ true);
    }
}

fn parse_class_member(p: &mut Parser<'_, '_>) {
    if p.at(S::SEMICOLON) || p.at(S::COMMA) {
        p.bump();
        return;
    }
    let m = p.start();
    // Leading KDoc, line/block comments, and adjacent WS belong to
    // this member, not to the surrounding CLASS_BODY. Absorb them.
    while matches!(p.current(), S::KDOC | S::LINE_COMMENT | S::BLOCK_COMMENT) {
        p.bump();
        if p.at(S::WHITE_SPACE) {
            p.bump();
        }
    }
    parse_modifier_list_opt(p);
    skip_trivia(p);
    let kind = match p.current() {
        S::KW_FUN => {
            parse_fun_body(p);
            S::FUN
        }
        S::KW_VAL | S::KW_VAR => {
            parse_property_body(p);
            S::PROPERTY
        }
        S::KW_CLASS | S::KW_INTERFACE => {
            parse_class_or_interface_body(p);
            S::CLASS
        }
        S::KW_OBJECT => {
            parse_object_decl_body(p);
            S::OBJECT_DECLARATION
        }
        S::KW_INIT => {
            parse_anonymous_initializer_body(p);
            S::ANONYMOUS_INITIALIZER
        }
        S::KW_CONSTRUCTOR => {
            parse_secondary_constructor_body(p);
            S::SECONDARY_CONSTRUCTOR
        }
        S::KW_TYPEALIAS => {
            parse_typealias_body(p);
            S::TYPEALIAS
        }
        S::KW_ENUM => {
            p.bump();
            skip_trivia(p);
            if p.at(S::KW_CLASS) {
                parse_class_or_interface_body(p);
                S::CLASS
            } else {
                m.complete(p, S::ERROR_ELEMENT);
                return;
            }
        }
        S::IDENTIFIER if looks_like_enum_entry(p) => {
            parse_enum_entry_body(p);
            S::ENUM_ENTRY
        }
        _ => {
            // Best-effort: consume one token so we make progress.
            if !p.at(S::EOF) {
                let err = p.start();
                p.bump();
                err.complete(p, S::ERROR_ELEMENT);
            }
            m.complete(p, S::ERROR_ELEMENT);
            return;
        }
    };
    m.complete(p, kind);
}

fn looks_like_enum_entry(_p: &Parser<'_, '_>) -> bool {
    // Conservative — enum entries are rare and tricky to detect
    // without context. Return false so plain identifiers don't get
    // mis-classified.
    false
}

/// Does the upcoming token sequence look like an explicit primary
/// constructor with modifiers? Matches `@Annotation constructor(...)`,
/// `private constructor(...)`, `@Inject @Other constructor(...)`, etc.
fn looks_like_primary_constructor_with_modifiers(p: &Parser<'_, '_>) -> bool {
    let mut i = 0;
    // Must have at least one modifier or annotation, then `constructor`,
    // then `(`.
    let mut saw_mod = false;
    loop {
        let k = p.nth(i);
        match k {
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC => {}
            S::AT => {
                saw_mod = true;
                // Skip `@Name(args)` form.
                i += 1;
                // Skip `@Name`
                let mut j = i;
                while matches!(
                    p.nth(j),
                    S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
                ) {
                    j += 1;
                }
                if !(p.nth(j) == S::IDENTIFIER || is_soft_keyword(p.nth(j))) {
                    return false;
                }
                j += 1;
                // Skip optional `(...)` args
                while matches!(
                    p.nth(j),
                    S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
                ) {
                    j += 1;
                }
                if p.nth(j) == S::LPAR {
                    let mut depth_par = 1i32;
                    j += 1;
                    while depth_par > 0 && j < 256 {
                        match p.nth(j) {
                            S::LPAR => depth_par += 1,
                            S::RPAR => depth_par -= 1,
                            S::EOF => return false,
                            _ => {}
                        }
                        j += 1;
                    }
                }
                i = j;
                continue;
            }
            k if is_modifier_keyword(k) => {
                saw_mod = true;
            }
            S::IDENTIFIER
                if matches!(
                    p.text_at(i),
                    "public"
                | "inner"
                | "companion"
                | "data"
                | "sealed"
                | "annotation"
                | "open"
                | "abstract"
                | "override"
                | "operator"
                | "infix"
                | "lateinit"
                | "tailrec"
                | "suspend"
                | "expect"
                | "actual"
                | "crossinline"
                | "noinline"
                | "reified"
                | "value"
                | "vararg"
                | "const"
                ) =>
            {
                saw_mod = true;
            }
            S::KW_CONSTRUCTOR if saw_mod => {
                // Found `constructor` after modifiers; primary ctor.
                let mut j = i + 1;
                while matches!(
                    p.nth(j),
                    S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
                ) {
                    j += 1;
                }
                return p.nth(j) == S::LPAR;
            }
            _ => return false,
        }
        i += 1;
        if i > 256 {
            return false;
        }
    }
}

fn parse_object_decl_body(p: &mut Parser<'_, '_>) {
    p.bump(); // object
    skip_trivia(p);
    if p.at(S::IDENTIFIER) {
        p.bump();
    }
    skip_trivia(p);
    if p.at(S::COLON) {
        // COLON sits OUTSIDE SUPER_TYPE_LIST (matches kotlinc PSI).
        p.bump();
        skip_trivia(p);
        let stl = p.start();
        loop {
            parse_super_type_entry(p);
            // Only consume trivia inside SUPER_TYPE_LIST when a comma
            // (another entry) follows. Trailing trivia belongs to the
            // outer OBJECT composite, between SUPER_TYPE_LIST and the
            // following `{` of CLASS_BODY.
            if next_non_trivia(p, 0) != S::COMMA {
                break;
            }
            skip_trivia(p);
            p.bump();
            skip_trivia(p);
        }
        stl.complete(p, S::SUPER_TYPE_LIST);
        skip_trivia(p);
    }
    if p.at(S::LBRACE) {
        parse_class_body(p);
    }
}

fn parse_enum_entry_body(p: &mut Parser<'_, '_>) {
    if p.at(S::IDENTIFIER) {
        p.bump();
    }
    skip_trivia(p);
    if p.at(S::LPAR) {
        parse_value_argument_list(p);
        skip_trivia(p);
    }
    if p.at(S::LBRACE) {
        parse_class_body(p);
    }
}

// ─── functions ───────────────────────────────────────────────────────────────

fn parse_fun_body(p: &mut Parser<'_, '_>) {
    p.bump(); // fun
    skip_trivia(p);
    if p.at(S::LT) {
        parse_type_parameter_list(p);
        skip_trivia(p);
    }
    // Possibly a receiver type — look for `<type>.<name>(`. We
    // approximate by trying receiver_then_dot_then_name and falling
    // back to plain-name on failure.
    if has_receiver_prefix(p) {
        parse_receiver_then_name(p);
    } else if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
        p.bump();
    }
    skip_trivia(p);
    if p.at(S::LPAR) {
        parse_value_parameter_list(p);
        if matches!(
            next_non_trivia(p, 0),
            S::COLON | S::KW_WHERE | S::EQ | S::LBRACE
        ) {
            skip_trivia(p);
        }
    }
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
        if matches!(next_non_trivia(p, 0), S::KW_WHERE | S::EQ | S::LBRACE) {
            skip_trivia(p);
        }
    }
    if p.at(S::KW_WHERE) {
        parse_type_constraint_list(p);
        if matches!(next_non_trivia(p, 0), S::EQ | S::LBRACE) {
            skip_trivia(p);
        }
    }
    if p.at(S::EQ) {
        p.bump();
        skip_trivia(p);
        parse_expression(p);
    } else if p.at(S::LBRACE) {
        parse_block(p);
    }
}

fn has_receiver_prefix(p: &Parser<'_, '_>) -> bool {
    // Walk forward, balancing <> and (), looking for an unbalanced
    // `.` before a `(` / `=` / `:`. Receiver functions/properties
    // have form `Foo.name(...)` or `Foo.name: Type = ...`; the `.`
    // must be at depth 0 and BEFORE any of those terminators.
    let mut depth_lt = 0i32;
    let mut depth_par = 0i32;
    let mut i = 0;
    loop {
        let k = p.nth(i);
        match k {
            S::LPAR => {
                if depth_lt == 0 && depth_par == 0 {
                    return false;
                }
                depth_par += 1;
            }
            S::RPAR => depth_par -= 1,
            S::LT => depth_lt += 1,
            S::GT => depth_lt -= 1,
            S::DOT if depth_lt == 0 && depth_par == 0 => return true,
            S::EQ | S::COLON if depth_lt == 0 && depth_par == 0 => return false,
            S::EOF | S::LBRACE | S::SEMICOLON => return false,
            S::WHITE_SPACE if depth_lt == 0 && depth_par == 0 && p.text_at(i).contains('\n') => {
                return false;
            }
            _ => {}
        }
        i += 1;
        if i > 64 {
            return false;
        }
    }
}

fn parse_receiver_then_name(p: &mut Parser<'_, '_>) {
    // Receiver TYPE_REFERENCE = USER_TYPE (with possible generics)
    // that ends BEFORE the final `. IDENT (` which introduces the
    // function name. The final DOT and IDENT live as direct children
    // of FUN (not wrapped in a TYPE_REFERENCE) — kotlinc PSI shape.
    let tref = p.start();
    let ut = p.start();
    parse_user_type_segment(p);
    // Extend the user-type chain only when the next DOT is followed
    // by another segment that ALSO has a DOT after it. The "last
    // dotted identifier before `(` / `<` " is the function name.
    while p.at(S::DOT) && more_user_type_segments_follow(p) {
        p.bump(); // .
        skip_ws(p);
        parse_user_type_segment(p);
    }
    ut.complete(p, S::USER_TYPE);
    tref.complete(p, S::TYPE_REFERENCE);
    // The final DOT + identifier — direct children of FUN.
    if p.at(S::DOT) {
        p.bump();
    }
    skip_ws(p);
    if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
        p.bump();
    }
}

/// `true` if the upcoming `. IDENT …` is followed by ANOTHER `.`
/// (i.e., it's a continuation of the receiver chain, not the final
/// function-name segment).
fn more_user_type_segments_follow(p: &Parser<'_, '_>) -> bool {
    // Position 0 is the `.` we're examining. Walk past it + WS, then
    // expect an identifier (or soft keyword). After that, possibly
    // generic args, then look for another `.` to confirm chain.
    let mut i = 1usize;
    while matches!(p.nth(i), S::WHITE_SPACE | S::NEWLINE) {
        i += 1;
    }
    if !(p.nth(i) == S::IDENTIFIER || is_soft_keyword(p.nth(i))) {
        return false;
    }
    i += 1;
    // Skip generic args `<...>` if present (matched-balanced).
    if p.nth(i) == S::LT {
        let mut depth = 1i32;
        i += 1;
        while i < 64 && depth > 0 {
            match p.nth(i) {
                S::LT => depth += 1,
                S::GT => depth -= 1,
                S::EOF => return false,
                _ => {}
            }
            i += 1;
        }
    }
    while matches!(p.nth(i), S::WHITE_SPACE) {
        i += 1;
    }
    p.nth(i) == S::DOT
}

fn parse_anonymous_initializer_body(p: &mut Parser<'_, '_>) {
    p.bump(); // init
    skip_trivia(p);
    if p.at(S::LBRACE) {
        parse_block(p);
    }
}

fn parse_secondary_constructor_body(p: &mut Parser<'_, '_>) {
    p.bump(); // constructor
    skip_trivia(p);
    if p.at(S::LPAR) {
        parse_value_parameter_list(p);
        skip_trivia(p);
    }
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        // `: this(...)` or `: super(...)` becomes
        //   CONSTRUCTOR_DELEGATION_CALL {
        //     CONSTRUCTOR_DELEGATION_REFERENCE { this|super },
        //     VALUE_ARGUMENT_LIST { ... }
        //   }
        // `this` lexes as plain IDENTIFIER; reclassify it as KW_THIS
        // so the emitted YAML matches kotlinc's "this" token.
        let is_this_ident = p.at(S::IDENTIFIER) && p.current_text() == "this";
        if matches!(p.current(), S::KW_SUPER) || is_this_ident {
            let delegation = p.start();
            let reference = p.start();
            if is_this_ident {
                p.bump_as(S::KW_THIS);
            } else {
                p.bump();
            }
            reference.complete(p, S::CONSTRUCTOR_DELEGATION_REFERENCE);
            skip_trivia(p);
            if p.at(S::LPAR) {
                parse_value_argument_list(p);
            }
            delegation.complete(p, S::CONSTRUCTOR_DELEGATION_CALL);
        }
    }
    // Look ahead past pure inline trivia for an opening `{` — but do
    // NOT consume any newline-bearing WS or KDoc, since those belong
    // to the next member in the enclosing CLASS_BODY.
    skip_ws(p);
    if p.at(S::LBRACE) {
        parse_block(p);
    }
}

fn parse_property_body(p: &mut Parser<'_, '_>) {
    p.bump(); // val/var
    skip_trivia(p);
    if p.at(S::LT) {
        parse_type_parameter_list(p);
        skip_trivia(p);
    }
    // Destructuring declaration: `val (a, b) = pair`. The DESTRUCTURING
    // wrapper takes the spot of the property name.
    if p.at(S::LPAR) {
        let d = p.start();
        p.bump();
        loop {
            skip_trivia(p);
            if !(p.at(S::IDENTIFIER) || is_soft_keyword(p.current())) {
                break;
            }
            let e = p.start();
            p.bump();
            // Optional `: Type`.
            if next_non_trivia(p, 0) == S::COLON {
                skip_trivia(p);
                p.bump();
                skip_trivia(p);
                parse_type_ref(p);
            }
            e.complete(p, S::DESTRUCTURING_DECLARATION_ENTRY);
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
            p.bump();
        }
        if p.at(S::RPAR) {
            p.bump();
        }
        d.complete(p, S::DESTRUCTURING_DECLARATION);
    } else if has_receiver_prefix(p) {
        parse_receiver_then_name(p);
    } else if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
        p.bump();
    }
    skip_trivia(p);
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
        // Only continue past trailing trivia if the property has more
        // syntax to consume (`=`, `by`, or an accessor block). Trivia
        // that ends a property belongs to the enclosing CLASS_BODY.
        if matches!(
            next_non_trivia(p, 0),
            S::EQ | S::KW_BY | S::KW_GET | S::KW_SET
        ) || next_non_trivia_text_eq(p, 0, "by")
            || next_non_trivia_text_eq(p, 0, "get")
            || next_non_trivia_text_eq(p, 0, "set")
        {
            skip_trivia(p);
        }
    }
    if p.at(S::EQ) {
        p.bump();
        skip_trivia(p);
        parse_expression(p);
        // Trailing `// comment` on the SAME LINE as the property
        // value belongs INSIDE the PROPERTY. Trivia containing a
        // newline belongs to the next composite.
        consume_same_line_trailing_comment(p);
        if next_non_trivia_property_continues(p) {
            skip_trivia(p);
        }
    }
    if p.at(S::KW_BY) || (p.at(S::IDENTIFIER) && p.current_text() == "by") {
        if p.at(S::IDENTIFIER) {
            p.bump_as(S::KW_BY);
        } else {
            p.bump();
        }
        skip_trivia(p);
        parse_expression(p);
        if next_non_trivia_property_continues(p) {
            skip_trivia(p);
        }
    }
    while looks_like_accessor(p) {
        skip_trivia(p);
        parse_property_accessor(p);
        if !next_non_trivia_property_continues(p) {
            break;
        }
        skip_trivia(p);
    }
}

fn next_non_trivia_property_continues(p: &Parser<'_, '_>) -> bool {
    let mut i = 0;
    loop {
        let k = p.nth(i);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            i += 1;
            continue;
        }
        return matches!(k, S::KW_BY | S::KW_GET | S::KW_SET)
            || (k == S::IDENTIFIER && matches!(p.text_at(i), "get" | "set" | "by"));
    }
}

/// Eat at most one `WHITE_SPACE` (no newline) followed by a
/// `LINE_COMMENT` / `BLOCK_COMMENT`. Used to absorb a trailing
/// same-line `// comment` into the declaration that just ended.
fn consume_same_line_trailing_comment(p: &mut Parser<'_, '_>) {
    let same_line_ws = p.at(S::WHITE_SPACE) && !p.current_text().contains('\n');
    let ws_then_comment = same_line_ws && matches!(p.nth(1), S::LINE_COMMENT | S::BLOCK_COMMENT);
    if ws_then_comment {
        p.bump(); // WS
    }
    if matches!(p.current(), S::LINE_COMMENT | S::BLOCK_COMMENT) {
        p.bump();
    }
}

fn looks_like_accessor(p: &Parser<'_, '_>) -> bool {
    let mut i = 0;
    loop {
        let k = p.nth(i);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
        ) {
            i += 1;
            continue;
        }
        return matches!(k, S::KW_GET | S::KW_SET)
            || (k == S::IDENTIFIER && matches!(p.text_at(i), "get" | "set"));
    }
}

fn parse_property_accessor(p: &mut Parser<'_, '_>) {
    let m = p.start();
    // `get`/`set` lex as IDENT; reclassify so the emitted token kind
    // matches kotlinc's `get`/`set` keyword leaves.
    match p.current_text() {
        "get" if p.at(S::IDENTIFIER) => p.bump_as(S::KW_GET),
        "set" if p.at(S::IDENTIFIER) => p.bump_as(S::KW_SET),
        _ => p.bump(),
    }
    skip_trivia(p);
    if p.at(S::LPAR) {
        parse_value_parameter_list(p);
        skip_trivia(p);
    }
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
        skip_trivia(p);
    }
    if p.at(S::EQ) {
        p.bump();
        skip_trivia(p);
        parse_expression(p);
    } else if p.at(S::LBRACE) {
        parse_block(p);
    }
    m.complete(p, S::PROPERTY_ACCESSOR);
}

fn parse_typealias_body(p: &mut Parser<'_, '_>) {
    p.bump(); // typealias
    parse_typealias_body_after_kw(p);
}

/// Inner half of [`parse_typealias_body`]: the `typealias` keyword has
/// already been consumed (possibly via `bump_as` from a soft-keyword
/// IDENT). Parses the name, optional type parameters, and the body.
fn parse_typealias_body_after_kw(p: &mut Parser<'_, '_>) {
    skip_trivia(p);
    if p.at(S::IDENTIFIER) {
        p.bump();
    }
    skip_trivia(p);
    if p.at(S::LT) {
        parse_type_parameter_list(p);
        skip_trivia(p);
    }
    if p.at(S::EQ) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
    }
}

// ─── types ───────────────────────────────────────────────────────────────────

fn parse_type_parameter_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // <
    skip_trivia(p);
    loop {
        parse_type_parameter(p);
        skip_trivia(p);
        if !p.at(S::COMMA) {
            break;
        }
        p.bump();
        skip_trivia(p);
    }
    if p.at(S::GT) {
        p.bump();
    }
    m.complete(p, S::TYPE_PARAMETER_LIST);
}

fn parse_type_parameter(p: &mut Parser<'_, '_>) {
    let m = p.start();
    // Type-parameter modifiers: `in`/`out` (variance), `reified`. They
    // sit in a MODIFIER_LIST inside the TYPE_PARAMETER. Multiple
    // modifiers may appear (e.g. `reified out T`). Trailing trivia
    // after the last modifier belongs at the TYPE_PARAMETER level.
    let mut had_modifier = false;
    let ml = p.start();
    loop {
        if had_modifier {
            // Only consume trivia between consecutive modifiers.
            let next_text = next_non_trivia_text(p, 0);
            let next_is_modifier_like = matches!(next_text, "out" | "in" | "reified");
            if next_is_modifier_like {
                skip_trivia(p);
            } else {
                break;
            }
        }
        let kind = match p.current_text() {
            "out" if p.at(S::IDENTIFIER) => Some(S::KW_OUT),
            "in" if p.at(S::IDENTIFIER) => Some(S::KW_IN),
            "reified" if p.at(S::IDENTIFIER) => Some(S::KW_REIFIED),
            _ => None,
        };
        let Some(kind) = kind else { break };
        p.bump_as(kind);
        had_modifier = true;
    }
    if had_modifier {
        ml.complete(p, S::MODIFIER_LIST);
    } else {
        ml.abandon(p);
    }
    skip_trivia(p);
    if p.at(S::IDENTIFIER) {
        p.bump();
    }
    skip_trivia(p);
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
    }
    m.complete(p, S::TYPE_PARAMETER);
}

fn parse_type_ref(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    parse_modifier_list_opt(p);
    skip_trivia(p);
    // Function type with receiver: `T.(A) -> R` (and the generic form
    // `Foo<X>.(A) -> R`). Look ahead through the leading USER_TYPE
    // segments for a `.(` before the `->` to disambiguate from a
    // dotted regular type.
    if looks_like_receiver_function_type(p) {
        parse_receiver_function_type(p);
        return m.complete(p, S::TYPE_REFERENCE);
    }
    // Function type: `(A, B) -> C`.
    if p.at(S::LPAR) && looks_like_function_type(p) {
        parse_function_type_after_modifiers(p);
        return m.complete(p, S::TYPE_REFERENCE);
    }
    // User type, optionally wrapped in NULLABLE_TYPE for `?`.
    // `A.B.C` is left-associatively nested: USER_TYPE wraps USER_TYPE
    // wraps the leading segment — produced by parse_user_type_chain.
    let mut user_cm = parse_user_type_chain(p);
    while next_non_trivia(p, 0) == S::QUEST {
        skip_ws(p);
        // Wrap the prior USER_TYPE / NULLABLE_TYPE in another
        // NULLABLE_TYPE composite — kotlinc PSI nests each `?` as a
        // wrapper around the previous type. Use `precede` to retain
        // the inner structure.
        let null_m = user_cm.precede(p);
        p.bump(); // ?
        user_cm = null_m.complete(p, S::NULLABLE_TYPE);
    }
    m.complete(p, S::TYPE_REFERENCE)
}

/// Look ahead for `Type.(args) -> result` — a function type with a
/// receiver. The receiver is a USER_TYPE that ends BEFORE the final
/// `.(` separator.
fn looks_like_receiver_function_type(p: &Parser<'_, '_>) -> bool {
    // Walk forward balancing `<>`s. Find the FIRST top-level `.(` that
    // is followed eventually by `->`.
    let mut i = 0usize;
    let mut depth_lt = 0i32;
    loop {
        let k = p.nth(i);
        match k {
            S::EOF | S::LBRACE | S::SEMICOLON | S::EQ => return false,
            S::LT => depth_lt += 1,
            S::GT => depth_lt -= 1,
            S::DOT if depth_lt == 0 && p.nth(i + 1) == S::LPAR => {
                // Confirm `->` follows the closing `)`.
                let mut j = i + 2;
                let mut depth = 1i32;
                while depth > 0 && j < 256 {
                    match p.nth(j) {
                        S::LPAR => depth += 1,
                        S::RPAR => depth -= 1,
                        S::EOF => return false,
                        _ => {}
                    }
                    j += 1;
                }
                while matches!(
                    p.nth(j),
                    S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT
                ) {
                    j += 1;
                }
                return p.nth(j) == S::ARROW;
            }
            _ => {}
        }
        i += 1;
        if i > 256 {
            return false;
        }
    }
}

fn parse_receiver_function_type(p: &mut Parser<'_, '_>) {
    // FUNCTION_TYPE wraps FUNCTION_TYPE_RECEIVER, the `.`, the params,
    // the `->`, and the return type.
    let ft = p.start();
    let recv = p.start();
    let first = p.start();
    parse_user_type_segment(p);
    let mut cm = first.complete(p, S::USER_TYPE);
    loop {
        if next_non_trivia(p, 0) != S::DOT {
            break;
        }
        let mut probe = 1usize;
        while matches!(p.nth(probe), S::WHITE_SPACE | S::NEWLINE) {
            probe += 1;
        }
        if p.nth(probe) == S::LPAR {
            break;
        }
        let outer = cm.precede(p);
        skip_ws(p);
        p.bump(); // .
        skip_ws(p);
        parse_user_type_segment(p);
        cm = outer.complete(p, S::USER_TYPE);
    }
    let _ = cm;
    recv.complete(p, S::FUNCTION_TYPE_RECEIVER);
    skip_trivia(p);
    if p.at(S::DOT) {
        p.bump();
    }
    skip_trivia(p);
    // Parameter list — function-type-style.
    if p.at(S::LPAR) {
        let plist = p.start();
        p.bump();
        skip_trivia(p);
        if !p.at(S::RPAR) {
            loop {
                let prm = p.start();
                if p.at(S::IDENTIFIER) && next_non_trivia(p, 1) == S::COLON {
                    p.bump();
                    skip_trivia(p);
                    p.bump();
                    skip_trivia(p);
                }
                parse_type_ref(p);
                prm.complete(p, S::VALUE_PARAMETER);
                skip_trivia(p);
                if !p.at(S::COMMA) {
                    break;
                }
                p.bump();
                skip_trivia(p);
            }
        }
        if p.at(S::RPAR) {
            p.bump();
        }
        plist.complete(p, S::VALUE_PARAMETER_LIST);
    }
    skip_trivia(p);
    if p.at(S::ARROW) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
    }
    ft.complete(p, S::FUNCTION_TYPE);
}

fn looks_like_function_type(p: &Parser<'_, '_>) -> bool {
    // Find the matching `)`, then check if `->` follows past trivia.
    let mut depth = 0i32;
    let mut i = 0;
    loop {
        let k = p.nth(i);
        match k {
            S::LPAR => depth += 1,
            S::RPAR => {
                depth -= 1;
                if depth == 0 {
                    let mut j = i + 1;
                    loop {
                        let k2 = p.nth(j);
                        if matches!(
                            k2,
                            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT
                        ) {
                            j += 1;
                            continue;
                        }
                        return k2 == S::ARROW;
                    }
                }
            }
            S::EOF | S::LBRACE | S::SEMICOLON => return false,
            _ => {}
        }
        i += 1;
        if i > 128 {
            return false;
        }
    }
}

fn parse_function_type_after_modifiers(p: &mut Parser<'_, '_>) {
    let m = p.start();
    // No explicit receiver path here — keep simple.
    // Parameters are wrapped in a VALUE_PARAMETER_LIST composite.
    let plist = p.start();
    p.bump(); // (
    skip_trivia(p);
    if !p.at(S::RPAR) {
        loop {
            let prm = p.start();
            // The "name:" prefix is optional. If we see ident-then-colon, consume it.
            if p.at(S::IDENTIFIER) && next_non_trivia(p, 1) == S::COLON {
                p.bump(); // name
                skip_trivia(p);
                p.bump(); // :
                skip_trivia(p);
            }
            parse_type_ref(p);
            prm.complete(p, S::VALUE_PARAMETER);
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
            p.bump();
            skip_trivia(p);
        }
    }
    if p.at(S::RPAR) {
        p.bump();
    }
    plist.complete(p, S::VALUE_PARAMETER_LIST);
    skip_trivia(p);
    if p.at(S::ARROW) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
    }
    m.complete(p, S::FUNCTION_TYPE);
}

/// Parse a possibly-dotted user type (`A`, `A.B`, `A.B.C<X>`).
/// kotlinc PSI nests each dot step as a USER_TYPE composite wrapping
/// the previous USER_TYPE, so `A.B.C` becomes
///   USER_TYPE { USER_TYPE { USER_TYPE { ref:A }, ., ref:B }, ., ref:C }
/// Implemented via Marker::precede so the inner structure is rebuilt
/// on the fly without backtracking.
fn parse_user_type_chain(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let first = p.start();
    parse_user_type_segment(p);
    let mut cm = first.complete(p, S::USER_TYPE);
    loop {
        // Only consume WS if a DOT actually follows — otherwise the
        // trailing whitespace belongs to the OUTER composite, not to
        // USER_TYPE. (kotlinc PSI: USER_TYPE never has trailing WS.)
        if next_non_trivia(p, 0) != S::DOT {
            break;
        }
        let outer = cm.precede(p);
        skip_ws(p);
        p.bump(); // .
        skip_ws(p);
        parse_user_type_segment(p);
        cm = outer.complete(p, S::USER_TYPE);
    }
    cm
}

fn parse_user_type_segment(p: &mut Parser<'_, '_>) {
    let r = p.start();
    if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
        p.bump();
    } else {
        p.error("expected type name");
    }
    r.complete(p, S::REFERENCE_EXPRESSION);
    // Only consume WS+`<...>` for generic arguments; trailing trivia
    // after the identifier with NO generics stays at the caller's
    // level (matches kotlinc's USER_TYPE shape — no trailing
    // whitespace children).
    if next_non_trivia(p, 0) == S::LT {
        skip_ws(p);
        parse_type_argument_list(p);
    }
}

fn parse_type_argument_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // <
    skip_trivia(p);
    if !p.at(S::GT) {
        loop {
            parse_type_projection(p);
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
            p.bump();
            skip_trivia(p);
        }
    }
    if p.at(S::GT) {
        p.bump();
    }
    m.complete(p, S::TYPE_ARGUMENT_LIST);
}

fn parse_type_projection(p: &mut Parser<'_, '_>) {
    let m = p.start();
    parse_modifier_list_opt(p);
    skip_trivia(p);
    if p.at(S::MUL) {
        p.bump();
    } else {
        parse_type_ref(p);
    }
    m.complete(p, S::TYPE_PROJECTION);
}

// ─── parameter / argument lists ──────────────────────────────────────────────

fn parse_value_parameter_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // (
              // Trivia between LPAR and the first parameter sits at LIST level
              // — but only the WS run, not any KDoc/annotation that "belongs to"
              // the upcoming parameter. After WS, parse_value_parameter opens
              // its marker before the KDoc/annotations so they go inside.
    consume_list_level_trivia(p);
    while !p.at(S::RPAR) && !p.at(S::EOF) {
        parse_value_parameter(p);
        // Trivia after the parameter (before COMMA / RPAR) stays
        // at LIST level. Only WS though — comments past a comma get
        // tied to the next parameter (kotlinc's convention).
        let saved_pos = p.pos();
        if p.at(S::WHITE_SPACE) {
            p.bump();
        }
        if !p.at(S::COMMA) {
            // Roll back the whitespace bump — it should be at the
            // VALUE_PARAMETER level if there was no comma after. Use
            // skip_trivia anyway to advance to RPAR.
            let _ = saved_pos;
            break;
        }
        p.bump(); // ,
        consume_list_level_trivia(p);
    }
    if p.at(S::RPAR) {
        p.bump();
    }
    m.complete(p, S::VALUE_PARAMETER_LIST);
}

/// Consume only WHITESPACE at the *list* level. KDoc and
/// LINE_COMMENT/BLOCK_COMMENT belong to the following declaration
/// (kotlinc PSI attaches them under FUN/CLASS/PROPERTY etc., not as
/// siblings).
fn consume_list_level_trivia(p: &mut Parser<'_, '_>) {
    while p.at(S::WHITE_SPACE) {
        p.bump();
    }
}

/// Consume trivia (WS + line/block comments) at the very top of the
/// file. A license header that opens the file is owned by the FILE
/// element — kotlinc PSI does NOT attach it to the upcoming package
/// directive or annotation list.
fn consume_leading_file_trivia(p: &mut Parser<'_, '_>) {
    while matches!(
        p.current(),
        S::WHITE_SPACE | S::LINE_COMMENT | S::BLOCK_COMMENT
    ) {
        p.bump();
    }
}

fn parse_value_parameter(p: &mut Parser<'_, '_>) {
    let m = p.start();
    // KDoc / annotations / modifiers — these "belong to" this
    // parameter, so they go INSIDE the VALUE_PARAMETER composite.
    while p.at(S::KDOC) {
        p.bump();
        if p.at(S::WHITE_SPACE) {
            p.bump();
        }
    }
    // Param-position soft modifiers: `vararg`, `crossinline`,
    // `noinline`, and (for primary-ctor params) `val`/`var`. These
    // come BEFORE the param name, so they can't be picked up by the
    // generic modifier-list scan (which requires a decl-introducer
    // follower like `class`/`fun`).
    let ml = p.start();
    let mut had_any = false;
    loop {
        // Only consume trivia between modifiers when ANOTHER modifier
        // follows — trailing trivia after the last modifier belongs at
        // the VALUE_PARAMETER level (kotlinc PSI shape).
        let next = next_non_trivia(p, 0);
        let next_text = next_non_trivia_text(p, 0);
        let next_is_modifier_like = next == S::AT
            || is_modifier_keyword(next)
            || matches!(
                next_text,
                "vararg" | "crossinline" | "noinline"
            );
        if had_any {
            if next_is_modifier_like {
                skip_trivia(p);
            } else {
                break;
            }
        }
        let reclass = match p.current_text() {
            "vararg" if p.at(S::IDENTIFIER) => Some(S::KW_VARARG),
            "crossinline" if p.at(S::IDENTIFIER) => Some(S::KW_CROSSINLINE),
            "noinline" if p.at(S::IDENTIFIER) => Some(S::KW_NOINLINE),
            _ => None,
        };
        if let Some(k) = reclass {
            p.bump_as(k);
            had_any = true;
            continue;
        }
        if p.at(S::AT) {
            parse_annotation_entry(p);
            had_any = true;
            continue;
        }
        if is_modifier_keyword(p.current()) {
            p.bump();
            had_any = true;
            continue;
        }
        break;
    }
    if had_any {
        ml.complete(p, S::MODIFIER_LIST);
    } else {
        ml.abandon(p);
    }
    skip_trivia(p);
    if matches!(p.current(), S::KW_VAL | S::KW_VAR) {
        p.bump();
        skip_trivia(p);
    }
    if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
        p.bump();
    }
    skip_trivia(p);
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
        skip_trivia(p);
    }
    if p.at(S::EQ) {
        p.bump();
        skip_trivia(p);
        parse_expression(p);
    }
    m.complete(p, S::VALUE_PARAMETER);
}

fn parse_value_argument_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // (
    skip_trivia(p);
    if !p.at(S::RPAR) {
        loop {
            parse_value_argument(p);
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
            p.bump();
            skip_trivia(p);
            // Trailing comma: `foo(a, b,)` — no further argument.
            if p.at(S::RPAR) {
                break;
            }
        }
    }
    if p.at(S::RPAR) {
        p.bump();
    }
    m.complete(p, S::VALUE_ARGUMENT_LIST);
}

fn parse_value_argument(p: &mut Parser<'_, '_>) {
    let m = p.start();
    // `name = expr` form. kotlinc PSI wraps the name in a
    // REFERENCE_EXPRESSION inside the VALUE_ARGUMENT_NAME.
    if p.at(S::IDENTIFIER) && next_non_trivia(p, 1) == S::EQ {
        let nm = p.start();
        let r = p.start();
        p.bump();
        r.complete(p, S::REFERENCE_EXPRESSION);
        nm.complete(p, S::VALUE_ARGUMENT_NAME);
        skip_trivia(p);
        p.bump(); // =
        skip_trivia(p);
    }
    if p.at(S::MUL) {
        p.bump();
        skip_trivia(p);
    }
    parse_expression(p);
    m.complete(p, S::VALUE_ARGUMENT);
}

// ─── expressions ─────────────────────────────────────────────────────────────

pub fn parse_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    parse_assignment(p)
}

/// Top-level precedence for assignment-style operators: `=`, `+=`,
/// `-=`, `*=`, `/=`, `%=`. kotlinc PSI wraps these as
/// BINARY_EXPRESSION with the operator inside an OPERATION_REFERENCE.
fn parse_assignment(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let lhs = parse_disjunction(p);
    let next = next_non_trivia(p, 0);
    if !matches!(
        next,
        S::EQ | S::PLUSEQ | S::MINUSEQ | S::MULEQ | S::DIVEQ | S::PERCEQ
    ) {
        return lhs;
    }
    skip_ws(p);
    let m = lhs.precede(p);
    let op = p.start();
    p.bump();
    op.complete(p, S::OPERATION_REFERENCE);
    skip_trivia(p);
    parse_disjunction(p);
    m.complete(p, S::BINARY_EXPRESSION)
}

// All the precedence-climbing parsers below follow the same shape:
// peek past inline trivia for the operator; if not present, break
// without consuming the trivia. This keeps trailing whitespace at
// the outer composite level (matches kotlinc PSI shape).

fn parse_disjunction(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_conjunction(p);
    loop {
        if next_non_trivia(p, 0) != S::OROR {
            break;
        }
        skip_ws(p);
        let m = lhs.precede(p);
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_conjunction(p);
        lhs = m.complete(p, S::BINARY_EXPRESSION);
    }
    lhs
}

fn parse_conjunction(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_equality(p);
    loop {
        if next_non_trivia(p, 0) != S::ANDAND {
            break;
        }
        skip_ws(p);
        let m = lhs.precede(p);
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_equality(p);
        lhs = m.complete(p, S::BINARY_EXPRESSION);
    }
    lhs
}

fn parse_equality(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_comparison(p);
    loop {
        if !matches!(next_non_trivia(p, 0), S::EQEQ | S::EXCLEQ) {
            break;
        }
        skip_ws(p);
        let m = lhs.precede(p);
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_comparison(p);
        lhs = m.complete(p, S::BINARY_EXPRESSION);
    }
    lhs
}

fn parse_comparison(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_elvis(p);
    loop {
        if !matches!(next_non_trivia(p, 0), S::LT | S::GT | S::LTEQ | S::GTEQ) {
            break;
        }
        skip_ws(p);
        let m = lhs.precede(p);
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_elvis(p);
        lhs = m.complete(p, S::BINARY_EXPRESSION);
    }
    lhs
}

fn parse_elvis(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_infix_op(p);
    loop {
        if next_non_trivia(p, 0) != S::ELVIS {
            break;
        }
        // Open the BINARY_EXPRESSION wrapper FIRST, then bump any
        // leading newline-WS so it sits as a sibling of the
        // OPERATION_REFERENCE inside the wrapper (kotlinc PSI shape).
        let m = lhs.precede(p);
        skip_trivia(p);
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_infix_op(p);
        lhs = m.complete(p, S::BINARY_EXPRESSION);
    }
    lhs
}

fn parse_infix_op(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_range(p);
    loop {
        // Newlines end an infix-operator chain in Kotlin — `a\nin b` is
        // statement `a` followed by statement `in b`, not `a in b`. Bail
        // out if a newline precedes the next token.
        if has_newline_before_next_non_trivia(p) {
            break;
        }
        let next = next_non_trivia(p, 0);
        // `is X`
        if next == S::KW_IS {
            skip_ws(p);
            let m = lhs.precede(p);
            let op = p.start();
            p.bump();
            op.complete(p, S::OPERATION_REFERENCE);
            skip_trivia(p);
            parse_type_ref(p);
            lhs = m.complete(p, S::IS_EXPRESSION);
            continue;
        }
        // `in X`
        if next == S::KW_IN {
            skip_ws(p);
            let m = lhs.precede(p);
            let op = p.start();
            p.bump();
            op.complete(p, S::OPERATION_REFERENCE);
            skip_trivia(p);
            parse_range(p);
            lhs = m.complete(p, S::BINARY_EXPRESSION);
            continue;
        }
        // `!is X` / `!in X` — negated form. The `!` is followed by
        // the keyword. The two raw tokens stay separate so the YAML
        // matches kotlinc's PSI which lists them as siblings inside
        // OPERATION_REFERENCE.
        if next == S::EXCL {
            let excl_offset = next_non_trivia_offset(p, 0);
            let nextnext = next_non_trivia(p, excl_offset + 1);
            if matches!(nextnext, S::KW_IS | S::KW_IN) {
                skip_ws(p);
                let m = lhs.precede(p);
                let op = p.start();
                p.bump(); // !
                skip_ws(p);
                p.bump(); // is / in
                op.complete(p, S::OPERATION_REFERENCE);
                skip_trivia(p);
                if nextnext == S::KW_IS {
                    parse_type_ref(p);
                    lhs = m.complete(p, S::IS_EXPRESSION);
                } else {
                    parse_range(p);
                    lhs = m.complete(p, S::BINARY_EXPRESSION);
                }
                continue;
            }
        }
        // Generic infix function call: `lhs IDENT rhs` (e.g. `0 until
        // 5`, `a or b`, `xs shr 2`). Only kicks in when the IDENT is
        // not a hard keyword and is followed by an expression on the
        // same line.
        if next == S::IDENTIFIER && is_infix_function_position(p) {
            skip_ws(p);
            let m = lhs.precede(p);
            let op = p.start();
            let r = p.start();
            p.bump(); // IDENT
            r.complete(p, S::REFERENCE_EXPRESSION);
            op.complete(p, S::OPERATION_REFERENCE);
            skip_trivia(p);
            parse_range(p);
            lhs = m.complete(p, S::BINARY_EXPRESSION);
            continue;
        }
        break;
    }
    lhs
}

/// Heuristic: when does `IDENT` at the cursor represent an infix
/// function call? We require:
/// 1. Trivia separation from the previous token (so `foo.bar` and
///    `foo()` don't get mis-parsed as infix calls).
/// 2. An expression-starting token follows the IDENT (so `IDENT(`,
///    `IDENT.`, etc. — call shapes — don't trigger here).
fn is_infix_function_position(p: &Parser<'_, '_>) -> bool {
    // Cursor must be at WS, then the IDENT (or at WS containing
    // newline — which we already rejected above — or directly at the
    // IDENT with no leading WS, which isn't infix).
    if !matches!(p.current(), S::WHITE_SPACE) {
        return false;
    }
    if p.text_at(0).contains('\n') {
        return false;
    }
    let after_ws = p.nth(1);
    if after_ws != S::IDENTIFIER {
        return false;
    }
    // The IDENT must NOT be one of the soft keywords we already
    // intercept elsewhere (decl introducers, modifiers).
    let ident_text = p.text_at(1);
    if matches!(
        ident_text,
        "fun"
            | "val"
            | "var"
            | "class"
            | "object"
            | "interface"
            | "package"
            | "import"
            | "return"
            | "throw"
            | "break"
            | "continue"
            | "if"
            | "else"
            | "when"
            | "for"
            | "while"
            | "do"
            | "try"
            | "catch"
            | "finally"
            | "by"
            | "where"
            | "get"
            | "set"
            | "constructor"
            | "init"
            | "this"
            | "super"
            | "true"
            | "false"
            | "null"
    ) {
        return false;
    }
    // Look past the IDENT for an expression-starter.
    let mut j = 2usize;
    while matches!(p.nth(j), S::WHITE_SPACE) {
        if p.text_at(j).contains('\n') {
            return false;
        }
        j += 1;
    }
    matches!(
        p.nth(j),
        S::IDENTIFIER
            | S::INTEGER_LITERAL
            | S::LONG_LITERAL
            | S::FLOAT_LITERAL
            | S::DOUBLE_LITERAL
            | S::CHARACTER_LITERAL
            | S::STRING_START
            | S::LPAR
            | S::LBRACE
            | S::LBRACKET
            | S::KW_TRUE
            | S::KW_FALSE
            | S::KW_NULL
            | S::KW_IF
            | S::KW_WHEN
            | S::KW_TRY
            | S::MINUS
            | S::PLUS
            | S::EXCL
    )
}

fn parse_range(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_additive(p);
    loop {
        if next_non_trivia(p, 0) != S::DOTDOT {
            break;
        }
        skip_ws(p);
        let m = lhs.precede(p);
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_additive(p);
        lhs = m.complete(p, S::BINARY_EXPRESSION);
    }
    lhs
}

fn parse_additive(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_multiplicative(p);
    loop {
        if !matches!(next_non_trivia(p, 0), S::PLUS | S::MINUS) {
            break;
        }
        skip_ws(p);
        let m = lhs.precede(p);
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_multiplicative(p);
        lhs = m.complete(p, S::BINARY_EXPRESSION);
    }
    lhs
}

fn parse_multiplicative(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_as_expression(p);
    loop {
        if !matches!(next_non_trivia(p, 0), S::MUL | S::DIV | S::PERC) {
            break;
        }
        skip_ws(p);
        let m = lhs.precede(p);
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_as_expression(p);
        lhs = m.complete(p, S::BINARY_EXPRESSION);
    }
    lhs
}

fn parse_as_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_prefix(p);
    loop {
        if next_non_trivia(p, 0) != S::KW_AS {
            break;
        }
        skip_ws(p);
        let m = lhs.precede(p);
        let op = p.start();
        // `as?` is the safe-cast operator. The lexer emits it as two
        // separate tokens (KW_AS + QUEST) — fuse them at parse time
        // into a single AS_SAFE leaf so the YAML shape matches
        // kotlinc's `OPERATION_REFERENCE { AS_SAFE "as?" }`.
        if p.nth(1) == S::QUEST {
            p.bump_n_as(2, S::AS_SAFE);
        } else {
            p.bump();
        }
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_type_ref(p);
        lhs = m.complete(p, S::BINARY_WITH_TYPE_RHS_EXPRESSION);
    }
    lhs
}

fn parse_prefix(p: &mut Parser<'_, '_>) -> CompletedMarker {
    skip_trivia(p);
    if matches!(
        p.current(),
        S::EXCL | S::MINUS | S::PLUS | S::PLUSPLUS | S::MINUSMINUS
    ) {
        let m = p.start();
        let op = p.start();
        p.bump();
        op.complete(p, S::OPERATION_REFERENCE);
        skip_trivia(p);
        parse_prefix(p);
        return m.complete(p, S::PREFIX_EXPRESSION);
    }
    parse_postfix(p)
}

fn parse_postfix(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let mut lhs = parse_atom(p);
    loop {
        // Kotlin allows inline whitespace between an expression and
        // its postfix operator (`a . b`, `f (x)`, `g { lambda }`).
        // We peek past WHITE_SPACE only if its text contains no
        // newline — a newline ends the expression.
        //
        // Exception: `.`, `?.`, and `::` continue the expression even
        // across a newline (call chains can break BEFORE the dot).
        if matches!(p.current(), S::WHITE_SPACE) {
            let nl = p.current_text().contains('\n');
            let next = p.nth(1);
            let dot_like = matches!(next, S::DOT | S::QUESTDOT | S::COLONCOLON);
            if (!nl && is_postfix_starter(next)) || (nl && dot_like) {
                // Bump the WS at the parser level so it appears as a
                // sibling of `lhs` inside the soon-to-be-precede'd
                // DOT_QUALIFIED_EXPRESSION composite — kotlinc places
                // the leading whitespace INSIDE the dot-qualified
                // wrapper, between LHS and DOT.
                p.bump();
            }
        }
        match p.current() {
            S::DOT => {
                let m = lhs.precede(p);
                p.bump();
                skip_ws(p);
                // If the right-hand side is `IDENT(args)`, kotlinc PSI
                // wraps it as a CALL_EXPRESSION *inside* the
                // DOT_QUALIFIED_EXPRESSION. Otherwise it's a bare
                // REFERENCE_EXPRESSION (with optional postfix ops
                // applied via the outer postfix loop).
                if is_call_rhs(p) {
                    parse_call_after_name(p);
                } else {
                    parse_atom(p);
                }
                lhs = m.complete(p, S::DOT_QUALIFIED_EXPRESSION);
            }
            S::QUESTDOT => {
                let m = lhs.precede(p);
                p.bump();
                skip_ws(p);
                if is_call_rhs(p) {
                    parse_call_after_name(p);
                } else {
                    parse_atom(p);
                }
                lhs = m.complete(p, S::SAFE_ACCESS_EXPRESSION);
            }
            S::LPAR => {
                let m = lhs.precede(p);
                parse_value_argument_list(p);
                // Optional trailing lambda. Only swallow WS if a `{`
                // actually follows; otherwise the trailing WS belongs
                // to the OUTER composite.
                if next_non_trivia(p, 0) == S::LBRACE {
                    skip_ws(p);
                    let la = p.start();
                    parse_lambda_expression(p);
                    la.complete(p, S::LAMBDA_ARGUMENT);
                }
                lhs = m.complete(p, S::CALL_EXPRESSION);
            }
            S::LBRACE => {
                // Trailing lambda only (no preceding arg list).
                let m = lhs.precede(p);
                let la = p.start();
                parse_lambda_expression(p);
                la.complete(p, S::LAMBDA_ARGUMENT);
                lhs = m.complete(p, S::CALL_EXPRESSION);
            }
            S::LBRACKET => {
                let m = lhs.precede(p);
                parse_indices(p);
                lhs = m.complete(p, S::ARRAY_ACCESS_EXPRESSION);
            }
            S::EXCLEXCL => {
                let m = lhs.precede(p);
                let op = p.start();
                p.bump();
                op.complete(p, S::OPERATION_REFERENCE);
                lhs = m.complete(p, S::POSTFIX_EXPRESSION);
            }
            S::PLUSPLUS | S::MINUSMINUS => {
                let m = lhs.precede(p);
                let op = p.start();
                p.bump();
                op.complete(p, S::OPERATION_REFERENCE);
                lhs = m.complete(p, S::POSTFIX_EXPRESSION);
            }
            S::COLONCOLON => {
                let m = lhs.precede(p);
                p.bump();
                skip_ws(p);
                // `X::class` is a CLASS_LITERAL_EXPRESSION with a bare
                // `class` keyword leaf (not wrapped in a REFERENCE).
                // `X::name` is a CALLABLE_REFERENCE_EXPRESSION whose
                // RHS is a REFERENCE_EXPRESSION { IDENTIFIER name }.
                if p.at(S::KW_CLASS) {
                    p.bump();
                    lhs = m.complete(p, S::CLASS_LITERAL_EXPRESSION);
                } else {
                    let r = p.start();
                    if p.at(S::IDENTIFIER) {
                        p.bump();
                    }
                    r.complete(p, S::REFERENCE_EXPRESSION);
                    lhs = m.complete(p, S::CALLABLE_REFERENCE_EXPRESSION);
                }
            }
            S::LT if looks_like_type_args(p) => {
                let m = lhs.precede(p);
                parse_type_argument_list(p);
                if p.at(S::LPAR) {
                    parse_value_argument_list(p);
                    if p.at(S::LBRACE) {
                        let la = p.start();
                        parse_lambda_expression(p);
                        la.complete(p, S::LAMBDA_ARGUMENT);
                    }
                }
                lhs = m.complete(p, S::CALL_EXPRESSION);
            }
            _ => break,
        }
    }
    lhs
}

/// Token kinds that can start a postfix operator (member access,
/// call, indexing, trailing lambda, etc.). Used by the postfix loop
/// to decide whether to skip inline whitespace.
fn is_postfix_starter(k: SyntaxKind) -> bool {
    matches!(
        k,
        S::DOT
            | S::QUESTDOT
            | S::LPAR
            | S::LBRACE
            | S::LBRACKET
            | S::EXCLEXCL
            | S::PLUSPLUS
            | S::MINUSMINUS
            | S::COLONCOLON
    )
}

/// `true` if the cursor is at `IDENT (...)` (or with a soft-keyword
/// identifier). Used to decide whether a `.NAME` should fold into a
/// CALL_EXPRESSION inside the qualified expression. `IDENT<` is only
/// a call if the `<...>` reads as a balanced type-argument list (we
/// don't want to misclassify `IDENT<width` as a generic call when it's
/// actually a less-than comparison).
fn is_call_rhs(p: &Parser<'_, '_>) -> bool {
    if !(p.at(S::IDENTIFIER) || is_soft_keyword(p.current())) {
        return false;
    }
    let mut i = 1;
    while matches!(p.nth(i), S::WHITE_SPACE) {
        i += 1;
    }
    match p.nth(i) {
        S::LPAR | S::LBRACE => true,
        S::LT => looks_like_type_args_at_offset(p, i),
        _ => false,
    }
}

/// Re-uses the heuristic from `looks_like_type_args` but starting at
/// an arbitrary offset (the `<` need not be the current token).
fn looks_like_type_args_at_offset(p: &Parser<'_, '_>, base: usize) -> bool {
    let mut depth = 1i32;
    let mut i = base + 1;
    loop {
        let k = p.nth(i);
        match k {
            S::LT => depth += 1,
            S::GT => {
                depth -= 1;
                if depth == 0 {
                    let mut j = i + 1;
                    loop {
                        let k2 = p.nth(j);
                        if matches!(k2, S::WHITE_SPACE) {
                            j += 1;
                            continue;
                        }
                        // `>` can be followed by `(`/`{`/`.`/`::` if it
                        // closed a type-argument list (trailing-lambda
                        // call shape: `Foo<X> { ... }`).
                        return matches!(k2, S::LPAR | S::LBRACE | S::DOT | S::COLONCOLON);
                    }
                }
            }
            S::IDENTIFIER | S::COMMA | S::DOT | S::QUEST | S::MUL | S::WHITE_SPACE => {}
            _ => return false,
        }
        i += 1;
        if i > 64 {
            return false;
        }
    }
}

/// Parse `IDENT(args)` (or `IDENT<type-args>(args)` or `IDENT { lambda }`)
/// as a CALL_EXPRESSION. Cursor is at the IDENT. Trailing trivia
/// (the WS before the next sibling) stays outside.
fn parse_call_after_name(p: &mut Parser<'_, '_>) {
    let call = p.start();
    let r = p.start();
    p.bump(); // IDENT
    r.complete(p, S::REFERENCE_EXPRESSION);
    if next_non_trivia(p, 0) == S::LT && looks_like_type_args_at(p) {
        skip_ws(p);
        parse_type_argument_list(p);
    }
    if next_non_trivia(p, 0) == S::LPAR {
        skip_ws(p);
        parse_value_argument_list(p);
    }
    if next_non_trivia(p, 0) == S::LBRACE {
        skip_ws(p);
        let la = p.start();
        parse_lambda_expression(p);
        la.complete(p, S::LAMBDA_ARGUMENT);
    }
    call.complete(p, S::CALL_EXPRESSION);
}

/// Same as `looks_like_type_args` but expects the cursor to already
/// be at the `<` it's testing. Needed for the call-after-name path.
fn looks_like_type_args_at(p: &Parser<'_, '_>) -> bool {
    // Reuse the cursor-at-`<` predicate. We're guaranteed cursor is
    // at `<` by the caller because they peeked via next_non_trivia.
    looks_like_type_args(p)
}

fn looks_like_type_args(p: &Parser<'_, '_>) -> bool {
    // Heuristic: scan forward looking for `>` followed by `(`. If we
    // see binary-op tokens first, it's a comparison.
    let mut depth = 1i32;
    let mut i = 1;
    loop {
        let k = p.nth(i);
        match k {
            S::LT => depth += 1,
            S::GT => {
                depth -= 1;
                if depth == 0 {
                    let mut j = i + 1;
                    loop {
                        let k2 = p.nth(j);
                        if matches!(k2, S::WHITE_SPACE | S::NEWLINE) {
                            j += 1;
                            continue;
                        }
                        return matches!(k2, S::LPAR | S::LBRACE | S::DOT | S::COLONCOLON);
                    }
                }
            }
            S::IDENTIFIER | S::COMMA | S::DOT | S::QUEST | S::MUL | S::WHITE_SPACE | S::NEWLINE => {
            }
            _ => return false,
        }
        i += 1;
        if i > 64 {
            return false;
        }
    }
}

fn parse_indices(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // [
    skip_trivia(p);
    if !p.at(S::RBRACKET) {
        loop {
            parse_expression(p);
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
            p.bump();
            skip_trivia(p);
        }
    }
    if p.at(S::RBRACKET) {
        p.bump();
    }
    m.complete(p, S::INDICES);
}

/// Consume an optional `@label` qualifier on `this` / `super` / loop
/// targets. Emitted as LABEL_QUALIFIER { LABEL { AT, IDENTIFIER } }
/// to match kotlinc PSI shape. No-op when the next token isn't `@`.
fn consume_label_qualifier_opt(p: &mut Parser<'_, '_>) {
    if !p.at(S::AT) {
        return;
    }
    let q = p.start();
    let l = p.start();
    p.bump(); // @
    if p.at(S::IDENTIFIER) {
        p.bump();
    }
    l.complete(p, S::LABEL);
    q.complete(p, S::LABEL_QUALIFIER);
}

fn parse_atom(p: &mut Parser<'_, '_>) -> CompletedMarker {
    skip_trivia(p);
    // `this` lexes as IDENT in our lexer but is a keyword in kotlinc.
    // Detect it here and route through the THIS_EXPRESSION arm so the
    // YAML matches the reference shape.
    if p.at(S::IDENTIFIER) && p.current_text() == "this" {
        let m = p.start();
        let r = p.start();
        p.bump_as(S::KW_THIS);
        r.complete(p, S::REFERENCE_EXPRESSION);
        consume_label_qualifier_opt(p);
        return m.complete(p, S::THIS_EXPRESSION);
    }
    // Standalone callable reference: `::name` or `::class`. Wraps
    // the reference in CALLABLE_REFERENCE_EXPRESSION (or
    // CLASS_LITERAL_EXPRESSION).
    if p.at(S::COLONCOLON) {
        let m = p.start();
        p.bump();
        skip_ws(p);
        if p.at(S::KW_CLASS) {
            p.bump();
            return m.complete(p, S::CLASS_LITERAL_EXPRESSION);
        }
        let r = p.start();
        if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
            p.bump();
        }
        r.complete(p, S::REFERENCE_EXPRESSION);
        return m.complete(p, S::CALLABLE_REFERENCE_EXPRESSION);
    }
    match p.current() {
        S::IDENTIFIER => {
            let m = p.start();
            p.bump();
            m.complete(p, S::REFERENCE_EXPRESSION)
        }
        k if is_soft_keyword(k) => {
            let m = p.start();
            p.bump();
            m.complete(p, S::REFERENCE_EXPRESSION)
        }
        S::INTEGER_LITERAL | S::LONG_LITERAL => {
            let m = p.start();
            p.bump();
            m.complete(p, S::INTEGER_CONSTANT)
        }
        S::FLOAT_LITERAL | S::DOUBLE_LITERAL => {
            let m = p.start();
            p.bump();
            m.complete(p, S::FLOAT_CONSTANT)
        }
        S::CHARACTER_LITERAL => {
            let m = p.start();
            p.bump();
            m.complete(p, S::CHARACTER_CONSTANT)
        }
        S::KW_TRUE | S::KW_FALSE => {
            let m = p.start();
            p.bump();
            m.complete(p, S::BOOLEAN_CONSTANT)
        }
        S::KW_NULL => {
            let m = p.start();
            p.bump();
            m.complete(p, S::NULL_CONSTANT)
        }
        S::STRING_START => parse_string_template(p),
        S::STRING_LITERAL => {
            let m = p.start();
            p.bump();
            m.complete(p, S::STRING_TEMPLATE)
        }
        S::LPAR => parse_parenthesized_or_function_literal(p),
        S::LBRACE => parse_lambda_expression(p),
        S::LBRACKET => parse_collection_literal(p),
        S::KW_IF => parse_if_expression(p),
        S::KW_WHEN => parse_when_expression(p),
        S::KW_FOR => parse_for_statement(p),
        S::KW_WHILE => parse_while_statement(p),
        S::KW_DO => parse_do_while_statement(p),
        S::KW_TRY => parse_try_expression(p),
        S::KW_RETURN => parse_return_expression(p),
        S::KW_THROW => parse_throw_expression(p),
        S::KW_BREAK => parse_break_expression(p),
        S::KW_CONTINUE => parse_continue_expression(p),
        S::KW_THIS => {
            let m = p.start();
            let r = p.start();
            p.bump();
            r.complete(p, S::REFERENCE_EXPRESSION);
            consume_label_qualifier_opt(p);
            m.complete(p, S::THIS_EXPRESSION)
        }
        S::KW_SUPER => {
            let m = p.start();
            let r = p.start();
            p.bump();
            r.complete(p, S::REFERENCE_EXPRESSION);
            consume_label_qualifier_opt(p);
            m.complete(p, S::SUPER_EXPRESSION)
        }
        S::KW_OBJECT => parse_object_literal(p),
        S::AT => {
            // `@Name(args) <expr>` is an ANNOTATED_EXPRESSION whose
            // body is the trailing expression and whose annotation is
            // wrapped in an ANNOTATION_ENTRY. The body is itself a
            // postfix expression — a `Call(args)` after `@Name(args)`
            // is part of THIS annotated expression, not a postfix on
            // the whole annotated form.
            let m = p.start();
            parse_annotation_entry(p);
            skip_trivia(p);
            parse_postfix(p);
            m.complete(p, S::ANNOTATED_EXPRESSION)
        }
        _ => {
            let m = p.start();
            if !p.at(S::EOF) {
                p.bump();
                p.error("expected expression");
            }
            m.complete(p, S::ERROR_ELEMENT)
        }
    }
}

fn parse_string_template(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    let is_raw = p.current_text().starts_with("\"\"\"");
    p.bump(); // STRING_START
    loop {
        match p.current() {
            S::STRING_END => {
                p.bump();
                break;
            }
            S::EOF => break,
            S::STRING_CHUNK => {
                // In a regular `"..."` string, a backslash-prefixed
                // chunk is an escape sequence — wrap it as
                // ESCAPE_STRING_TEMPLATE_ENTRY with the token
                // reclassified as ESCAPE_SEQUENCE. In a raw `"""..."""`
                // string, no escape interpretation happens; the
                // backslash is just literal text.
                if !is_raw && p.current_text().starts_with('\\') {
                    let entry = p.start();
                    p.bump_as(S::ESCAPE_SEQUENCE);
                    entry.complete(p, S::ESCAPE_STRING_TEMPLATE_ENTRY);
                } else {
                    let lit = p.start();
                    p.bump();
                    lit.complete(p, S::LITERAL_STRING_TEMPLATE_ENTRY);
                }
            }
            S::STRING_IDENT_REF => {
                let short = p.start();
                p.bump();
                short.complete(p, S::SHORT_STRING_TEMPLATE_ENTRY);
            }
            S::STRING_EXPR_START => {
                let long = p.start();
                p.bump();
                skip_trivia(p);
                if !p.at(S::STRING_EXPR_END) && !p.at(S::EOF) {
                    parse_expression(p);
                    skip_trivia(p);
                }
                if p.at(S::STRING_EXPR_END) {
                    p.bump();
                }
                long.complete(p, S::LONG_STRING_TEMPLATE_ENTRY);
            }
            _ => {
                // Stray token inside the template — consume one to
                // avoid an infinite loop. Shouldn't happen with a
                // well-behaved lexer.
                p.bump();
            }
        }
    }
    m.complete(p, S::STRING_TEMPLATE)
}

fn parse_parenthesized_or_function_literal(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // (
    skip_trivia(p);
    if !p.at(S::RPAR) {
        parse_expression(p);
        skip_trivia(p);
    }
    if p.at(S::RPAR) {
        p.bump();
    }
    m.complete(p, S::PARENTHESIZED)
}

fn parse_collection_literal(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // [
    skip_trivia(p);
    if !p.at(S::RBRACKET) {
        loop {
            parse_expression(p);
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
            p.bump();
            skip_trivia(p);
        }
    }
    if p.at(S::RBRACKET) {
        p.bump();
    }
    m.complete(p, S::COLLECTION_LITERAL_EXPRESSION)
}

fn parse_lambda_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    let lit = p.start();
    p.bump(); // {
              // Skip a leading WS (kotlinc places it between `{` and the
              // BLOCK at FUNCTION_LITERAL level, not inside the BLOCK).
    if p.at(S::WHITE_SPACE) {
        p.bump();
    }
    // Optional parameter list `x, y ->` or `x: Int, y: Int ->`.
    if has_lambda_params(p) {
        let pl = p.start();
        loop {
            let prm = p.start();
            if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
                p.bump();
            }
            // Only consume trailing trivia if a COLON (type annotation)
            // follows — otherwise the trivia belongs to the surrounding
            // VALUE_PARAMETER_LIST (before the comma or `->`).
            if next_non_trivia(p, 0) == S::COLON {
                skip_trivia(p);
                p.bump();
                skip_trivia(p);
                parse_type_ref(p);
            }
            prm.complete(p, S::VALUE_PARAMETER);
            if next_non_trivia(p, 0) != S::COMMA {
                break;
            }
            skip_trivia(p);
            p.bump();
            skip_trivia(p);
        }
        pl.complete(p, S::VALUE_PARAMETER_LIST);
        skip_trivia(p);
        if p.at(S::ARROW) {
            p.bump();
        }
        // WS between ARROW and BLOCK sits at FUNCTION_LITERAL level.
        if p.at(S::WHITE_SPACE) {
            p.bump();
        }
    }
    // Lambda body is wrapped in a BLOCK composite — emitted even
    // when empty (`{}` -> BLOCK with empty children). The trailing
    // WS before `}` sits at FUNCTION_LITERAL level, not inside BLOCK.
    let block = p.start();
    if !p.at(S::RBRACE) && next_non_trivia(p, 0) != S::RBRACE {
        parse_block_body(p);
    }
    block.complete(p, S::BLOCK);
    // Any trailing trivia (WS, line/block comments, KDoc) before `}`
    // sits at FUNCTION_LITERAL level, not inside BLOCK.
    while matches!(
        p.current(),
        S::WHITE_SPACE | S::LINE_COMMENT | S::BLOCK_COMMENT | S::KDOC
    ) {
        p.bump();
    }
    if p.at(S::RBRACE) {
        p.bump();
    }
    lit.complete(p, S::FUNCTION_LITERAL);
    m.complete(p, S::LAMBDA_EXPRESSION)
}

fn has_lambda_params(p: &Parser<'_, '_>) -> bool {
    // Look ahead for an `->` before any unbalanced `}`.
    let mut depth_brace = 0i32;
    let mut depth_par = 0i32;
    let mut i = 0;
    loop {
        let k = p.nth(i);
        match k {
            S::ARROW if depth_brace == 0 && depth_par == 0 => return true,
            S::LBRACE => depth_brace += 1,
            S::RBRACE => {
                if depth_brace == 0 {
                    return false;
                }
                depth_brace -= 1;
            }
            S::LPAR => depth_par += 1,
            S::RPAR => depth_par -= 1,
            S::SEMICOLON | S::EOF => return false,
            _ => {}
        }
        i += 1;
        if i > 64 {
            return false;
        }
    }
}

// ─── control flow ────────────────────────────────────────────────────────────

fn parse_if_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // if
    skip_trivia(p);
    // LPAR / RPAR live as direct children of IF; only the inner
    // expression is wrapped in CONDITION (matches kotlinc PSI).
    if p.at(S::LPAR) {
        p.bump();
        skip_trivia(p);
        let c = p.start();
        parse_expression(p);
        c.complete(p, S::CONDITION);
        skip_trivia(p);
        if p.at(S::RPAR) {
            p.bump();
        }
    }
    skip_trivia(p);
    // THEN wraps just the body; the trailing WS sits at IF level.
    if !p.at(S::KW_ELSE) && !p.at(S::EOF) {
        let then = p.start();
        parse_control_body(p);
        then.complete(p, S::THEN);
    }
    if next_non_trivia(p, 0) == S::KW_ELSE {
        skip_trivia(p);
        p.bump(); // else
        skip_trivia(p);
        let el = p.start();
        parse_control_body(p);
        el.complete(p, S::ELSE);
    }
    m.complete(p, S::IF)
}

/// Body of an `if/else`/`while`/`for` etc. A `{...}` here is a BLOCK,
/// NOT a lambda. Other expressions/statements parse normally.
fn parse_control_body(p: &mut Parser<'_, '_>) {
    if p.at(S::LBRACE) {
        parse_block(p);
    } else {
        parse_statement_or_expression(p);
    }
}

/// Like `parse_control_body`, but wraps the result in a `BODY`
/// composite — kotlinc PSI wraps loop bodies (`for`, `while`, `do`)
/// in a BODY node containing the BLOCK / statement.
fn parse_loop_body(p: &mut Parser<'_, '_>) {
    let b = p.start();
    parse_control_body(p);
    b.complete(p, S::BODY);
}

fn parse_when_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // when
    skip_trivia(p);
    if p.at(S::LPAR) {
        p.bump();
        skip_trivia(p);
        parse_expression(p);
        skip_trivia(p);
        if p.at(S::RPAR) {
            p.bump();
        }
        skip_trivia(p);
    }
    if p.at(S::LBRACE) {
        p.bump();
        skip_trivia(p);
        while !p.at(S::RBRACE) && !p.at(S::EOF) {
            parse_when_entry(p);
            // Allow `;` between entries (mostly for the inline form
            // `when (x) { 1 -> a; 2 -> b }`).
            skip_trivia(p);
            if p.at(S::SEMICOLON) {
                p.bump();
                skip_trivia(p);
            }
        }
        if p.at(S::RBRACE) {
            p.bump();
        }
    }
    m.complete(p, S::WHEN)
}

fn parse_when_entry(p: &mut Parser<'_, '_>) {
    let m = p.start();
    if p.at(S::KW_ELSE) {
        p.bump();
        skip_trivia(p);
    } else {
        loop {
            let cond = p.start();
            if p.at(S::KW_IS) {
                p.bump();
                skip_trivia(p);
                parse_type_ref(p);
                cond.complete(p, S::WHEN_CONDITION_IS_PATTERN);
            } else if p.at(S::KW_IN) {
                p.bump();
                skip_trivia(p);
                parse_expression(p);
                cond.complete(p, S::WHEN_CONDITION_IN_RANGE);
            } else {
                parse_expression(p);
                cond.complete(p, S::WHEN_CONDITION_WITH_EXPRESSION);
            }
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
            p.bump();
            skip_trivia(p);
        }
    }
    if p.at(S::ARROW) {
        p.bump();
        skip_trivia(p);
        parse_control_body(p);
    }
    m.complete(p, S::WHEN_ENTRY);
}

fn parse_for_statement(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // for
    skip_trivia(p);
    if p.at(S::LPAR) {
        p.bump();
        skip_trivia(p);
        // Parameter
        let prm = p.start();
        if p.at(S::LPAR) {
            // Destructuring `(a, b) in ...`
            let d = p.start();
            p.bump();
            loop {
                skip_trivia(p);
                if !(p.at(S::IDENTIFIER) || is_soft_keyword(p.current())) {
                    break;
                }
                let e = p.start();
                p.bump();
                e.complete(p, S::DESTRUCTURING_DECLARATION_ENTRY);
                skip_trivia(p);
                if !p.at(S::COMMA) {
                    break;
                }
                p.bump();
            }
            if p.at(S::RPAR) {
                p.bump();
            }
            d.complete(p, S::DESTRUCTURING_DECLARATION);
        } else if p.at(S::IDENTIFIER) {
            p.bump();
        }
        // Only consume trailing trivia if a COLON (type annotation)
        // follows — otherwise it belongs to the enclosing FOR.
        if next_non_trivia(p, 0) == S::COLON {
            skip_trivia(p);
            p.bump();
            skip_trivia(p);
            parse_type_ref(p);
        }
        prm.complete(p, S::VALUE_PARAMETER);
        skip_trivia(p);
        if p.at(S::KW_IN) {
            p.bump();
            skip_trivia(p);
            // kotlinc wraps the iterable in LOOP_RANGE.
            let lr = p.start();
            parse_expression(p);
            lr.complete(p, S::LOOP_RANGE);
            skip_trivia(p);
        }
        if p.at(S::RPAR) {
            p.bump();
        }
        skip_trivia(p);
        parse_loop_body(p);
    }
    m.complete(p, S::FOR)
}

fn parse_while_statement(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // while
    skip_trivia(p);
    if p.at(S::LPAR) {
        // kotlinc PSI keeps LPAR / RPAR as siblings of CONDITION
        // inside WHILE; CONDITION wraps only the inner expression.
        p.bump();
        skip_trivia(p);
        let c = p.start();
        parse_expression(p);
        c.complete(p, S::CONDITION);
        skip_trivia(p);
        if p.at(S::RPAR) {
            p.bump();
        }
        skip_trivia(p);
        parse_loop_body(p);
    }
    m.complete(p, S::WHILE)
}

fn parse_do_while_statement(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // do
    skip_trivia(p);
    parse_loop_body(p);
    skip_trivia(p);
    if p.at(S::KW_WHILE) {
        p.bump();
        skip_trivia(p);
        if p.at(S::LPAR) {
            p.bump();
            skip_trivia(p);
            let c = p.start();
            parse_expression(p);
            c.complete(p, S::CONDITION);
            skip_trivia(p);
            if p.at(S::RPAR) {
                p.bump();
            }
        }
    }
    m.complete(p, S::DO_WHILE)
}

fn parse_try_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // try
    skip_trivia(p);
    if p.at(S::LBRACE) {
        parse_block(p);
    }
    // Only swallow trivia if more `catch`/`finally` follows.
    if matches!(next_non_trivia(p, 0), S::KW_CATCH | S::KW_FINALLY) {
        skip_trivia(p);
    }
    while p.at(S::KW_CATCH) {
        let c = p.start();
        p.bump();
        skip_trivia(p);
        if p.at(S::LPAR) {
            parse_value_parameter_list(p);
            skip_trivia(p);
        }
        if p.at(S::LBRACE) {
            parse_block(p);
        }
        c.complete(p, S::CATCH);
        if matches!(next_non_trivia(p, 0), S::KW_CATCH | S::KW_FINALLY) {
            skip_trivia(p);
        }
    }
    if p.at(S::KW_FINALLY) {
        let f = p.start();
        p.bump();
        skip_trivia(p);
        if p.at(S::LBRACE) {
            parse_block(p);
        }
        f.complete(p, S::FINALLY);
    }
    m.complete(p, S::TRY)
}

fn parse_return_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // return
    if p.at(S::AT) {
        p.bump();
        if p.at(S::IDENTIFIER) {
            p.bump();
        }
    }
    // A newline ends the return statement — `return` on its own line
    // is the bare-return form. Only consume inline WS before looking
    // for an expression.
    let nl = p.at(S::WHITE_SPACE) && p.current_text().contains('\n');
    skip_ws(p);
    if !nl && !is_stmt_terminator(p.current()) {
        parse_expression(p);
    }
    m.complete(p, S::RETURN)
}

fn parse_throw_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // throw
    skip_ws(p);
    parse_expression(p);
    m.complete(p, S::THROW)
}

fn parse_break_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // break
    if p.at(S::AT) {
        p.bump();
        if p.at(S::IDENTIFIER) {
            p.bump();
        }
    }
    m.complete(p, S::BREAK)
}

fn parse_continue_expression(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // continue
    if p.at(S::AT) {
        p.bump();
        if p.at(S::IDENTIFIER) {
            p.bump();
        }
    }
    m.complete(p, S::CONTINUE)
}

fn parse_object_literal(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // object
    skip_trivia(p);
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        loop {
            parse_super_type_entry(p);
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
            p.bump();
            skip_trivia(p);
        }
    }
    skip_trivia(p);
    if p.at(S::LBRACE) {
        parse_class_body(p);
    }
    m.complete(p, S::OBJECT_LITERAL)
}

fn is_stmt_terminator(k: SyntaxKind) -> bool {
    matches!(
        k,
        S::NEWLINE | S::SEMICOLON | S::RBRACE | S::RPAR | S::COMMA | S::EOF
    )
}

// ─── blocks ──────────────────────────────────────────────────────────────────

fn parse_block(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // {
    skip_trivia(p);
    parse_block_body(p);
    // parse_block_body leaves trailing trivia for the outer composite
    // — for a real BLOCK (delimited by `{` and `}`), that trivia
    // belongs INSIDE the block (between the last statement and `}`).
    skip_trivia(p);
    if p.at(S::RBRACE) {
        p.bump();
    }
    m.complete(p, S::BLOCK);
}

/// Parse `{ }` as a `LBRACE`, a possibly-empty `BLOCK`, then `RBRACE`.
/// Used in `if/else` and `when` branches where the block delimiter
/// `{` doesn't itself live inside the BLOCK — kotlinc PSI keeps LBRACE
/// outside the BLOCK and emits an empty BLOCK composite when the body
/// is `{ }`.
#[allow(dead_code)]
fn parse_brace_block_with_empty(_p: &mut Parser<'_, '_>) {
    // Placeholder for a future cleanup pass; right now we route
    // control-body braces through `parse_block` which keeps `{`
    // inside the BLOCK to remain consistent with kotlinc.
}

fn parse_block_body(p: &mut Parser<'_, '_>) {
    while !p.at(S::RBRACE) && !p.at(S::EOF) {
        // Block contains only trivia (a comment, whitespace) — leave
        // the trailing trivia to the outer composite and stop.
        if next_non_trivia(p, 0) == S::RBRACE {
            break;
        }
        parse_statement_or_expression(p);
        // Trailing trivia: leave it for the outer composite if it
        // sits before the closing `}`. Only consume trivia + the
        // following `;` if ANOTHER statement follows. This matches
        // kotlinc PSI where the trailing WS before `}` sits at
        // FUNCTION_LITERAL / outer level, not inside BLOCK.
        if next_non_trivia(p, 0) == S::RBRACE || p.at(S::EOF) {
            break;
        }
        skip_trivia(p);
        if p.at(S::SEMICOLON) {
            p.bump();
            skip_trivia(p);
        }
    }
}

fn parse_statement_or_expression(p: &mut Parser<'_, '_>) {
    skip_trivia(p);
    match p.current() {
        S::KW_VAL | S::KW_VAR => {
            let m = p.start();
            parse_property_body(p);
            m.complete(p, S::PROPERTY);
        }
        S::KW_FUN => {
            let m = p.start();
            parse_fun_body(p);
            m.complete(p, S::FUN);
        }
        S::KW_CLASS | S::KW_INTERFACE => {
            let m = p.start();
            parse_class_or_interface_body(p);
            m.complete(p, S::CLASS);
        }
        S::KW_OBJECT => {
            let m = p.start();
            parse_object_decl_body(p);
            m.complete(p, S::OBJECT_DECLARATION);
        }
        _ => {
            // Expression-statement. Modifier list won't apply here.
            let _ = parse_expression(p);
        }
    }
}

// ─── small helpers ───────────────────────────────────────────────────────────

fn next_non_trivia(p: &Parser<'_, '_>, mut offset: usize) -> SyntaxKind {
    loop {
        let k = p.nth(offset);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT
        ) {
            offset += 1;
            continue;
        }
        return k;
    }
}

/// Same as `next_non_trivia` but returns the absolute offset of the
/// first non-trivia token at or after `start`.
fn next_non_trivia_offset(p: &Parser<'_, '_>, mut offset: usize) -> usize {
    loop {
        let k = p.nth(offset);
        if matches!(
            k,
            S::WHITE_SPACE | S::NEWLINE | S::LINE_COMMENT | S::BLOCK_COMMENT
        ) {
            offset += 1;
            continue;
        }
        return offset;
    }
}

/// `true` if there's a newline (a `\n` inside WHITE_SPACE, or a
/// NEWLINE token) between the cursor and the first non-trivia token.
/// Used by infix/binary parsers to stop at a newline boundary —
/// Kotlin's grammar treats newlines as soft statement terminators for
/// infix operators like `in` and `is`.
fn has_newline_before_next_non_trivia(p: &Parser<'_, '_>) -> bool {
    let mut offset = 0usize;
    loop {
        let k = p.nth(offset);
        match k {
            S::NEWLINE => return true,
            S::WHITE_SPACE => {
                if p.text_at(offset).contains('\n') {
                    return true;
                }
                offset += 1;
            }
            S::LINE_COMMENT | S::BLOCK_COMMENT => {
                offset += 1;
            }
            _ => return false,
        }
    }
}

// ─── kw shim — keep the file readable ───────────────────────────────────────

// `pub`/`private` and friends already exist as named SyntaxKind
// variants. We added a single placeholder name `KW_PUBLIC_OR_UNUSED_KEEP_IDENT_OUT`
// purely to keep the `is_modifier_keyword` arm exhaustive against
// future additions. Until kotlin gets a `pub` keyword this is a
// no-op variant.
#[allow(non_upper_case_globals)]
const _ASSERTION_THAT_KW_PRIVATE_EXISTS: SyntaxKind = SyntaxKind::KW_PRIVATE;
