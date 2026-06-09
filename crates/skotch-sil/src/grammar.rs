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
    // Leading whitespace before the first content sits at FILE level
    // — kotlinc puts it directly under `kotlin.FILE`. A leading KDoc
    // OR a `@file:` annotation that's clearly file-level also sits
    // here.
    consume_list_level_trivia(p);
    if !p.at(S::EOF) {
        parse_optional_kdoc_then_file_annotations(p);
    }
    if p.at(S::KW_PACKAGE) {
        parse_package_directive(p);
    }
    consume_list_level_trivia(p);
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

/// Alias kept for readability where the intent is "rolling up trailing
/// trivia at a composite boundary" rather than "stepping past leading
/// trivia before grammar".
fn skip_trivia_collected(p: &mut Parser<'_, '_>) {
    skip_trivia(p);
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
    while p.at(S::AT) && next_non_trivia_is_file(p) {
        let m = p.start();
        p.bump(); // @
        skip_trivia(p);
        if p.at(S::KW_FILE) {
            p.bump();
            skip_trivia(p);
            if p.at(S::COLON) {
                p.bump();
                skip_trivia(p);
            }
        }
        // Eat the annotation reference and optional args.
        parse_user_type_chain(p);
        skip_trivia(p);
        if p.at(S::LPAR) {
            parse_value_argument_list(p);
        }
        m.complete(p, S::ANNOTATION_ENTRY);
        skip_trivia_collected(p);
    }
}

fn next_non_trivia_is_file(p: &Parser<'_, '_>) -> bool {
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
        return k == S::KW_FILE;
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
fn parse_qualified_name(p: &mut Parser<'_, '_>) {
    let mut lhs = parse_reference_expression(p);
    loop {
        // Don't skip whitespace — Kotlin's dotted names are not
        // newline-tolerant. A space after a name terminates the chain.
        if !p.at(S::DOT) {
            break;
        }
        // Look at what's after the `.` — must be an identifier or
        // soft-keyword to extend the chain. `.` followed by `*` or
        // any other non-identifier token belongs to the caller.
        let after_dot = next_non_trivia(p, 1);
        if !(after_dot == S::IDENTIFIER || is_soft_keyword(after_dot)) {
            break;
        }
        let dot_m = lhs.precede(p);
        p.bump(); // .
        skip_ws(p);
        let r = p.start();
        p.bump(); // identifier
        r.complete(p, S::REFERENCE_EXPRESSION);
        lhs = dot_m.complete(p, S::DOT_QUALIFIED_EXPRESSION);
    }
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

    // Modifier list + annotations are common decl prefix. A KDoc
    // immediately preceding the decl belongs to the decl, NOT to the
    // file (matches kotlinc PSI's `KDoc` placement under FUN/CLASS).
    let m = p.start();
    while p.at(S::KDOC) {
        p.bump();
        if p.at(S::WHITE_SPACE) {
            p.bump();
        }
    }
    let has_modifiers = parse_modifier_list_opt(p);
    skip_trivia(p);

    let kind = match p.current() {
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
                parse_class_or_interface_body(p);
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
    k == S::AT
        || is_modifier_keyword(k)
        || is_soft_modifier_keyword(k)
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
    matches!(
        k,
        S::KW_DATA | S::KW_SEALED | S::KW_COMPANION | S::KW_ENUM
    )
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
        ) || is_modifier_keyword(k)
            || is_soft_modifier_keyword(k)
            || k == S::AT;
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
    )
}

fn parse_annotation_entry(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // @
    if matches!(
        p.current(),
        S::KW_FILE | S::KW_FIELD | S::KW_GET | S::KW_SET | S::KW_PARAM | S::KW_PROPERTY | S::KW_RECEIVER
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
    if p.at(S::LPAR) {
        // Primary constructor.
        let pc = p.start();
        parse_value_parameter_list(p);
        pc.complete(p, S::PRIMARY_CONSTRUCTOR);
        skip_trivia_if_class_continues(p);
    }
    if p.at(S::COLON) {
        let stl = p.start();
        p.bump(); // :
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
    parse_type_ref(p);
    skip_trivia(p);
    if p.at(S::LPAR) {
        parse_value_argument_list(p);
        m.complete(p, S::SUPER_TYPE_CALL_ENTRY);
    } else if p.at(S::KW_BY) {
        p.bump();
        skip_trivia(p);
        parse_expression(p);
        m.complete(p, S::DELEGATED_SUPER_TYPE_ENTRY);
    } else {
        m.complete(p, S::SUPER_TYPE_ENTRY);
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
    let m = p.start();
    p.bump(); // {
    skip_trivia(p);
    while !p.at(S::RBRACE) && !p.at(S::EOF) {
        parse_class_member(p);
        skip_trivia_collected(p);
    }
    if p.at(S::RBRACE) {
        p.bump();
    }
    m.complete(p, S::CLASS_BODY);
}

fn parse_class_member(p: &mut Parser<'_, '_>) {
    skip_trivia_collected(p);
    if p.at(S::SEMICOLON) || p.at(S::COMMA) {
        p.bump();
        return;
    }
    let m = p.start();
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

fn parse_object_decl_body(p: &mut Parser<'_, '_>) {
    p.bump(); // object
    skip_trivia(p);
    if p.at(S::IDENTIFIER) {
        p.bump();
    }
    skip_trivia(p);
    if p.at(S::COLON) {
        let stl = p.start();
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
        skip_trivia(p);
    }
    if p.at(S::COLON) {
        p.bump();
        skip_trivia(p);
        parse_type_ref(p);
        skip_trivia(p);
    }
    if p.at(S::KW_WHERE) {
        parse_type_constraint_list(p);
        skip_trivia(p);
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
        // `: this(...)` or `: super(...)`
        if matches!(p.current(), S::KW_THIS | S::KW_SUPER) {
            p.bump();
            skip_trivia(p);
            if p.at(S::LPAR) {
                parse_value_argument_list(p);
            }
            skip_trivia(p);
        }
    }
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
    if has_receiver_prefix(p) {
        parse_receiver_then_name(p);
    } else if p.at(S::IDENTIFIER) || is_soft_keyword(p.current()) {
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
        // Only swallow more trivia if the property continues (`by` or
        // accessors). Otherwise leave it for the outer composite.
        if next_non_trivia_property_continues(p) {
            skip_trivia(p);
        }
    }
    if p.at(S::KW_BY) {
        p.bump();
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
    let k = next_non_trivia(p, 0);
    matches!(k, S::KW_BY | S::KW_GET | S::KW_SET)
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
        return matches!(k, S::KW_GET | S::KW_SET);
    }
}

fn parse_property_accessor(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(); // get / set
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
    parse_modifier_list_opt(p);
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

fn parse_type_ref(p: &mut Parser<'_, '_>) {
    let m = p.start();
    parse_modifier_list_opt(p);
    skip_trivia(p);
    // Function type: `(A, B) -> C` or `T.(A) -> C`.
    if p.at(S::LPAR) && looks_like_function_type(p) {
        parse_function_type_after_modifiers(p);
        m.complete(p, S::TYPE_REFERENCE);
        return;
    }
    // User type or nullable user type.
    let inner = p.start();
    parse_user_type_chain(p);
    inner.complete(p, S::USER_TYPE);
    // Only consume WS if a nullable `?` follows; otherwise the
    // trailing WS belongs to the outer composite.
    while next_non_trivia(p, 0) == S::QUEST {
        skip_ws(p);
        let q = p.start();
        p.bump();
        q.complete(p, S::NULLABLE_TYPE);
    }
    m.complete(p, S::TYPE_REFERENCE);
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

fn parse_user_type_chain(p: &mut Parser<'_, '_>) {
    parse_user_type_segment(p);
    loop {
        // Only consume WS if a DOT actually follows — otherwise the
        // trailing whitespace belongs to the OUTER composite, not to
        // USER_TYPE. (kotlinc PSI: USER_TYPE never has trailing WS.)
        if next_non_trivia(p, 0) != S::DOT {
            break;
        }
        skip_ws(p);
        p.bump(); // .
        skip_ws(p);
        parse_user_type_segment(p);
    }
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

/// Consume trivia at the *list* level — single WS run plus any
/// embedded line/block comments that don't introduce a new
/// declaration. KDoc is left alone (it belongs to the next param).
fn consume_list_level_trivia(p: &mut Parser<'_, '_>) {
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
    parse_modifier_list_opt(p);
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
        if !matches!(
            next_non_trivia(p, 0),
            S::LT | S::GT | S::LTEQ | S::GTEQ
        ) {
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
        skip_ws(p);
        let m = lhs.precede(p);
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
        break;
    }
    lhs
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
        if !matches!(
            next_non_trivia(p, 0),
            S::MUL | S::DIV | S::PERC
        ) {
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
        p.bump();
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
        if matches!(p.current(), S::WHITE_SPACE)
            && !p.current_text().contains('\n')
            && is_postfix_starter(p.nth(1))
        {
            skip_ws(p);
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
                let r = p.start();
                if p.at(S::IDENTIFIER) || p.at(S::KW_CLASS) {
                    p.bump();
                }
                r.complete(p, S::REFERENCE_EXPRESSION);
                lhs = m.complete(p, S::CALLABLE_REFERENCE_EXPRESSION);
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
/// CALL_EXPRESSION inside the qualified expression.
fn is_call_rhs(p: &Parser<'_, '_>) -> bool {
    if !(p.at(S::IDENTIFIER) || is_soft_keyword(p.current())) {
        return false;
    }
    let mut i = 1;
    while matches!(p.nth(i), S::WHITE_SPACE) {
        i += 1;
    }
    matches!(p.nth(i), S::LPAR | S::LT | S::LBRACE)
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
                        return matches!(k2, S::LPAR | S::DOT | S::COLONCOLON);
                    }
                }
            }
            S::IDENTIFIER | S::COMMA | S::DOT | S::QUEST | S::MUL | S::WHITE_SPACE | S::NEWLINE => {}
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

fn parse_atom(p: &mut Parser<'_, '_>) -> CompletedMarker {
    skip_trivia(p);
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
            p.bump();
            m.complete(p, S::THIS_EXPRESSION)
        }
        S::KW_SUPER => {
            let m = p.start();
            p.bump();
            m.complete(p, S::SUPER_EXPRESSION)
        }
        S::KW_OBJECT => parse_object_literal(p),
        S::AT => {
            // Label or annotation: just consume `@ident` and continue.
            let m = p.start();
            p.bump();
            if p.at(S::IDENTIFIER) {
                p.bump();
            }
            m.complete(p, S::REFERENCE_EXPRESSION)
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
    p.bump(); // STRING_START
    loop {
        match p.current() {
            S::STRING_END => {
                p.bump();
                break;
            }
            S::EOF => break,
            S::STRING_CHUNK => {
                // A backslash-prefixed chunk is an escape sequence;
                // wrap it as ESCAPE_STRING_TEMPLATE_ENTRY with the
                // token reclassified as ESCAPE_SEQUENCE.
                if p.current_text().starts_with('\\') {
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
            skip_trivia(p);
            if p.at(S::COLON) {
                p.bump();
                skip_trivia(p);
                parse_type_ref(p);
                skip_trivia(p);
            }
            prm.complete(p, S::VALUE_PARAMETER);
            skip_trivia(p);
            if !p.at(S::COMMA) {
                break;
            }
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
    // Lambda body is wrapped in a BLOCK composite. The trailing WS
    // before `}` sits at FUNCTION_LITERAL level, not inside BLOCK.
    if !p.at(S::RBRACE) {
        let block = p.start();
        parse_block_body(p);
        block.complete(p, S::BLOCK);
    }
    if p.at(S::WHITE_SPACE) {
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
        parse_statement_or_expression(p);
        then.complete(p, S::THEN);
    }
    if next_non_trivia(p, 0) == S::KW_ELSE {
        skip_trivia(p);
        p.bump(); // else
        skip_trivia(p);
        let el = p.start();
        parse_statement_or_expression(p);
        el.complete(p, S::ELSE);
    }
    m.complete(p, S::IF)
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
            skip_trivia(p);
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
        parse_statement_or_expression(p);
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
        skip_trivia(p);
        if p.at(S::COLON) {
            p.bump();
            skip_trivia(p);
            parse_type_ref(p);
        }
        prm.complete(p, S::VALUE_PARAMETER);
        skip_trivia(p);
        if p.at(S::KW_IN) {
            p.bump();
            skip_trivia(p);
            parse_expression(p);
            skip_trivia(p);
        }
        if p.at(S::RPAR) {
            p.bump();
        }
        skip_trivia(p);
        parse_statement_or_expression(p);
    }
    m.complete(p, S::FOR)
}

fn parse_while_statement(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // while
    skip_trivia(p);
    if p.at(S::LPAR) {
        let c = p.start();
        p.bump();
        skip_trivia(p);
        parse_expression(p);
        skip_trivia(p);
        if p.at(S::RPAR) {
            p.bump();
        }
        c.complete(p, S::CONDITION);
        skip_trivia(p);
        parse_statement_or_expression(p);
    }
    m.complete(p, S::WHILE)
}

fn parse_do_while_statement(p: &mut Parser<'_, '_>) -> CompletedMarker {
    let m = p.start();
    p.bump(); // do
    skip_trivia(p);
    parse_statement_or_expression(p);
    skip_trivia(p);
    if p.at(S::KW_WHILE) {
        p.bump();
        skip_trivia(p);
        if p.at(S::LPAR) {
            let c = p.start();
            p.bump();
            skip_trivia(p);
            parse_expression(p);
            skip_trivia(p);
            if p.at(S::RPAR) {
                p.bump();
            }
            c.complete(p, S::CONDITION);
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
    if matches!(
        next_non_trivia(p, 0),
        S::KW_CATCH | S::KW_FINALLY
    ) {
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
        if matches!(
            next_non_trivia(p, 0),
            S::KW_CATCH | S::KW_FINALLY
        ) {
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
    skip_ws(p);
    if !is_stmt_terminator(p.current()) {
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

fn parse_block_body(p: &mut Parser<'_, '_>) {
    while !p.at(S::RBRACE) && !p.at(S::EOF) {
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

// ─── kw shim — keep the file readable ───────────────────────────────────────

// `pub`/`private` and friends already exist as named SyntaxKind
// variants. We added a single placeholder name `KW_PUBLIC_OR_UNUSED_KEEP_IDENT_OUT`
// purely to keep the `is_modifier_keyword` arm exhaustive against
// future additions. Until kotlin gets a `pub` keyword this is a
// no-op variant.
#[allow(non_upper_case_globals)]
const _ASSERTION_THAT_KW_PRIVATE_EXISTS: SyntaxKind = SyntaxKind::KW_PRIVATE;
