//! Hand-rolled recursive-descent parser for the Kotlin 2 subset skotch
//! currently accepts.
//!
//! ## Why hand-rolled (not chumsky)?
//!
//! The original architectural plan named `chumsky` 0.10 as the parser
//! library. We deferred that swap to a later PR for two reasons:
//!
//! 1. Chumsky 0.10 has a non-trivial learning curve and a fluid API; for
//!    the ~10 fixture-driven productions we started with, a hand-rolled
//!    parser is faster to write and easier to read.
//! 2. Hand-rolled keeps the dependency graph small while we're still
//!    iterating on the AST shape (`skotch-syntax`). Once the AST stabilizes
//!    we can swap parsers without changing any consumer.
//!
//! The public API ([`parse_file`]) takes a [`LexedFile`] and returns a
//! [`KtFile`] AST plus diagnostics. Errors are recorded in the
//! diagnostics sink and the parser does best-effort recovery so a single
//! syntax error doesn't blow up the entire file.
//!
//! ## Newline handling
//!
//! Kotlin's grammar is newline-sensitive, but the initial fixtures all
//! avoid newline-ambiguous constructs. The parser therefore treats
//! `Newline` and `Semi` tokens as universal statement separators and
//! skips them everywhere else. Future PRs that add fixtures like
//! ```text
//! val x = 1
//!   + 2  // continuation?
//! ```
//! will need to revisit this.

use skotch_diagnostics::{Diagnostic, Diagnostics};
use skotch_intern::{Interner, Symbol};
use skotch_lexer::{LexedFile, TokenPayload};
use skotch_span::{FileId, Span};
use skotch_syntax::{
    BinOp, Block, CallArg, ClassDecl, ConstructorParam, Decl, EnumDecl, Expr, FunDecl, ImportDecl,
    InterfaceDecl, KtFile, ObjectDecl, PackageDecl, Param, PropertyDecl, SecondaryConstructor,
    Stmt, SuperClassRef, TemplatePart, Token, TokenKind, TypeAliasDecl, TypeParam, TypeRef,
    UnaryOp, ValDecl, Visibility, WhenBranch,
};

/// Parse a lexed file into an AST. The lexer's `LexedFile` is consumed
/// only by reference; the parser also takes a mutable [`Interner`] to
/// produce [`Symbol`] handles for identifiers, and a [`Diagnostics`]
/// sink for parse errors.
pub fn parse_file(lexed: &LexedFile, interner: &mut Interner, diags: &mut Diagnostics) -> KtFile {
    let mut p = Parser {
        file: lexed.file,
        tokens: &lexed.tokens,
        payloads: &lexed.payloads,
        pos: 0,
        interner,
        diags,
    };
    p.parse_file()
}

/// Parse a source file via the SIL (Source Information Layer)
/// pipeline — kotlinc-PSI-shaped lossless concrete syntax tree.
///
/// This is the path forward for the AST migration. Returns a
/// [`skotch_sil::SilTree`] that consumers wrap with the typed
/// [`skotch_ast`] accessors. The legacy [`parse_file`] above remains
/// available; consumers migrate one at a time.
///
/// `file_name` is the display path written into the SIL tree's
/// `file:` field (used by YAML emit and diagnostics).
pub fn parse_to_sil(file_name: &str, source: &str) -> skotch_sil::SilTree {
    skotch_sil::parse_sil(file_name, source)
}

struct Parser<'a> {
    file: FileId,
    tokens: &'a [Token],
    payloads: &'a [Option<TokenPayload>],
    pos: usize,
    interner: &'a mut Interner,
    diags: &'a mut Diagnostics,
}

impl<'a> Parser<'a> {
    // ─── token plumbing ──────────────────────────────────────────────────

    fn peek_kind(&self) -> TokenKind {
        if self.pos >= self.tokens.len() {
            return TokenKind::Eof;
        }
        self.tokens[self.pos].kind
    }

    /// Returns true if the token at `pos + offset` can serve as an
    /// identifier — either a real `Ident` or a Kotlin soft keyword
    /// (`data`, `open`, `sealed`, etc.) that is valid as a name in
    /// expression/argument context.
    fn is_name_token_at(&self, offset: usize) -> bool {
        matches!(
            self.peek_kind_at(offset),
            TokenKind::Ident
                | TokenKind::KwData
                | TokenKind::KwEnum
                | TokenKind::KwSealed
                | TokenKind::KwOpen
                | TokenKind::KwAbstract
                | TokenKind::KwOverride
                | TokenKind::KwInline
                | TokenKind::KwOperator
                | TokenKind::KwInfix
                | TokenKind::KwSuspend
                | TokenKind::KwLateinit
                | TokenKind::KwTailrec
                | TokenKind::KwVararg
                | TokenKind::KwConst
                | TokenKind::KwConstructor
                | TokenKind::KwInit
        )
    }

    #[allow(dead_code)]
    fn peek_kind_at(&self, offset: usize) -> TokenKind {
        self.tokens
            .get(self.pos + offset)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }

    fn peek_span(&self) -> Span {
        if self.pos >= self.tokens.len() {
            return Span::new(FileId(0), 0, 0);
        }
        self.tokens[self.pos].span
    }

    fn payload(&self, index: usize) -> Option<&TokenPayload> {
        self.payloads.get(index).and_then(|p| p.as_ref())
    }

    /// Skip universal trivia: newlines and semicolons. Used between
    /// statements and almost everywhere else.
    fn skip_trivia(&mut self) {
        while matches!(self.peek_kind(), TokenKind::Newline | TokenKind::Semi) {
            self.pos += 1;
        }
    }

    /// Skip annotation(s) like `@Suppress("unused")`, `@JvmStatic`,
    /// `@field:JvmField`, etc. Annotations don't affect codegen yet.
    /// Parse annotations and return them. Replaces the old skip_annotations.
    fn parse_annotations(&mut self) -> Vec<skotch_syntax::Annotation> {
        let mut annotations = Vec::new();
        while self.peek_kind() == TokenKind::At {
            let start = self.peek_span();
            self.bump(); // consume '@'

            // Parse annotation name.
            if self.peek_kind() != TokenKind::Ident {
                self.skip_trivia();
                continue;
            }
            let name_idx = self.pos;
            self.bump();
            let mut name = self.intern_ident_at(name_idx);
            let mut target = None;

            // Handle use-site target: `@field:JvmField` — colon + ident.
            if self.peek_kind() == TokenKind::Colon && self.peek_kind_at(1) == TokenKind::Ident {
                target = Some(name); // "field" is the target
                self.bump(); // consume ':'
                let actual_name_idx = self.pos;
                self.bump(); // consume actual annotation name
                name = self.intern_ident_at(actual_name_idx);
            }

            // Parse optional arguments.
            let mut args = Vec::new();
            if self.peek_kind() == TokenKind::LParen {
                self.bump(); // consume '('
                while self.peek_kind() != TokenKind::RParen && self.peek_kind() != TokenKind::Eof {
                    self.skip_trivia();
                    let arg = self.parse_annotation_arg();
                    args.push(arg);
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::Comma {
                        self.bump();
                    }
                }
                if self.peek_kind() == TokenKind::RParen {
                    self.bump();
                }
            }

            let end = self.peek_span();
            annotations.push(skotch_syntax::Annotation {
                name,
                target,
                args,
                span: start.merge(end),
            });
            self.skip_trivia();
        }
        annotations
    }

    /// Parse a single annotation argument value.
    fn parse_annotation_arg(&mut self) -> skotch_syntax::AnnotationArg {
        match self.peek_kind() {
            TokenKind::StringStart => {
                // Consume StringStart, collect StringChunk content, consume StringEnd.
                self.bump(); // consume StringStart
                let mut content = String::new();
                while self.peek_kind() != TokenKind::StringEnd && self.peek_kind() != TokenKind::Eof
                {
                    let idx = self.pos;
                    self.bump();
                    if let Some(TokenPayload::StringChunk(s)) = self.payload(idx) {
                        content.push_str(s);
                    }
                }
                if self.peek_kind() == TokenKind::StringEnd {
                    self.bump();
                }
                skotch_syntax::AnnotationArg::StringLit(content)
            }
            TokenKind::IntLit => {
                let idx = self.pos;
                self.bump();
                let v = match self.payload(idx) {
                    Some(TokenPayload::Int(v)) => *v,
                    _ => 0,
                };
                skotch_syntax::AnnotationArg::IntLit(v)
            }
            TokenKind::KwTrue => {
                self.bump();
                skotch_syntax::AnnotationArg::BoolLit(true)
            }
            TokenKind::KwFalse => {
                self.bump();
                skotch_syntax::AnnotationArg::BoolLit(false)
            }
            TokenKind::LBracket => {
                self.bump(); // consume '['
                let mut items = Vec::new();
                while self.peek_kind() != TokenKind::RBracket && self.peek_kind() != TokenKind::Eof
                {
                    self.skip_trivia();
                    items.push(self.parse_annotation_arg());
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::Comma {
                        self.bump();
                    }
                }
                if self.peek_kind() == TokenKind::RBracket {
                    self.bump();
                }
                skotch_syntax::AnnotationArg::Array(items)
            }
            TokenKind::Ident => {
                // Could be a simple ident, a qualified name (Foo.BAR), or
                // a named argument (name = value).
                let idx = self.pos;
                self.bump();
                let sym = self.intern_ident_at(idx);

                // Check for qualified name: `AnnotationTarget.CLASS`
                if self.peek_kind() == TokenKind::Dot {
                    let mut parts = vec![sym];
                    while self.peek_kind() == TokenKind::Dot {
                        self.bump(); // consume '.'
                        if self.peek_kind() == TokenKind::Ident {
                            let next_idx = self.pos;
                            self.bump();
                            parts.push(self.intern_ident_at(next_idx));
                        }
                    }
                    skotch_syntax::AnnotationArg::QualifiedName(parts)
                } else if self.peek_kind() == TokenKind::ColonColon {
                    // `Foo::class` — class literal reference. Skip `::class`.
                    self.bump(); // consume `::`
                    if self.peek_kind() == TokenKind::KwClass {
                        self.bump(); // consume `class`
                    }
                    skotch_syntax::AnnotationArg::Ident(sym)
                } else if self.peek_kind() == TokenKind::Eq {
                    // Named argument: `name = "value"` — skip the name, parse value.
                    self.bump(); // consume '='
                    self.skip_trivia();
                    self.parse_annotation_arg()
                } else if self.peek_kind() == TokenKind::LParen {
                    // Nested annotation-style call inside the args
                    // list of an outer annotation: e.g. `@Deprecated(
                    // "old", ReplaceWith("new", "package"))`. We
                    // record the head name and skip the balanced
                    // parens — the AST doesn't model nested calls,
                    // but consuming them keeps the outer annotation
                    // parse on the rails.
                    self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                    skotch_syntax::AnnotationArg::Ident(sym)
                } else {
                    skotch_syntax::AnnotationArg::Ident(sym)
                }
            }
            _ => {
                // Unknown argument — skip token.
                self.bump();
                skotch_syntax::AnnotationArg::StringLit(String::new())
            }
        }
    }

    /// Backward compatibility: skip annotations without capturing them.
    /// Used in contexts where annotations are not needed (class members, etc).
    fn skip_annotations(&mut self) {
        let _ = self.parse_annotations();
    }

    /// Consume one token regardless of kind.
    fn bump(&mut self) -> Token {
        if self.pos >= self.tokens.len() {
            return Token::new(TokenKind::Eof, Span::new(FileId(0), 0, 0));
        }
        let t = self.tokens[self.pos];
        self.pos += 1;
        t
    }

    /// If the next non-trivia token is `kind`, consume it and return its
    /// span; otherwise emit an error and return the current span.
    fn expect(&mut self, kind: TokenKind, what: &str) -> Span {
        self.skip_trivia();
        if self.peek_kind() == kind {
            let t = self.bump();
            t.span
        } else {
            let span = self.peek_span();
            self.diags.push(Diagnostic::error(
                span,
                format!("expected {what}, found {:?}", self.peek_kind()),
            ));
            span
        }
    }

    /// Consume the next token if it matches `kind`. Returns true on hit.
    fn eat(&mut self, kind: TokenKind) -> bool {
        self.skip_trivia();
        if self.peek_kind() == kind {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Skip past balanced delimiters (e.g. `(...)`, `{...}`).
    fn skip_balanced(&mut self, open: TokenKind, close: TokenKind) {
        if self.peek_kind() != open {
            return;
        }
        self.bump(); // consume open
        let mut depth = 1u32;
        while depth > 0 && self.peek_kind() != TokenKind::Eof {
            if self.peek_kind() == open {
                depth += 1;
            } else if self.peek_kind() == close {
                depth -= 1;
            }
            self.bump();
        }
    }

    /// Skip a generic argument list on a supertype identifier, e.g.
    /// `: Comparable<Money>` or `: Map<String, List<Int>>`. The class
    /// model doesn't carry supertype type args yet — this just consumes
    /// the tokens so the rest of the class header parses cleanly. Safe
    /// no-op when no `<` is present.
    fn skip_supertype_generic_args(&mut self) {
        if self.peek_kind() != TokenKind::Lt {
            return;
        }
        self.bump(); // consume `<`
        let mut depth = 1u32;
        while depth > 0 && self.peek_kind() != TokenKind::Eof {
            match self.peek_kind() {
                TokenKind::Lt => depth += 1,
                TokenKind::Gt => depth -= 1,
                _ => {}
            }
            self.bump();
        }
        self.skip_trivia();
    }

    /// Parse a simple supertype clause `: Parent(args)?, Iface1, Iface2, ...`
    /// for declaration kinds (object/enum/interface) that don't support
    /// interface delegation. Returns `(parent_class, interfaces)`. Both
    /// are empty when no `:` is present. The first supertype may be a
    /// concrete class (recognized by a trailing `(args)`); all later
    /// supertypes are interfaces. For `interface` declarations, every
    /// supertype is an interface — the caller passes `allow_parent_class:
    /// false` and the parser treats a leading `Foo(...)` clause as an
    /// error rather than silently producing a `parent_class`.
    fn parse_simple_supertype_clause(
        &mut self,
        allow_parent_class: bool,
    ) -> (Option<SuperClassRef>, Vec<Symbol>) {
        let mut parent_class: Option<SuperClassRef> = None;
        let mut interfaces: Vec<Symbol> = Vec::new();
        if self.peek_kind() != TokenKind::Colon {
            return (parent_class, interfaces);
        }
        self.bump(); // consume ':'
        let mut first = true;
        loop {
            self.skip_trivia();
            if self.peek_kind() != TokenKind::Ident {
                break;
            }
            let name_idx = self.pos;
            let name_span = self.peek_span();
            self.bump();
            let supertype_name = self.intern_ident_at(name_idx);
            self.skip_trivia();
            // Optional generic args — `: Comparable<Money>` etc.
            self.skip_supertype_generic_args();
            if first && self.peek_kind() == TokenKind::LParen {
                if !allow_parent_class {
                    self.diags.push(Diagnostic::error(
                        name_span,
                        "interfaces cannot extend a class — only other interfaces",
                    ));
                }
                self.bump(); // consume '('
                self.skip_trivia();
                let mut args = Vec::new();
                if self.peek_kind() != TokenKind::RParen {
                    loop {
                        self.skip_trivia();
                        let expr = self.parse_expr();
                        args.push(CallArg { name: None, expr });
                        self.skip_trivia();
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RParen, ")");
                self.skip_trivia();
                if allow_parent_class {
                    parent_class = Some(SuperClassRef {
                        name: supertype_name,
                        name_span,
                        args,
                    });
                }
            } else {
                interfaces.push(supertype_name);
            }
            first = false;
            self.skip_trivia();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.skip_trivia();
        (parent_class, interfaces)
    }

    /// Get the string content of an Ident token at a given index.
    fn lexeme_str(&self, idx: usize) -> &str {
        match self.payload(idx) {
            Some(TokenPayload::Ident(s)) => s.as_str(),
            _ => "",
        }
    }

    fn intern_ident_at(&mut self, idx: usize) -> Symbol {
        // Clone the string out of the payload table so we can release
        // the immutable borrow before mutating the interner.
        let s = match self.payload(idx) {
            Some(TokenPayload::Ident(s)) => s.clone(),
            _ => {
                // Keyword tokens don't carry an Ident payload — derive the
                // text from the token kind (e.g. KwData → "data").
                self.tokens[idx]
                    .kind
                    .keyword_text()
                    .unwrap_or("")
                    .to_string()
            }
        };
        self.interner.intern(&s)
    }

    // ─── grammar ─────────────────────────────────────────────────────────

    fn parse_file(&mut self) -> KtFile {
        let mut package = None;
        let mut imports = Vec::new();
        let mut decls = Vec::new();

        // Skip file-level annotations: @file:OptIn(...), @file:Suppress(...)
        // These appear before the package declaration and are metadata only.
        self.skip_trivia();
        while self.peek_kind() == TokenKind::At {
            let saved = self.pos;
            self.bump(); // consume '@'
            if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "file" {
                self.bump(); // consume 'file'
                if self.peek_kind() == TokenKind::Colon {
                    self.bump(); // consume ':'
                                 // Skip the annotation name and arguments.
                    if self.peek_kind() == TokenKind::Ident || self.peek_kind().is_keyword() {
                        self.bump(); // annotation name
                    }
                    if self.peek_kind() == TokenKind::LParen {
                        self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                    }
                    self.skip_trivia();
                    continue;
                }
            }
            // Not a @file: annotation — restore position.
            self.pos = saved;
            break;
        }

        self.skip_trivia();
        if self.peek_kind() == TokenKind::KwPackage {
            package = Some(self.parse_package());
        }
        loop {
            self.skip_trivia();
            if self.peek_kind() != TokenKind::KwImport {
                break;
            }
            imports.push(self.parse_import());
        }
        loop {
            self.skip_trivia();
            if self.peek_kind() == TokenKind::Eof {
                break;
            }
            // Parse annotations before declarations.
            let annotations = self.parse_annotations();
            // Skip modifier keywords that we recognize but don't enforce.
            let mut is_data = false;
            let mut is_enum = false;
            let mut is_open = false;
            let mut is_abstract = false;
            let mut is_sealed = false;
            let mut is_suspend = false;
            let mut is_inline = false;
            let mut is_annotation_class = false;
            let mut is_value_class = false;
            let mut is_const = false;
            let mut visibility = Visibility::Public;
            // Unified modifier loop. Accepts Kotlin modifier keywords
            // (KwOpen / KwAbstract / KwSealed / KwPrivate / etc.) AND
            // the soft modifiers the lexer emits as plain Ident
            // tokens: `public`, `final`, `annotation class`,
            // `value class`. Modifiers may appear in any order (e.g.
            // `internal value class Foo` or `public abstract class
            // Bar`) — that's why this is one loop instead of three.
            loop {
                let kind = self.peek_kind();
                if matches!(
                    kind,
                    TokenKind::KwConst
                        | TokenKind::KwOpen
                        | TokenKind::KwAbstract
                        | TokenKind::KwSealed
                        | TokenKind::KwInfix
                        | TokenKind::KwInline
                        | TokenKind::KwPrivate
                        | TokenKind::KwProtected
                        | TokenKind::KwInternal
                        | TokenKind::KwOverride
                        | TokenKind::KwData
                        | TokenKind::KwEnum
                        | TokenKind::KwOperator
                        | TokenKind::KwSuspend
                        | TokenKind::KwTailrec
                ) {
                    match kind {
                        TokenKind::KwData => is_data = true,
                        TokenKind::KwEnum => is_enum = true,
                        TokenKind::KwOpen => is_open = true,
                        TokenKind::KwAbstract => is_abstract = true,
                        TokenKind::KwSealed => is_sealed = true,
                        TokenKind::KwSuspend => is_suspend = true,
                        TokenKind::KwInline => is_inline = true,
                        TokenKind::KwConst => is_const = true,
                        TokenKind::KwPrivate => visibility = Visibility::Private,
                        TokenKind::KwProtected => visibility = Visibility::Protected,
                        TokenKind::KwInternal => visibility = Visibility::Internal,
                        _ => {}
                    }
                    self.bump();
                    self.skip_trivia();
                    continue;
                }
                if kind == TokenKind::Ident {
                    let kw = self.lexeme_str(self.pos).to_string();
                    if kw == "annotation" && self.peek_kind_at(1) == TokenKind::KwClass {
                        is_annotation_class = true;
                        self.bump();
                        self.skip_trivia();
                        continue;
                    }
                    if kw == "value" && self.peek_kind_at(1) == TokenKind::KwClass {
                        is_value_class = true;
                        self.bump();
                        self.skip_trivia();
                        continue;
                    }
                    if kw == "public" || kw == "final" {
                        self.bump();
                        self.skip_trivia();
                        continue;
                    }
                }
                break;
            }
            match self.peek_kind() {
                TokenKind::KwFun => {
                    let mut f = self.parse_fun_decl();
                    f.is_open = is_open;
                    f.is_override = false;
                    f.is_abstract = is_abstract;
                    f.is_suspend = is_suspend;
                    f.is_inline = is_inline;
                    f.visibility = visibility;
                    f.annotations = annotations.clone();
                    decls.push(Decl::Fun(f));
                }
                TokenKind::KwVal | TokenKind::KwVar => {
                    if self.peek_kind_at(2) == TokenKind::Dot {
                        let mut f = self.parse_extension_property();
                        f.annotations = annotations.clone();
                        decls.push(Decl::Fun(f));
                    } else {
                        let mut v = self.parse_val_decl();
                        v.visibility = visibility;
                        v.is_const = is_const;
                        v.annotations = annotations.clone();
                        decls.push(Decl::Val(v));
                    }
                }
                TokenKind::KwClass => {
                    if is_enum {
                        decls.push(Decl::Enum(self.parse_enum_decl()));
                    } else if is_annotation_class {
                        // `annotation class MyAnnotation(val msg: String)` →
                        // parse as class, mark as abstract+interface in MIR.
                        let mut cd = self.parse_class_decl();
                        cd.is_abstract = true;
                        cd.visibility = visibility;
                        cd.annotations = annotations.clone();
                        // Add a synthetic annotation to mark it as an annotation class.
                        cd.annotations.push(skotch_syntax::Annotation {
                            name: self.interner.intern("AnnotationClass"),
                            target: None,
                            args: Vec::new(),
                            span: cd.span,
                        });
                        decls.push(Decl::Class(cd));
                    } else {
                        let mut cd = self.parse_class_decl();
                        cd.is_data = is_data || is_value_class;
                        cd.is_open = is_open || is_sealed;
                        cd.is_abstract = is_abstract || is_sealed;
                        cd.is_sealed = is_sealed;
                        cd.visibility = visibility;
                        cd.annotations = annotations.clone();
                        decls.push(Decl::Class(cd));
                    }
                }
                TokenKind::KwInterface => {
                    decls.push(Decl::Interface(self.parse_interface_decl()));
                }
                TokenKind::KwObject => {
                    decls.push(Decl::Object(self.parse_object_decl()));
                }
                TokenKind::Ident if self.lexeme_str(self.pos) == "typealias" => {
                    decls.push(Decl::TypeAlias(self.parse_typealias_decl()));
                }
                _ => {
                    let span = self.peek_span();
                    self.diags.push(Diagnostic::error(
                        span,
                        format!("unexpected token {:?} at top level", self.peek_kind()),
                    ));
                    self.bump();
                }
            }
        }

        KtFile {
            file: self.file,
            package,
            imports,
            decls,
        }
    }

    #[allow(dead_code)]
    fn recover_to_top_level(&mut self) {
        // Skip ahead to the next `fun`/`val`/`var`/`class`/`object` or EOF.
        while !matches!(
            self.peek_kind(),
            TokenKind::KwFun
                | TokenKind::KwVal
                | TokenKind::KwVar
                | TokenKind::KwClass
                | TokenKind::KwObject
                | TokenKind::Eof
        ) {
            self.bump();
        }
    }

    fn parse_package(&mut self) -> PackageDecl {
        let kw = self.expect(TokenKind::KwPackage, "package");
        let mut path = Vec::new();
        let mut end_span = kw;
        loop {
            self.skip_trivia();
            // Accept keywords as package path segments (e.g. `package com.example.data`
            // where `data` is a soft keyword).
            if self.peek_kind() != TokenKind::Ident && !self.peek_kind().is_keyword() {
                self.diags.push(Diagnostic::error(
                    self.peek_span(),
                    "expected identifier in package path",
                ));
                break;
            }
            let idx = self.pos;
            let span = self.peek_span();
            self.bump();
            path.push(self.intern_ident_at(idx));
            end_span = span;
            if !self.eat(TokenKind::Dot) {
                break;
            }
        }
        PackageDecl {
            path,
            span: kw.merge(end_span),
        }
    }

    fn parse_import(&mut self) -> ImportDecl {
        let kw = self.expect(TokenKind::KwImport, "import");
        let mut path = Vec::new();
        let mut end_span = kw;
        let mut is_wildcard = false;
        loop {
            self.skip_trivia();
            // Check for wildcard `*`
            if self.peek_kind() == TokenKind::Star {
                self.bump();
                is_wildcard = true;
                break;
            }
            // Accept identifiers AND keywords as import path segments.
            // Kotlin allows keywords in package names (e.g. `jetchat.data.unreadMessages`
            // where `data` is a keyword).
            if self.peek_kind() != TokenKind::Ident && !self.peek_kind().is_keyword() {
                break;
            }
            let idx = self.pos;
            let span = self.peek_span();
            self.bump();
            path.push(self.intern_ident_at(idx));
            end_span = span;
            if !self.eat(TokenKind::Dot) {
                break;
            }
        }
        // Check for import alias: `import com.example.Foo as Bar`
        self.skip_trivia();
        let alias = if self.peek_kind() == TokenKind::KwAs {
            self.bump(); // consume `as`
            self.skip_trivia();
            if self.peek_kind() == TokenKind::Ident {
                let idx = self.pos;
                let span = self.peek_span();
                self.bump();
                end_span = span;
                Some(self.intern_ident_at(idx))
            } else {
                self.diags.push(Diagnostic::error(
                    self.peek_span(),
                    "expected alias name after 'as'",
                ));
                None
            }
        } else {
            None
        };
        ImportDecl {
            path,
            is_wildcard,
            alias,
            span: kw.merge(end_span),
        }
    }

    fn parse_class_decl(&mut self) -> ClassDecl {
        let kw = self.expect(TokenKind::KwClass, "class");
        self.skip_trivia();
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.diags
                .push(Diagnostic::error(name_span, "expected class name"));
            self.interner.intern("")
        };
        self.skip_trivia();

        // Type parameters: `class Box<T>(...)`.
        let type_params = self.parse_type_params();
        self.skip_trivia();

        // Optional annotations + visibility modifier + `constructor`
        // keyword between the class name (+ type params) and the
        // primary constructor args:
        //   `class Color private constructor(val r: Int)`
        //   `class Foo @Throws(E::class) constructor(x: T) : ...`
        // Annotations on the primary constructor are common when the
        // user wants to attach `@Inject`, `@Throws`, or a custom
        // qualifier to the synthetic `<init>` method (this is the
        // canonical place — Kotlin doesn't otherwise let you target
        // the primary ctor by name). Skotch parses them and drops the
        // payload — the ctor still compiles to the same bytecode
        // regardless of annotations, since the JVM-level ctor is the
        // class constructor.
        let _ctor_annotations = self.parse_annotations();
        while matches!(
            self.peek_kind(),
            TokenKind::KwPrivate | TokenKind::KwProtected | TokenKind::KwInternal
        ) {
            self.bump();
            self.skip_trivia();
        }
        // A second annotation block may appear AFTER the visibility
        // modifier and before `constructor`, e.g.
        //   `class Foo private @Inject constructor(x: T)`
        let _ctor_annotations2 = self.parse_annotations();
        if self.peek_kind() == TokenKind::KwConstructor {
            self.bump();
            self.skip_trivia();
        }

        // Primary constructor parameters.
        let mut constructor_params = Vec::new();
        if self.peek_kind() == TokenKind::LParen {
            self.bump();
            self.skip_trivia();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    // Parse annotations on the constructor param (e.g.
                    // `@JvmField val x: Int`) so they reach mir-lower
                    // for backend codegen decisions.
                    let param_annotations = self.parse_annotations();
                    let param_start = self.peek_span();
                    // Skip visibility modifiers: private/protected/internal.
                    if matches!(
                        self.peek_kind(),
                        TokenKind::KwPrivate | TokenKind::KwProtected | TokenKind::KwInternal
                    ) {
                        self.bump();
                        self.skip_trivia();
                    }
                    // Check for val/var prefix.
                    let is_val = self.peek_kind() == TokenKind::KwVal;
                    let is_var = self.peek_kind() == TokenKind::KwVar;
                    if is_val || is_var {
                        self.bump();
                        self.skip_trivia();
                    }
                    let p = self.parse_param();
                    constructor_params.push(ConstructorParam {
                        is_val,
                        is_var,
                        name: p.name,
                        ty: p.ty,
                        span: param_start.merge(p.span),
                        annotations: param_annotations,
                    });
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    // Trailing comma support.
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::RParen {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, ")");
            self.skip_trivia();
        }

        // Superclass / interface clause: `: ParentClass(args), Interface1, Interface2`
        // Also handles interface delegation: `: Base by b`
        let mut parent_class = None;
        let mut interfaces = Vec::new();
        let mut interface_delegates = Vec::new();
        if self.peek_kind() == TokenKind::Colon {
            self.bump(); // consume ':'
            self.skip_trivia();
            let parent_name_idx = self.pos;
            let parent_name_span = self.peek_span();
            if self.peek_kind() == TokenKind::Ident {
                self.bump();
                let mut parent_name = self.intern_ident_at(parent_name_idx);
                self.skip_trivia();
                // Skip a parameterized supertype's generic args, e.g.
                // `: Comparable<Money>` or `: Map<String, Int>`. The
                // class model doesn't yet carry supertype type args;
                // this just keeps the rest of the source parseable.
                self.skip_supertype_generic_args();
                // Nested supertype reference: `: Outer<T>.Inner(args)`
                // (KotlinCrypto/hash's SHAKEDigest extends
                // `XofFactory<A>.XofDelegate(delegate)`). Fold the
                // dotted-name chain into a single concatenated parent
                // name so the rest of the supertype clause parses.
                // Class lookups still resolve on the LAST segment
                // since that's what the JVM dispatches against.
                while self.peek_kind() == TokenKind::Dot {
                    self.bump();
                    self.skip_trivia();
                    if self.peek_kind() != TokenKind::Ident {
                        break;
                    }
                    let seg_idx = self.pos;
                    self.bump();
                    let segment = self.intern_ident_at(seg_idx);
                    let joined = format!(
                        "{}.{}",
                        self.interner.resolve(parent_name),
                        self.interner.resolve(segment),
                    );
                    parent_name = self.interner.intern(&joined);
                    self.skip_trivia();
                    self.skip_supertype_generic_args();
                }
                if self.peek_kind() == TokenKind::LParen {
                    // Superclass with constructor call: Name(args)
                    self.bump();
                    self.skip_trivia();
                    let mut args = Vec::new();
                    if self.peek_kind() != TokenKind::RParen {
                        loop {
                            self.skip_trivia();
                            let expr = self.parse_expr();
                            args.push(CallArg { name: None, expr });
                            self.skip_trivia();
                            if !self.eat(TokenKind::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen, ")");
                    self.skip_trivia();
                    parent_class = Some(SuperClassRef {
                        name: parent_name,
                        name_span: parent_name_span,
                        args,
                    });
                } else if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "by"
                {
                    // Interface delegation: `Base by b`
                    self.bump(); // consume 'by'
                    self.skip_trivia();
                    let delegate_idx = self.pos;
                    self.expect(TokenKind::Ident, "delegate parameter name");
                    let delegate_name = self.intern_ident_at(delegate_idx);
                    interfaces.push(parent_name);
                    interface_delegates.push((parent_name, delegate_name));
                    self.skip_trivia();
                } else {
                    // No parens → interface implementation (not a superclass call).
                    interfaces.push(parent_name);
                }
                // Additional interfaces after comma.
                while self.eat(TokenKind::Comma) {
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::Ident {
                        let iface_idx = self.pos;
                        self.bump();
                        let iface_name = self.intern_ident_at(iface_idx);
                        self.skip_trivia();
                        // Same generic-args skip as the first supertype.
                        self.skip_supertype_generic_args();
                        // Check for delegation: `Interface by param`
                        if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "by"
                        {
                            self.bump(); // consume 'by'
                            self.skip_trivia();
                            let delegate_idx = self.pos;
                            self.expect(TokenKind::Ident, "delegate parameter name");
                            let delegate_name = self.intern_ident_at(delegate_idx);
                            interface_delegates.push((iface_name, delegate_name));
                        }
                        interfaces.push(iface_name);
                        self.skip_trivia();
                    }
                }
            }
        }

        // Class body.
        let mut properties = Vec::new();
        let mut methods = Vec::new();
        let mut companion_methods = Vec::new();
        let mut companion_properties = Vec::new();
        let mut init_blocks = Vec::new();
        let mut secondary_constructors = Vec::new();
        let mut nested_classes = Vec::new();
        if self.peek_kind() == TokenKind::LBrace {
            self.bump();
            loop {
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                    break;
                }
                // Parse annotations before class members.
                let member_annotations = self.parse_annotations();
                // Capture modifier keywords before members.
                let mut mem_override = false;
                let mut mem_open = false;
                let mut mem_lateinit = false;
                let mut mem_abstract = false;
                let mut mem_suspend = false;
                while matches!(
                    self.peek_kind(),
                    TokenKind::KwOverride
                        | TokenKind::KwOpen
                        | TokenKind::KwAbstract
                        | TokenKind::KwInfix
                        | TokenKind::KwPrivate
                        | TokenKind::KwProtected
                        | TokenKind::KwInternal
                        | TokenKind::KwOperator
                        | TokenKind::KwLateinit
                        | TokenKind::KwSuspend
                        | TokenKind::KwTailrec
                ) {
                    match self.peek_kind() {
                        TokenKind::KwOverride => mem_override = true,
                        TokenKind::KwOpen => mem_open = true,
                        TokenKind::KwAbstract => mem_abstract = true,
                        TokenKind::KwLateinit => mem_lateinit = true,
                        TokenKind::KwSuspend => mem_suspend = true,
                        _ => {}
                    }
                    self.bump();
                    self.skip_trivia();
                }
                match self.peek_kind() {
                    TokenKind::KwFun => {
                        let mut f = self.parse_fun_decl();
                        f.is_override = mem_override;
                        f.is_open = mem_open;
                        f.is_abstract = mem_abstract;
                        f.is_suspend = mem_suspend;
                        f.annotations = member_annotations.clone();
                        methods.push(f);
                    }
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let mut prop = self.parse_property_decl();
                        prop.is_lateinit = mem_lateinit;
                        properties.push(prop);
                    }
                    TokenKind::KwInit => {
                        self.bump(); // consume 'init'
                        self.skip_trivia();
                        init_blocks.push(self.parse_block());
                    }
                    TokenKind::KwConstructor => {
                        secondary_constructors.push(self.parse_secondary_constructor());
                    }
                    TokenKind::KwClass => {
                        // Nested (static inner) class declaration.
                        nested_classes.push(self.parse_class_decl());
                    }
                    TokenKind::Ident => {
                        // Check for `inner class ...` or `companion object { ... }`.
                        let idx = self.pos;
                        let text = match self.payload(idx) {
                            Some(TokenPayload::Ident(s)) => s.clone(),
                            _ => String::new(),
                        };
                        if text == "inner" && self.peek_kind_at(1) == TokenKind::KwClass {
                            self.bump(); // consume 'inner'
                            self.skip_trivia();
                            let mut inner_class = self.parse_class_decl();
                            inner_class.is_inner = true;
                            nested_classes.push(inner_class);
                        } else if text == "companion" {
                            self.bump(); // consume 'companion'
                            self.skip_trivia();
                            if self.peek_kind() == TokenKind::KwObject {
                                self.bump(); // consume 'object'
                                self.skip_trivia();
                                // Optional supertype clause:
                                //   `companion object : Parent(args), Iface`
                                // KotlinCrypto/hash's SHAKE128 has a
                                // `public companion object:
                                // SHAKEXofFactory<SHAKE128>() { ... }`.
                                // We don't model the companion's
                                // supertype yet; just consume the
                                // tokens so the body parses cleanly.
                                if self.peek_kind() == TokenKind::Colon {
                                    self.bump();
                                    self.skip_trivia();
                                    loop {
                                        if self.peek_kind() != TokenKind::Ident {
                                            break;
                                        }
                                        self.bump();
                                        self.skip_supertype_generic_args();
                                        // Optional constructor-call parens.
                                        if self.peek_kind() == TokenKind::LParen {
                                            self.skip_balanced(
                                                TokenKind::LParen,
                                                TokenKind::RParen,
                                            );
                                        }
                                        self.skip_trivia();
                                        // Multiple interfaces after comma.
                                        if self.peek_kind() != TokenKind::Comma {
                                            break;
                                        }
                                        self.bump();
                                        self.skip_trivia();
                                    }
                                }
                                // Parse companion object body.
                                if self.peek_kind() == TokenKind::LBrace {
                                    self.bump();
                                    loop {
                                        self.skip_trivia();
                                        if self.peek_kind() == TokenKind::RBrace
                                            || self.peek_kind() == TokenKind::Eof
                                        {
                                            break;
                                        }
                                        // Parse annotations on the companion
                                        // member so they reach mir-lower (for
                                        // example `@JvmStatic`, which the
                                        // companion-static-delegate synthesis
                                        // looks for).
                                        let comp_annotations = self.parse_annotations();
                                        // Skip modifiers (const, private, etc.)
                                        while matches!(
                                            self.peek_kind(),
                                            TokenKind::KwConst
                                                | TokenKind::KwPrivate
                                                | TokenKind::KwInternal
                                        ) {
                                            self.bump();
                                            self.skip_trivia();
                                        }
                                        if self.peek_kind() == TokenKind::KwFun {
                                            let mut f = self.parse_fun_decl();
                                            f.annotations = comp_annotations;
                                            companion_methods.push(f);
                                        } else if matches!(
                                            self.peek_kind(),
                                            TokenKind::KwVal | TokenKind::KwVar
                                        ) {
                                            // PropertyDecl doesn't carry
                                            // annotations yet; drop them on
                                            // the floor for parity with the
                                            // prior skip_annotations behavior.
                                            let _ = comp_annotations;
                                            companion_properties.push(self.parse_property_decl());
                                        } else {
                                            let _ = comp_annotations;
                                            self.bump();
                                        }
                                    }
                                    if self.peek_kind() == TokenKind::RBrace {
                                        self.bump();
                                    }
                                }
                            }
                        } else {
                            self.bump(); // skip unknown ident
                        }
                    }
                    _ => {
                        self.bump(); // skip unknown token
                    }
                }
            }
            self.expect(TokenKind::RBrace, "}");
        }

        ClassDecl {
            is_data: false,     // set by caller if `data` modifier present
            is_open: false,     // set by caller
            is_abstract: false, // set by caller
            is_sealed: false,   // set by caller
            name,
            name_span,
            type_params,
            constructor_params,
            parent_class,
            interfaces,
            interface_delegates,
            properties,
            methods,
            companion_methods,
            companion_properties,
            init_blocks,
            secondary_constructors,
            nested_classes,
            is_inner: false,                   // set by caller for `inner class`
            visibility: Visibility::default(), // set by caller
            annotations: Vec::new(),
            span: kw.merge(name_span),
        }
    }

    /// Parse a secondary constructor: `constructor(params) : this(args) { body }`.
    fn parse_secondary_constructor(&mut self) -> SecondaryConstructor {
        let start = self.expect(TokenKind::KwConstructor, "constructor");
        self.skip_trivia();

        // Parse parameter list. Kotlin allows a trailing comma after
        // the last param (idiomatic on multi-line declarations like
        // BLAKE2Digest's primary ctor with 16 named args), so after
        // consuming a comma we re-check for the closing `)` before
        // looping back into parse_param.
        let mut params = Vec::new();
        self.expect(TokenKind::LParen, "(");
        self.skip_trivia();
        if self.peek_kind() != TokenKind::RParen {
            loop {
                self.skip_trivia();
                params.push(self.parse_param());
                self.skip_trivia();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RParen {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen, ")");
        self.skip_trivia();

        // Parse optional delegation: `: this(args)` or `: super(args)`.
        // The two share an arg-list shape but route the call differently
        // at MIR-lowering time — `this(...)` dispatches to a sibling
        // secondary constructor on the same class, while `super(...)`
        // invokes the parent's primary constructor. The `delegate_is_super`
        // flag carries that distinction through.
        let mut delegate_args = Vec::new();
        let mut has_delegation = false;
        let mut delegate_is_super = false;
        if self.eat(TokenKind::Colon) {
            has_delegation = true;
            self.skip_trivia();
            // Consume the delegation target keyword. `super` lexes to
            // its own `KwSuper`; `this` lexes as a plain Ident
            // (`KwConstructor` also gets accepted for backward
            // compatibility with the prior implementation, though
            // kotlinc would reject it).
            if self.peek_kind() == TokenKind::KwSuper {
                delegate_is_super = true;
                self.bump();
                self.skip_trivia();
            } else if self.peek_kind() == TokenKind::Ident
                || self.peek_kind() == TokenKind::KwConstructor
            {
                self.bump();
                self.skip_trivia();
            }
            // Parse delegate call args. Named-arg form
            // `name = expr` is common — the KotlinCrypto/hash
            // codebase uses it heavily in `: super(...)` delegations
            // (e.g. `super(bitStrength = 224, h = H)`). Detect the
            // `Ident Eq` lookahead the same way regular call sites do.
            self.expect(TokenKind::LParen, "(");
            self.skip_trivia();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    let saved = self.pos;
                    let mut named = false;
                    if self.peek_kind() == TokenKind::Ident {
                        let after = self.pos + 1;
                        let next_kind = self.peek_kind_skip_trivia_at(after);
                        if next_kind == TokenKind::Eq {
                            named = true;
                        }
                    }
                    if named {
                        let name_idx = self.pos;
                        self.bump();
                        let name_sym = self.intern_ident_at(name_idx);
                        self.skip_trivia();
                        self.bump(); // consume '='
                        self.skip_trivia();
                        let value = self.parse_expr();
                        delegate_args.push(CallArg {
                            name: Some(name_sym),
                            expr: value,
                        });
                    } else {
                        self.pos = saved;
                        let value = self.parse_expr();
                        delegate_args.push(CallArg {
                            name: None,
                            expr: value,
                        });
                    }
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    // Trailing-comma support for multi-line
                    // `: super(\n  a = …,\n  b = …,\n)` delegations.
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::RParen {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, ")");
            self.skip_trivia();
        }

        // Parse optional body block.
        let body = if self.peek_kind() == TokenKind::LBrace {
            Some(self.parse_block())
        } else {
            None
        };

        let end = body.as_ref().map(|b| b.span).unwrap_or(self.peek_span());
        SecondaryConstructor {
            params,
            has_delegation,
            delegate_is_super,
            delegate_args,
            body,
            span: start.merge(end),
        }
    }

    fn parse_typealias_decl(&mut self) -> TypeAliasDecl {
        let start = self.peek_span();
        self.bump(); // consume `typealias` ident
        self.skip_trivia();
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.interner.intern("")
        };
        let type_params = self.parse_type_params();
        self.skip_trivia();
        self.expect(TokenKind::Eq, "=");
        self.skip_trivia();
        let target = self.parse_type_ref();
        TypeAliasDecl {
            name,
            name_span,
            type_params,
            target,
            span: start.merge(name_span),
        }
    }

    fn parse_object_decl(&mut self) -> ObjectDecl {
        let kw = self.expect(TokenKind::KwObject, "object");
        self.skip_trivia();
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.diags
                .push(Diagnostic::error(name_span, "expected object name"));
            self.interner.intern("")
        };
        self.skip_trivia();

        // Optional supertype clause: `: Parent(args)?, Iface1, Iface2`.
        // Kotlin allows objects to extend a class (in addition to
        // implementing interfaces); the parent_class is the first entry
        // when it carries a `(args)` constructor invocation.
        let (parent_class, interfaces) = self.parse_simple_supertype_clause(true);

        let mut methods = Vec::new();
        if self.peek_kind() == TokenKind::LBrace {
            self.bump();
            loop {
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                    break;
                }
                // Skip annotations before object members.
                self.skip_annotations();
                // Skip modifier keywords.
                let mut obj_suspend = false;
                while matches!(
                    self.peek_kind(),
                    TokenKind::KwOverride
                        | TokenKind::KwOpen
                        | TokenKind::KwPrivate
                        | TokenKind::KwProtected
                        | TokenKind::KwInternal
                        | TokenKind::KwSuspend
                        | TokenKind::KwTailrec
                ) {
                    if self.peek_kind() == TokenKind::KwSuspend {
                        obj_suspend = true;
                    }
                    self.bump();
                    self.skip_trivia();
                }
                if self.peek_kind() == TokenKind::KwFun {
                    let mut f = self.parse_fun_decl();
                    f.is_suspend = obj_suspend;
                    methods.push(f);
                } else {
                    self.bump(); // skip unknown
                }
            }
            self.expect(TokenKind::RBrace, "}");
        }

        ObjectDecl {
            name,
            name_span,
            methods,
            parent_class,
            interfaces,
            span: kw.merge(name_span),
        }
    }

    fn parse_enum_decl(&mut self) -> EnumDecl {
        let kw = self.expect(TokenKind::KwClass, "class");
        self.skip_trivia();
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.diags
                .push(Diagnostic::error(name_span, "expected enum name"));
            self.interner.intern("")
        };
        self.skip_trivia();

        // Optional constructor params: enum class Color(val hex: Int)
        let mut constructor_params = Vec::new();
        if self.peek_kind() == TokenKind::LParen {
            self.bump();
            self.skip_trivia();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    self.skip_annotations();
                    let param_start = self.peek_span();
                    let is_val = self.peek_kind() == TokenKind::KwVal;
                    let is_var = self.peek_kind() == TokenKind::KwVar;
                    if is_val || is_var {
                        self.bump();
                        self.skip_trivia();
                    }
                    let p = self.parse_param();
                    constructor_params.push(ConstructorParam {
                        is_val,
                        is_var,
                        name: p.name,
                        ty: p.ty,
                        span: param_start.merge(p.span),
                        annotations: Vec::new(),
                    });
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, ")");
            self.skip_trivia();
        }

        // Optional `: Iface1, Iface2` — enums can implement interfaces.
        // The parent class slot is unused (enums always extend
        // `kotlin/Enum` implicitly), so pass `allow_parent_class: false`
        // — anyone writing `enum class X : Foo(...)` gets a parse error
        // rather than silently producing the wrong supertype.
        let (_, enum_interfaces) = self.parse_simple_supertype_clause(false);

        let mut entries = Vec::new();
        let mut enum_methods = Vec::new();
        if self.peek_kind() == TokenKind::LBrace {
            self.bump();
            loop {
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                    break;
                }
                if self.peek_kind() == TokenKind::Ident {
                    let entry_idx = self.pos;
                    self.bump();
                    let entry_name = self.intern_ident_at(entry_idx);
                    // Optional args: RED(0xFF0000)
                    let mut entry_args = Vec::new();
                    if self.peek_kind() == TokenKind::LParen {
                        self.bump();
                        self.skip_trivia();
                        if self.peek_kind() != TokenKind::RParen {
                            loop {
                                self.skip_trivia();
                                entry_args.push(self.parse_expr());
                                self.skip_trivia();
                                if !self.eat(TokenKind::Comma) {
                                    break;
                                }
                            }
                        }
                        self.expect(TokenKind::RParen, ")");
                    }
                    // Optional anonymous class body: PLUS { override fun ... }
                    let mut entry_methods = Vec::new();
                    // Skip newlines but NOT semicolons before checking for body.
                    while self.peek_kind() == TokenKind::Newline {
                        self.pos += 1;
                    }
                    if self.peek_kind() == TokenKind::LBrace {
                        self.bump();
                        loop {
                            self.skip_trivia();
                            if self.peek_kind() == TokenKind::RBrace
                                || self.peek_kind() == TokenKind::Eof
                            {
                                break;
                            }
                            // Skip `override` keyword if present.
                            if self.peek_kind() == TokenKind::KwOverride {
                                self.bump();
                                self.skip_trivia();
                            }
                            if self.peek_kind() == TokenKind::KwFun {
                                entry_methods.push(self.parse_fun_decl());
                            } else {
                                self.bump(); // skip unknown token
                            }
                        }
                        if self.peek_kind() == TokenKind::RBrace {
                            self.bump();
                        }
                    }
                    entries.push(skotch_syntax::EnumEntry {
                        name: entry_name,
                        args: entry_args,
                        methods: entry_methods,
                    });
                    // Skip newlines but NOT semicolons — we need to detect
                    // the `;` separator between entries and class body.
                    while self.peek_kind() == TokenKind::Newline {
                        self.pos += 1;
                    }
                    if self.peek_kind() == TokenKind::Comma {
                        self.bump();
                    } else if self.peek_kind() == TokenKind::Semi {
                        self.bump();
                        break;
                    }
                } else {
                    self.bump(); // skip unknown
                }
            }
            // Parse class-body methods after the entries (abstract methods, etc.).
            loop {
                self.skip_trivia();
                let pk = self.peek_kind();
                if pk == TokenKind::RBrace || pk == TokenKind::Eof {
                    break;
                }
                // Skip `abstract` keyword if present.
                let is_abstract = pk == TokenKind::KwAbstract;
                if is_abstract {
                    self.bump();
                    self.skip_trivia();
                }
                if self.peek_kind() == TokenKind::KwFun {
                    let mut fd = self.parse_fun_decl();
                    fd.is_abstract = is_abstract;
                    enum_methods.push(fd);
                } else {
                    self.bump(); // skip unknown token
                }
            }
            if self.peek_kind() == TokenKind::RBrace {
                self.bump();
            }
        }

        EnumDecl {
            name,
            name_span,
            constructor_params,
            entries,
            methods: enum_methods,
            interfaces: enum_interfaces,
            span: kw.merge(name_span),
        }
    }

    fn parse_interface_decl(&mut self) -> InterfaceDecl {
        let kw = self.expect(TokenKind::KwInterface, "interface");
        self.skip_trivia();
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.diags
                .push(Diagnostic::error(name_span, "expected interface name"));
            self.interner.intern("")
        };
        self.skip_trivia();
        // Optional generic params on the interface declaration itself
        // (`interface Iter<T> : Source<T>`). We skip them since
        // InterfaceDecl doesn't yet carry type params; the supertype
        // clause needs to parse cleanly though.
        if self.peek_kind() == TokenKind::Lt {
            self.skip_supertype_generic_args();
        }

        // Optional `: Iface1, Iface2` — interfaces may extend other
        // interfaces. `allow_parent_class: false` rejects a leading
        // `: Foo(args)` clause with a diagnostic (interfaces can't
        // extend classes in Kotlin).
        let (_, super_interfaces) = self.parse_simple_supertype_clause(false);

        let mut methods = Vec::new();
        if self.peek_kind() == TokenKind::LBrace {
            self.bump();
            loop {
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                    break;
                }
                // Skip annotations before interface members.
                self.skip_annotations();
                // Skip modifiers.
                let mut iface_suspend = false;
                while matches!(
                    self.peek_kind(),
                    TokenKind::KwOverride
                        | TokenKind::KwOpen
                        | TokenKind::KwAbstract
                        | TokenKind::KwInfix
                        | TokenKind::KwPrivate
                        | TokenKind::KwProtected
                        | TokenKind::KwInternal
                        | TokenKind::KwOperator
                        | TokenKind::KwSuspend
                        | TokenKind::KwTailrec
                ) {
                    if self.peek_kind() == TokenKind::KwSuspend {
                        iface_suspend = true;
                    }
                    self.bump();
                    self.skip_trivia();
                }
                if self.peek_kind() == TokenKind::KwFun {
                    let mut f = self.parse_fun_decl();
                    f.is_suspend = iface_suspend;
                    // Interface methods without a body are abstract by default.
                    if f.body.stmts.is_empty() {
                        f.is_abstract = true;
                    }
                    methods.push(f);
                } else {
                    self.bump(); // skip unknown
                }
            }
            self.expect(TokenKind::RBrace, "}");
        }

        InterfaceDecl {
            name,
            name_span,
            methods,
            interfaces: super_interfaces,
            span: kw.merge(name_span),
        }
    }

    fn parse_property_decl(&mut self) -> PropertyDecl {
        let start = self.peek_span();
        let is_var = self.peek_kind() == TokenKind::KwVar;
        self.bump(); // consume val/var
        self.skip_trivia();
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident || self.peek_kind().is_soft_keyword() {
            // Soft keywords (`data`, `open`, `init`, …) are legal
            // identifiers when they appear in identifier positions like
            // `val data: Int = 0`. `intern_ident_at` falls back to
            // `keyword_text()` when the token isn't a plain Ident.
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.interner.intern("")
        };
        self.skip_trivia();
        let ty = if self.eat(TokenKind::Colon) {
            self.skip_trivia();
            Some(self.parse_type_ref())
        } else {
            None
        };
        // Check for `by <delegate>` delegation.
        // Supports: `by lazy { ... }`, `by Cached { ... }`, `by MyDelegate()`
        self.skip_trivia();
        let (delegate, delegate_is_lazy_block) =
            if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "by" {
                self.bump(); // consume `by`
                self.skip_trivia();
                // Parse the delegate expression. For `lazy { ... }` this is
                // just the block. For `Cached { ... }` it's a constructor call
                // with trailing lambda. For any other expression, parse it.
                if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "lazy" {
                    self.bump(); // consume `lazy`
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::LBrace {
                        (Some(Box::new(self.parse_block())), true)
                    } else {
                        (None, false)
                    }
                } else {
                    // Generic delegate: parse a postfix expression only.
                    // Don't use parse_expr() which crosses newlines.
                    let expr = self.parse_postfix();
                    // Wrap in a synthetic block with a single expression stmt.
                    let block = Block {
                        stmts: vec![Stmt::Expr(expr)],
                        span: self.peek_span(),
                    };
                    (Some(Box::new(block)), false)
                }
            } else {
                (None, false)
            };
        self.skip_trivia();
        let init = if self.eat(TokenKind::Eq) {
            self.skip_trivia();
            Some(self.parse_expr())
        } else {
            None
        };
        // Check for custom getter: `get() = expr` or `get() { ... }`.
        self.skip_trivia();
        let getter = if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "get" {
            self.bump(); // consume `get`
            self.skip_trivia();
            if self.peek_kind() == TokenKind::LParen {
                self.bump(); // (
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RParen {
                    self.bump(); // )
                }
            }
            self.skip_trivia();
            if self.eat(TokenKind::Eq) {
                self.skip_trivia();
                let expr = self.parse_expr();
                let sp = expr.span();
                Some(Block {
                    stmts: vec![Stmt::Return {
                        value: Some(expr),
                        label: None,
                        span: sp,
                    }],
                    span: sp,
                })
            } else if self.peek_kind() == TokenKind::LBrace {
                Some(self.parse_block())
            } else {
                None
            }
        } else {
            None
        };
        // Check for custom setter: `set(value) { ... }` or `set(value) = expr`.
        self.skip_trivia();
        let setter = if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "set" {
            self.bump(); // consume `set`
            self.skip_trivia();
            let setter_param = if self.peek_kind() == TokenKind::LParen {
                self.bump(); // (
                self.skip_trivia();
                let p_idx = self.pos;
                let p = if self.peek_kind() == TokenKind::Ident {
                    self.bump();
                    self.intern_ident_at(p_idx)
                } else {
                    self.interner.intern("value")
                };
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RParen {
                    self.bump(); // )
                }
                p
            } else {
                self.interner.intern("value")
            };
            self.skip_trivia();
            let body = if self.eat(TokenKind::Eq) {
                self.skip_trivia();
                let expr = self.parse_expr();
                let sp = expr.span();
                Block {
                    stmts: vec![Stmt::Expr(expr)],
                    span: sp,
                }
            } else if self.peek_kind() == TokenKind::LBrace {
                self.parse_block()
            } else {
                Block {
                    stmts: Vec::new(),
                    span: start,
                }
            };
            Some((setter_param, body))
        } else {
            None
        };
        PropertyDecl {
            is_var,
            is_lateinit: false, // set by caller when `lateinit` modifier present
            name,
            name_span,
            ty,
            init,
            delegate,
            delegate_is_lazy_block,
            getter,
            setter,
            span: start.merge(name_span),
            // Caller (typically `parse_class_body` or `parse_file`)
            // attaches annotations after the PropertyDecl is built.
            annotations: Vec::new(),
        }
    }

    /// Parse `<T, R : Comparable<R>>` type parameter list.  Returns empty
    /// vec if no `<` follows.
    fn parse_type_params(&mut self) -> Vec<TypeParam> {
        self.skip_trivia();
        if self.peek_kind() != TokenKind::Lt {
            return Vec::new();
        }
        self.bump(); // consume `<`
        let mut params = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek_kind() == TokenKind::Gt {
                break;
            }
            let tp_span = self.peek_span();
            // Optional modifiers: `reified`, `out`, `in` (soft keywords).
            // Note: `in` is lexed as KwIn (hard keyword for `for`).
            let mut is_reified = false;
            loop {
                if self.peek_kind() == TokenKind::KwIn {
                    self.bump();
                    self.skip_trivia();
                    continue;
                }
                if self.peek_kind() == TokenKind::Ident {
                    let text = self.lexeme_str(self.pos);
                    match text {
                        "reified" => {
                            is_reified = true;
                            self.bump();
                            self.skip_trivia();
                        }
                        "out" => {
                            self.bump();
                            self.skip_trivia();
                        }
                        _ => break,
                    }
                } else {
                    break;
                }
            }
            let tp_idx = self.pos;
            let tp_name = if self.peek_kind() == TokenKind::Ident {
                self.bump();
                self.intern_ident_at(tp_idx)
            } else {
                self.interner.intern("")
            };
            self.skip_trivia();
            let bound = if self.eat(TokenKind::Colon) {
                self.skip_trivia();
                let bound_idx = self.pos;
                if self.peek_kind() == TokenKind::Ident {
                    self.bump();
                    let b = self.intern_ident_at(bound_idx);
                    // Skip type arguments on bound: `Comparable<T>`.
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::Lt {
                        let mut depth = 1u32;
                        self.bump();
                        while depth > 0 && self.peek_kind() != TokenKind::Eof {
                            match self.peek_kind() {
                                TokenKind::Lt => depth += 1,
                                TokenKind::Gt => depth -= 1,
                                _ => {}
                            }
                            self.bump();
                        }
                    }
                    Some(b)
                } else {
                    None
                }
            } else {
                None
            };
            params.push(TypeParam {
                name: tp_name,
                bound,
                is_reified,
                span: tp_span,
            });
            self.skip_trivia();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.skip_trivia();
        if self.peek_kind() == TokenKind::Gt {
            self.bump();
        }
        params
    }

    fn parse_fun_decl(&mut self) -> FunDecl {
        let kw = self.expect(TokenKind::KwFun, "fun");
        self.skip_trivia();

        // Type parameters: `fun <T> identity(x: T): T`.
        let type_params = self.parse_type_params();
        self.skip_trivia();

        let name_idx = self.pos;
        let name_span = self.peek_span();

        // Check for extension function: `fun Type.name(...)`. Generic
        // receiver types like `fun <T> Iterable<T>.foo()` are also
        // supported — after reading the receiver ident, check for
        // `<...>` type args, then look for the `.`. Surfaced by
        // parity/49-functional-pipelines.
        let mut receiver_ty = None;
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            let first_ident = self.intern_ident_at(name_idx);
            // Look ahead: is this `Ident<...>` followed by a `.`?
            // If so, parse the type args and treat as a generic
            // receiver type.
            let mut recv_type_args: Vec<TypeRef> = Vec::new();
            let saved_pos = self.pos;
            if self.peek_kind() == TokenKind::Lt {
                self.bump(); // tentatively consume `<`
                self.skip_trivia();
                let mut args = Vec::new();
                loop {
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::Gt || self.peek_kind() == TokenKind::Eof {
                        break;
                    }
                    // Star projection `*` becomes `Any`. Kotlin's
                    // `Iterable<*>` erases to `Iterable<Any?>` at the
                    // JVM level — using `Any` here is close enough for
                    // skotch's MIR (which already erases generics).
                    if self.peek_kind() == TokenKind::Star {
                        let star_span = self.peek_span();
                        self.bump();
                        let any_name = self.interner.intern("Any");
                        args.push(TypeRef {
                            name: any_name,
                            nullable: false,
                            func_params: None,
                            type_args: Vec::new(),
                            is_suspend: false,
                            is_composable: false,
                            has_receiver: false,
                            span: star_span,
                        });
                    } else {
                        args.push(self.parse_type_ref());
                    }
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.skip_trivia();
                if self.peek_kind() == TokenKind::Gt && self.peek_kind_at(1) == TokenKind::Dot {
                    self.bump(); // consume `>`
                    recv_type_args = args;
                } else {
                    // Not a generic receiver — restore position so the
                    // body below treats first_ident as the fn name.
                    self.pos = saved_pos;
                }
            }
            if self.peek_kind() == TokenKind::Dot {
                // Extension function: first_ident is the receiver type
                // (possibly with type args).
                let recv_name = first_ident;
                let recv_span = name_span;
                receiver_ty = Some(TypeRef {
                    name: recv_name,
                    nullable: false,
                    func_params: None,
                    type_args: recv_type_args,
                    is_suspend: false,
                    is_composable: false,
                    has_receiver: false,
                    span: recv_span,
                });
                self.bump(); // consume `.`
                let fn_name_idx = self.pos;
                if self.peek_kind() == TokenKind::Ident {
                    self.bump();
                    self.intern_ident_at(fn_name_idx)
                } else {
                    self.diags.push(Diagnostic::error(
                        self.peek_span(),
                        "expected function name after '.'",
                    ));
                    self.interner.intern("")
                }
            } else {
                first_ident
            }
        } else {
            self.diags
                .push(Diagnostic::error(name_span, "expected function name"));
            self.interner.intern("")
        };
        self.expect(TokenKind::LParen, "(");
        let mut params = Vec::new();
        self.skip_trivia();
        if self.peek_kind() != TokenKind::RParen {
            loop {
                params.push(self.parse_param());
                self.skip_trivia();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RParen {
                    break;
                }
            }
        }
        let rparen = self.expect(TokenKind::RParen, ")");
        self.skip_trivia();
        let return_ty = if self.eat(TokenKind::Colon) {
            Some(self.parse_type_ref())
        } else {
            None
        };
        self.skip_trivia();
        // Support block body `{ ... }`, expression body `= expr`, or
        // no body at all (abstract methods).
        let body = if self.peek_kind() == TokenKind::Eq {
            self.bump(); // consume `=`
            self.skip_trivia();
            let expr = self.parse_expr();
            let span = expr.span();
            Block {
                stmts: vec![Stmt::Return {
                    value: Some(expr),
                    label: None,
                    span,
                }],
                span,
            }
        } else if self.peek_kind() == TokenKind::LBrace {
            self.parse_block()
        } else {
            // Abstract function — no body. Synthesize an empty block.
            let end = return_ty.as_ref().map(|t| t.span).unwrap_or(rparen);
            Block {
                stmts: vec![],
                span: end,
            }
        };
        let span = kw.merge(rparen).merge(body.span);
        FunDecl {
            name,
            name_span,
            type_params,
            params,
            return_ty,
            receiver_ty,
            body,
            is_open: false,
            is_override: false,
            is_abstract: false,
            is_suspend: false,
            is_inline: false,
            is_ext_property: false,
            visibility: Visibility::Public,
            annotations: Vec::new(),
            span,
        }
    }

    fn parse_param(&mut self) -> Param {
        self.skip_trivia();
        self.skip_annotations();
        // Check for `vararg` modifier before the parameter name.
        let is_vararg = self.eat(TokenKind::KwVararg);
        if is_vararg {
            self.skip_trivia();
        }
        // Consume `crossinline` / `noinline` modifiers (Ident tokens —
        // not lexer keywords). Inline-function lambda-param modifiers
        // controlling non-local return / inlining behavior. skotch
        // currently ignores them: `crossinline` parses identically to
        // a regular inline lambda (non-local returns are already not
        // supported), `noinline` is a JVM perf hint. Surfaced by
        // parity/50-modifiers-and-delegation.
        while self.peek_kind() == TokenKind::Ident {
            let text = match self.payload(self.pos) {
                Some(TokenPayload::Ident(s)) => s.clone(),
                _ => String::new(),
            };
            if text == "crossinline" || text == "noinline" {
                self.bump();
                self.skip_trivia();
            } else {
                break;
            }
        }
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident || self.peek_kind().is_keyword() {
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.diags
                .push(Diagnostic::error(name_span, "expected parameter name"));
            self.interner.intern("")
        };
        // Type annotation is optional for lambda params: `{ x, y -> ... }`
        let ty = if self.eat(TokenKind::Colon) {
            self.parse_type_ref()
        } else {
            // Infer as Any for untyped lambda params.
            TypeRef {
                name: self.interner.intern("Any"),
                nullable: false,
                func_params: None,
                type_args: Vec::new(),
                is_suspend: false,
                is_composable: false,
                has_receiver: false,
                span: name_span,
            }
        };
        // Check for default value: `param: Type = expr`
        self.skip_trivia();
        let default = if self.eat(TokenKind::Eq) {
            self.skip_trivia();
            Some(Box::new(self.parse_expr()))
        } else {
            None
        };
        let end = default.as_ref().map(|d| d.span()).unwrap_or(ty.span);
        Param {
            name,
            default,
            is_vararg,
            span: name_span.merge(end),
            ty,
        }
    }

    fn parse_type_ref(&mut self) -> TypeRef {
        self.skip_trivia();
        let span = self.peek_span();

        // Scan annotations on types: `@Composable () -> Unit`.
        // Compose uses annotations on function types extensively.
        // Track whether `@Composable` (qualified or not) appears so the
        // resulting TypeRef carries `is_composable = true`. The JVM
        // descriptor for a `@Composable () -> R` function-type param
        // is `Function2<Composer, Int, R>`, not `Function0<R>`, and
        // without this the surrounding `@Composable` function gets
        // emitted with the wrong descriptor (e.g. `JetchatDrawer(...
        // Function0, Composer, I)V` instead of `(...Function2,
        // Composer, I)V` — Compose runtime then ClassCastExceptions
        // the trailing content lambda).
        let mut saw_composable = false;
        while self.peek_kind() == TokenKind::At {
            self.bump(); // consume '@'
                         // Skip annotation name (possibly qualified: @a.b.Composable)
            let mut last_name: Option<String> = None;
            while self.peek_kind() == TokenKind::Ident || self.peek_kind().is_keyword() {
                last_name = Some(self.lexeme_str(self.pos).to_string());
                self.bump();
                if self.peek_kind() == TokenKind::Dot {
                    self.bump();
                } else {
                    break;
                }
            }
            if last_name.as_deref() == Some("Composable") {
                saw_composable = true;
            }
            // Skip annotation arguments if present, but NOT if the `(`
            // starts a function type `() -> Unit`. Detect by checking if
            // the matching `)` is followed by `->`.
            if self.peek_kind() == TokenKind::LParen {
                let saved = self.pos;
                self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                self.skip_trivia();
                if self.peek_kind() == TokenKind::Arrow {
                    // This was a function type, not annotation args — restore.
                    self.pos = saved;
                }
            }
            self.skip_trivia();
        }

        // `suspend () -> T` function-type prefix. Consume
        // the `suspend` keyword and remember it so the returned TypeRef
        // carries `is_suspend = true`. The `suspend` keyword only
        // applies when followed by a `(`, i.e. a function type.
        let is_suspend_type =
            self.peek_kind() == TokenKind::KwSuspend && self.peek_kind_at(1) == TokenKind::LParen;
        if is_suspend_type {
            self.bump(); // consume `suspend`
            self.skip_trivia();
        }

        // Function type: `(Type, Type) -> ReturnType`
        // Disambiguate from parenthesized type `(Type)` by checking for `->`
        // after the closing `)`.
        if self.peek_kind() == TokenKind::LParen {
            let saved_pos = self.pos;
            self.bump();
            self.skip_trivia();
            let mut param_types = Vec::new();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    param_types.push(self.parse_type_ref());
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, ")");
            self.skip_trivia();
            if self.peek_kind() == TokenKind::Arrow {
                // It IS a function type: `(P1, P2) -> R`
                self.bump(); // consume `->`
                self.skip_trivia();
                let ret = self.parse_type_ref();
                let end = ret.span;
                return TypeRef {
                    name: ret.name,
                    nullable: false,
                    func_params: Some(param_types),
                    type_args: Vec::new(),
                    is_suspend: is_suspend_type,
                    is_composable: saw_composable,
                    has_receiver: false,
                    span: span.merge(end),
                };
            }
            // No `->` found: this is a parenthesized type like `(Int)`.
            // Return the inner type (must be exactly one).
            if param_types.len() == 1 {
                return param_types.into_iter().next().unwrap();
            }
            // Multiple types without `->` is invalid, but recover gracefully.
            self.pos = saved_pos + 1; // reset after `(`
        }

        let idx = self.pos;
        let name = if self.peek_kind() == TokenKind::Ident || self.peek_kind().is_keyword() {
            self.bump();
            let mut sym = self.intern_ident_at(idx);
            // Handle fully-qualified type names: `a.b.c.ClassName`
            // Consume dot-separated segments as long as the next segment is
            // an identifier and NOT followed by `(` (which would be a function
            // type `Type.() -> R`).
            while self.peek_kind() == TokenKind::Dot
                && self.peek_kind_at(1) != TokenKind::LParen
                && (self.peek_kind_at(1) == TokenKind::Ident || self.peek_kind_at(1).is_keyword())
            {
                self.bump(); // consume `.`
                let seg_idx = self.pos;
                self.bump(); // consume segment
                             // Use the LAST segment as the type name (it's the class name).
                sym = self.intern_ident_at(seg_idx);
            }
            sym
        } else {
            self.diags
                .push(Diagnostic::error(span, "expected type name"));
            self.interner.intern("")
        };
        // Check for receiver function type: `Type.() -> ReturnType`
        if self.peek_kind() == TokenKind::Dot && self.peek_kind_at(1) == TokenKind::LParen {
            self.bump(); // consume `.`
            self.bump(); // consume `(`
            self.skip_trivia();
            let mut param_types = Vec::new();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    param_types.push(self.parse_type_ref());
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, ")");
            self.skip_trivia();
            self.expect(TokenKind::Arrow, "->");
            self.skip_trivia();
            let ret = self.parse_type_ref();
            let end = ret.span;
            // Encode receiver type as the first func_param with a special
            // marker: the `name` field holds the receiver type name.
            let receiver_type = TypeRef {
                name,
                nullable: false,
                func_params: None,
                type_args: Vec::new(),
                is_suspend: false,
                is_composable: false,
                has_receiver: false,
                span,
            };
            let mut all_params = vec![receiver_type];
            all_params.extend(param_types);
            return TypeRef {
                name: ret.name,
                nullable: false,
                func_params: Some(all_params),
                type_args: Vec::new(),
                is_suspend: false,
                is_composable: saw_composable,
                has_receiver: true,
                span: span.merge(end),
            };
        }

        // Type arguments: `List<Int>`, `Map<String, Int>`.
        let type_args = if self.peek_kind() == TokenKind::Lt {
            self.bump(); // consume `<`
            let mut args = Vec::new();
            loop {
                self.skip_trivia();
                if self.peek_kind() == TokenKind::Gt {
                    break;
                }
                // Handle star projection: `List<*>`.
                if self.peek_kind() == TokenKind::Star {
                    let star_span = self.peek_span();
                    self.bump();
                    args.push(TypeRef {
                        name: self.interner.intern("*"),
                        nullable: false,
                        func_params: None,
                        type_args: Vec::new(),
                        is_suspend: false,
                        is_composable: false,
                        has_receiver: false,
                        span: star_span,
                    });
                } else {
                    args.push(self.parse_type_ref());
                }
                self.skip_trivia();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.skip_trivia();
            if self.peek_kind() == TokenKind::Gt {
                self.bump();
            }
            args
        } else {
            Vec::new()
        };

        // Receiver function type with generic receiver: `Tree<T>.() -> R`.
        // The bare `Type.()` form was handled above before the type-args
        // block; this is the post-generic equivalent. Both shapes encode
        // the receiver as `func_params[0]` and set `has_receiver = true`.
        if self.peek_kind() == TokenKind::Dot && self.peek_kind_at(1) == TokenKind::LParen {
            self.bump(); // consume `.`
            self.bump(); // consume `(`
            self.skip_trivia();
            let mut param_types = Vec::new();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    param_types.push(self.parse_type_ref());
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, ")");
            self.skip_trivia();
            self.expect(TokenKind::Arrow, "->");
            self.skip_trivia();
            let ret = self.parse_type_ref();
            let end = ret.span;
            let receiver_type = TypeRef {
                name,
                nullable: false,
                func_params: None,
                type_args,
                is_suspend: false,
                is_composable: false,
                has_receiver: false,
                span,
            };
            let mut all_params = vec![receiver_type];
            all_params.extend(param_types);
            return TypeRef {
                name: ret.name,
                nullable: false,
                func_params: Some(all_params),
                type_args: Vec::new(),
                is_suspend: false,
                is_composable: false,
                has_receiver: true,
                span: span.merge(end),
            };
        }

        // Nested type reference: `Outer.Inner` or `Outer<A>.Inner`.
        // Collapse the dotted chain into a single composite name so
        // the TypeRef can still flow through type-resolution as one
        // entity. Used both for return-type slots (`fun newReader():
        // Xof<A>.Reader`) and for cast targets (`x as Outer.Inner`).
        // The inner segment may itself carry generic args, which are
        // currently dropped after parse — the JVM-level resolver
        // does name matching on the suffix.
        let mut name = name;
        let mut type_args = type_args;
        while self.peek_kind() == TokenKind::Dot && matches!(self.peek_kind_at(1), TokenKind::Ident)
        {
            self.bump(); // `.`
            self.skip_trivia();
            let seg_idx = self.pos;
            self.bump(); // segment ident
            let segment = self.intern_ident_at(seg_idx);
            let joined = format!(
                "{}.{}",
                self.interner.resolve(name),
                self.interner.resolve(segment),
            );
            name = self.interner.intern(&joined);
            // Drop the OUTER's type args once we've stepped into a
            // nested segment — `Outer<A>.Inner` ends up as the name
            // `Outer.Inner` and the args of the INNER segment win.
            type_args = Vec::new();
            // Read inner-segment type args, if any.
            if self.peek_kind() == TokenKind::Lt {
                self.bump();
                loop {
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::Gt {
                        break;
                    }
                    if self.peek_kind() == TokenKind::Star {
                        let star_span = self.peek_span();
                        self.bump();
                        type_args.push(TypeRef {
                            name: self.interner.intern("*"),
                            nullable: false,
                            func_params: None,
                            type_args: Vec::new(),
                            is_suspend: false,
                            is_composable: false,
                            has_receiver: false,
                            span: star_span,
                        });
                    } else {
                        type_args.push(self.parse_type_ref());
                    }
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.skip_trivia();
                if self.peek_kind() == TokenKind::Gt {
                    self.bump();
                }
            }
        }

        let mut end = span;
        let nullable = if self.peek_kind() == TokenKind::Question {
            end = self.peek_span();
            self.bump();
            true
        } else {
            false
        };
        TypeRef {
            name,
            nullable,
            func_params: None,
            type_args,
            is_suspend: false,
            is_composable: false,
            has_receiver: false,
            span: span.merge(end),
        }
    }

    /// Parse `val Type.name: RetType get() = expr` as an extension function.
    /// Desugars `val String.lastChar: Char get() = this[length-1]` into
    /// `fun String.lastChar(): Char = this[length-1]` (a getter function).
    fn parse_extension_property(&mut self) -> FunDecl {
        let kw_span = self.peek_span();
        self.bump(); // consume val/var
        self.skip_trivia();

        // Parse receiver type
        let recv_idx = self.pos;
        let recv_span = self.peek_span();
        self.expect(TokenKind::Ident, "receiver type name");
        let recv_name = self.intern_ident_at(recv_idx);
        self.expect(TokenKind::Dot, "'.' in extension property");
        self.skip_trivia();

        // Parse property name
        let prop_idx = self.pos;
        let prop_span = self.peek_span();
        self.expect(TokenKind::Ident, "property name");
        let prop_name = self.intern_ident_at(prop_idx);
        self.skip_trivia();

        // Parse optional return type
        let return_ty = if self.eat(TokenKind::Colon) {
            self.skip_trivia();
            Some(self.parse_type_ref())
        } else {
            None
        };
        self.skip_trivia();

        // Parse getter body: `get() = expr` or `get() { stmts }` or
        // `by delegate` or just `= expr`
        let body = if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "by" {
            // Property delegation: `val ReceiverType.prop by delegate`
            self.bump(); // consume `by`
            self.skip_trivia();
            let delegate_expr = self.parse_postfix();
            let s = delegate_expr.span();
            Block {
                stmts: vec![Stmt::Return {
                    value: Some(delegate_expr),
                    label: None,
                    span: s,
                }],
                span: s,
            }
        } else if self.peek_kind() == TokenKind::Ident {
            // Might be `get() = expr`
            let idx = self.pos;
            let text = self.payload(idx).and_then(|p| {
                if let TokenPayload::Ident(s) = p {
                    Some(s.clone())
                } else {
                    None
                }
            });
            if text.as_deref() == Some("get") {
                self.bump(); // consume `get`
                self.skip_trivia();
                if self.eat(TokenKind::LParen) {
                    self.eat(TokenKind::RParen);
                }
                self.skip_trivia();
            }
            if self.eat(TokenKind::Eq) {
                self.skip_trivia();
                let expr = self.parse_expr();
                let s = expr.span();
                Block {
                    stmts: vec![Stmt::Return {
                        value: Some(expr),
                        label: None,
                        span: s,
                    }],
                    span: s,
                }
            } else {
                self.parse_block()
            }
        } else if self.eat(TokenKind::Eq) {
            self.skip_trivia();
            let expr = self.parse_expr();
            let s = expr.span();
            Block {
                stmts: vec![Stmt::Return {
                    value: Some(expr),
                    label: None,
                    span: s,
                }],
                span: s,
            }
        } else {
            self.parse_block()
        };

        let span = kw_span.merge(prop_span);
        let receiver_type = TypeRef {
            name: recv_name,
            nullable: false,
            func_params: None,
            type_args: Vec::new(),
            is_suspend: false,
            is_composable: false,
            has_receiver: false,
            span: recv_span,
        };

        FunDecl {
            name: prop_name,
            name_span: prop_span,
            params: Vec::new(),
            type_params: Vec::new(),
            return_ty,
            body,
            span,
            is_suspend: false,
            is_inline: false,
            is_open: false,
            is_abstract: false,
            is_override: false,
            is_ext_property: true,
            visibility: Visibility::Public,
            annotations: Vec::new(),
            receiver_ty: Some(receiver_type),
        }
    }

    fn parse_val_decl(&mut self) -> ValDecl {
        self.skip_trivia();
        let is_var = self.peek_kind() == TokenKind::KwVar;
        let kw = self.peek_span();
        self.bump(); // val/var
        self.skip_trivia();
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let mut name = if self.peek_kind() == TokenKind::Ident || self.peek_kind().is_keyword() {
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.diags.push(Diagnostic::error(
                name_span,
                "expected name in val/var declaration",
            ));
            self.interner.intern("")
        };
        // Handle extension properties: `var ReceiverType.propName`
        // Parse dotted segments and use the LAST one as the property name.
        while self.peek_kind() == TokenKind::Dot
            && (self.peek_kind_at(1) == TokenKind::Ident || self.peek_kind_at(1).is_keyword())
        {
            self.bump(); // consume `.`
            let seg_idx = self.pos;
            self.bump(); // consume segment
            name = self.intern_ident_at(seg_idx);
        }
        self.skip_trivia();
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.parse_type_ref())
        } else {
            None
        };
        self.skip_trivia();
        // Check for `by <delegate>` — property delegation.
        // `val x: T by lazy { ... }` or `val x: T by Delegate { ... }`
        // Desugars to: val $delegate = <delegate>; val x = $delegate.getValue(...)
        // For simplicity, we desugar `val x by lazy { body }` to `val x = body`
        // (eagerly evaluate the lazy body). For generic delegates with
        // `getValue`, we call the delegate's getValue method.
        if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "by" {
            self.bump(); // consume `by`
            self.skip_trivia();
            // Parse the delegate expression.
            let delegate_expr =
                if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "lazy" {
                    self.bump(); // consume `lazy`
                    self.skip_trivia();
                    // Parse the trailing lambda body.
                    if self.peek_kind() == TokenKind::LBrace {
                        self.parse_lambda_expr()
                    } else {
                        Expr::NullLit(self.peek_span())
                    }
                } else {
                    // Generic delegate: parse a postfix expression only.
                    // Don't use parse_expr() which would consume across
                    // newlines into the next declaration (e.g. @Composable).
                    self.parse_postfix()
                };
            // Desugar: for `by lazy { body }`, the init is the lambda body
            // invoked immediately. For generic delegates, we call getValue.
            let init = match &delegate_expr {
                Expr::Lambda { body, .. } => {
                    // by lazy { expr } → eagerly evaluate the lambda body.
                    // Use the last expression as the init value.
                    if let Some(Stmt::Expr(e) | Stmt::Return { value: Some(e), .. }) =
                        body.stmts.last()
                    {
                        e.clone()
                    } else {
                        delegate_expr
                    }
                }
                _ => {
                    // Generic delegate: `val x by Delegate { ... }`
                    // Desugar to calling getValue on the delegate.
                    // For now, wrap as: delegate.getValue(null, null)
                    // which the MIR lowerer will handle.
                    let get_value = Expr::Field {
                        receiver: Box::new(delegate_expr),
                        name: self.interner.intern("getValue"),
                        span: self.peek_span(),
                    };
                    Expr::Call {
                        callee: Box::new(get_value),
                        args: vec![],
                        type_args: Vec::new(),
                        span: self.peek_span(),
                    }
                }
            };
            return ValDecl {
                is_var,
                is_const: false,
                name,
                name_span,
                ty,
                init,
                visibility: Visibility::default(),
                annotations: Vec::new(),
                span: kw.merge(name_span),
            };
        }
        self.expect(TokenKind::Eq, "=");
        let init = self.parse_expr();
        ValDecl {
            is_var,
            is_const: false,
            name,
            name_span,
            ty,
            visibility: Visibility::default(),
            annotations: Vec::new(),
            span: kw.merge(init.span()),
            init,
        }
    }

    fn parse_block(&mut self) -> Block {
        let lb = self.expect(TokenKind::LBrace, "{");
        let mut stmts = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                break;
            }
            stmts.push(self.parse_stmt());
        }
        let rb = self.expect(TokenKind::RBrace, "}");
        Block {
            stmts,
            span: lb.merge(rb),
        }
    }

    /// Parse a brace-delimited block, or a single statement as a block
    /// (e.g. `for (x in xs) total += x` — no braces).
    fn parse_block_or_single_stmt(&mut self) -> Block {
        if self.peek_kind() == TokenKind::LBrace {
            self.parse_block()
        } else {
            let stmt = self.parse_stmt();
            let span = match &stmt {
                Stmt::Expr(e) => e.span(),
                Stmt::Val(v) => v.span,
                Stmt::Return { span, .. }
                | Stmt::While { span, .. }
                | Stmt::DoWhile { span, .. }
                | Stmt::For { span, .. }
                | Stmt::ForIn { span, .. }
                | Stmt::Assign { span, .. }
                | Stmt::TryStmt { span, .. }
                | Stmt::ThrowStmt { span, .. }
                | Stmt::IndexAssign { span, .. }
                | Stmt::FieldAssign { span, .. }
                | Stmt::Destructure { span, .. } => *span,
                Stmt::LocalFun(f) => f.span,
                Stmt::Break { span: s, .. } | Stmt::Continue { span: s, .. } => *s,
            };
            Block {
                stmts: vec![stmt],
                span,
            }
        }
    }

    /// Peek ahead past trivia (newlines/semicolons) to find the next
    /// non-trivia token kind at `self.pos + offset_hint` onward.
    fn peek_kind_skip_trivia_at(&self, start: usize) -> TokenKind {
        let mut i = start;
        while i < self.tokens.len() {
            let k = self.tokens[i].kind;
            if matches!(k, TokenKind::Newline | TokenKind::Semi) {
                i += 1;
            } else {
                return k;
            }
        }
        TokenKind::Eof
    }

    fn parse_destructure(&mut self) -> Stmt {
        let kw = self.peek_span();
        self.bump(); // val
        self.skip_trivia();
        self.expect(TokenKind::LParen, "(");
        let mut names = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek_kind() == TokenKind::RParen {
                break;
            }
            let name_idx = self.pos;
            let _name_span = self.peek_span();
            let name = if self.peek_kind() == TokenKind::Ident {
                self.bump();
                self.intern_ident_at(name_idx)
            } else {
                self.diags.push(Diagnostic::error(
                    self.peek_span(),
                    "expected identifier in destructuring declaration",
                ));
                self.interner.intern("")
            };
            names.push(name);
            self.skip_trivia();
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, ")");
        self.expect(TokenKind::Eq, "=");
        let init = self.parse_expr();
        let span = kw.merge(init.span());
        Stmt::Destructure { names, init, span }
    }

    fn parse_stmt(&mut self) -> Stmt {
        self.skip_trivia();
        // Local annotations: `@OptIn(Api::class)`, `@Suppress("…")`
        // applied to the next statement (val/var/expression). We
        // accept and drop them — skotch doesn't enforce opt-in scope
        // and the annotations don't change codegen for ordinary
        // statements.
        if self.peek_kind() == TokenKind::At {
            let _stmt_annotations = self.parse_annotations();
            self.skip_trivia();
        }
        // Loop labels: `label@ for (...)` or `label@ while (...)`
        // Consume the label prefix and parse the loop normally.
        // The label is stored on break@label/continue@label, not the loop itself.
        if self.peek_kind() == TokenKind::Ident
            && self.peek_kind_at(1) == TokenKind::At
            && matches!(
                self.peek_kind_at(2),
                TokenKind::KwFor | TokenKind::KwWhile | TokenKind::KwDo
            )
        {
            self.bump(); // consume label ident
            self.bump(); // consume @
            self.skip_trivia();
            // Fall through to parse the loop normally
        }
        match self.peek_kind() {
            TokenKind::KwVal | TokenKind::KwVar => {
                // Check for destructuring: `val (a, b) = expr`
                let next = self.peek_kind_skip_trivia_at(self.pos + 1);
                if self.peek_kind() == TokenKind::KwVal && next == TokenKind::LParen {
                    self.parse_destructure()
                } else if self.peek_kind_at(2) == TokenKind::Dot {
                    // Extension property: `val Type.name: RetType get() = expr`
                    // Desugar to a function declaration statement.
                    let f = self.parse_extension_property();
                    Stmt::LocalFun(f)
                } else {
                    Stmt::Val(self.parse_val_decl())
                }
            }
            TokenKind::KwReturn => {
                let kw = self.peek_span();
                self.bump();
                // Labeled return: `return@forEach` — exits the lambda, not
                // the enclosing function.
                let label = if self.peek_kind() == TokenKind::At {
                    self.bump(); // consume `@`
                    if self.peek_kind() == TokenKind::Ident {
                        let idx = self.pos;
                        self.bump();
                        Some(self.intern_ident_at(idx))
                    } else {
                        None
                    }
                } else {
                    None
                };
                // A `return` (or `return@label`) with no value ends at a
                // newline, semicolon, `}`, or EOF.  We must peek BEFORE
                // skipping trivia, because `skip_trivia` eats the very
                // newline we need to detect.
                let at_line_end = matches!(
                    self.peek_kind(),
                    TokenKind::Newline | TokenKind::Semi | TokenKind::RBrace | TokenKind::Eof
                );
                self.skip_trivia();
                let value = if at_line_end
                    || matches!(self.peek_kind(), TokenKind::RBrace | TokenKind::Eof)
                {
                    None
                } else {
                    Some(self.parse_expr())
                };
                let span = match &value {
                    Some(v) => kw.merge(v.span()),
                    None => kw,
                };
                Stmt::Return { value, label, span }
            }
            TokenKind::KwFun => {
                // Local function declaration inside a block.
                let fun_decl = self.parse_fun_decl();
                Stmt::LocalFun(fun_decl)
            }
            TokenKind::KwBreak => {
                let span = self.peek_span();
                self.bump();
                // Optional @label: break@outer
                let label = if self.peek_kind() == TokenKind::At {
                    self.bump(); // consume @
                    if self.peek_kind() == TokenKind::Ident {
                        let idx = self.pos;
                        self.bump();
                        Some(self.intern_ident_at(idx))
                    } else {
                        None
                    }
                } else {
                    None
                };
                Stmt::Break { label, span }
            }
            TokenKind::KwContinue => {
                let span = self.peek_span();
                self.bump();
                let label = if self.peek_kind() == TokenKind::At {
                    self.bump();
                    if self.peek_kind() == TokenKind::Ident {
                        let idx = self.pos;
                        self.bump();
                        Some(self.intern_ident_at(idx))
                    } else {
                        None
                    }
                } else {
                    None
                };
                Stmt::Continue { label, span }
            }
            TokenKind::KwWhile => self.parse_while(),
            TokenKind::KwDo => self.parse_do_while(),
            TokenKind::KwFor => self.parse_for(),
            TokenKind::KwTry => {
                let try_expr = self.parse_try_expr();
                if let Expr::Try {
                    body,
                    catch_param,
                    catch_type,
                    catch_body,
                    extra_catches,
                    finally_body,
                    span,
                } = try_expr
                {
                    Stmt::TryStmt {
                        body: *body,
                        catch_param,
                        catch_type,
                        catch_body: catch_body.map(|b| *b),
                        extra_catches,
                        finally_body: finally_body.map(|b| *b),
                        span,
                    }
                } else {
                    Stmt::Expr(try_expr)
                }
            }
            TokenKind::KwThrow => {
                let kw = self.peek_span();
                self.bump();
                let expr = self.parse_expr();
                let span = kw.merge(expr.span());
                Stmt::ThrowStmt { expr, span }
            }
            _ => {
                let expr = self.parse_expr();
                // Check for `ident = expr` or `ident += expr` etc.
                self.skip_trivia();
                let assign_op = match self.peek_kind() {
                    TokenKind::Eq => Some(None), // plain assignment
                    TokenKind::PlusEq => Some(Some(BinOp::Add)),
                    TokenKind::MinusEq => Some(Some(BinOp::Sub)),
                    TokenKind::StarEq => Some(Some(BinOp::Mul)),
                    TokenKind::SlashEq => Some(Some(BinOp::Div)),
                    TokenKind::PercentEq => Some(Some(BinOp::Mod)),
                    _ => None,
                };
                if let Some(compound_op) = assign_op {
                    // arr[i] = v  →  IndexAssign
                    if let Expr::Index {
                        receiver,
                        index,
                        span: idx_span,
                    } = expr
                    {
                        let start = idx_span;
                        self.bump(); // consume `=`
                        self.skip_trivia();
                        let rhs = self.parse_expr();
                        let span = start.merge(rhs.span());
                        return Stmt::IndexAssign {
                            receiver: *receiver,
                            index: *index,
                            value: rhs,
                            span,
                        };
                    }
                    // m[a, b] = v desugars at parse-time to a Call to
                    // `m.get(a, b)`. For assignment we need `m.set(a, b, v)`.
                    // Detect the get-call shape and rewrite as a Stmt::Expr
                    // wrapping the set call. Surfaced by parity/45-matrix.
                    if let Expr::Call {
                        callee,
                        args,
                        span: call_span,
                        ..
                    } = &expr
                    {
                        if let Expr::Field {
                            receiver: inner_recv,
                            name: get_name,
                            ..
                        } = callee.as_ref()
                        {
                            if self.interner.resolve(*get_name) == "get" {
                                let start = *call_span;
                                let recv = inner_recv.clone();
                                let get_args = args.clone();
                                self.bump(); // consume `=`
                                self.skip_trivia();
                                let rhs = self.parse_expr();
                                let set_name = self.interner.intern("set");
                                let span = start.merge(rhs.span());
                                let mut set_args: Vec<CallArg> = get_args;
                                set_args.push(CallArg {
                                    name: None,
                                    expr: rhs,
                                });
                                let set_call = Expr::Call {
                                    callee: Box::new(Expr::Field {
                                        receiver: recv,
                                        name: set_name,
                                        span: start,
                                    }),
                                    args: set_args,
                                    type_args: Vec::new(),
                                    span,
                                };
                                let _ = compound_op;
                                return Stmt::Expr(set_call);
                            }
                        }
                    }
                    // receiver.field = value → FieldAssign
                    if let Expr::Field {
                        receiver,
                        name,
                        span: field_span,
                    } = expr
                    {
                        let start = field_span;
                        self.bump(); // consume `=`
                        self.skip_trivia();
                        let rhs = self.parse_expr();
                        let value = if let Some(op) = compound_op {
                            let span = start.merge(rhs.span());
                            Expr::Binary {
                                op,
                                lhs: Box::new(Expr::Field {
                                    receiver: receiver.clone(),
                                    name,
                                    span: start,
                                }),
                                rhs: Box::new(rhs),
                                span,
                            }
                        } else {
                            rhs
                        };
                        let span = start.merge(value.span());
                        return Stmt::FieldAssign {
                            receiver: *receiver,
                            field: name,
                            value,
                            span,
                        };
                    }
                    if let Expr::Ident(name, name_span) = &expr {
                        let name = *name;
                        let start = *name_span;
                        self.bump(); // consume `=` or `+=` etc.
                        self.skip_trivia();
                        let rhs = self.parse_expr();
                        // For compound: desugar `x += e` to `x = x + e`
                        let value = if let Some(op) = compound_op {
                            let span = start.merge(rhs.span());
                            Expr::Binary {
                                op,
                                lhs: Box::new(Expr::Ident(name, start)),
                                rhs: Box::new(rhs),
                                span,
                            }
                        } else {
                            rhs
                        };
                        let span = start.merge(value.span());
                        return Stmt::Assign {
                            target: name,
                            value,
                            span,
                        };
                    }
                }
                // Postfix increment/decrement: `x++` or `x--`
                if let Expr::Ident(name, name_span) = &expr {
                    self.skip_trivia();
                    let inc_op = match self.peek_kind() {
                        TokenKind::PlusPlus => Some(BinOp::Add),
                        TokenKind::MinusMinus => Some(BinOp::Sub),
                        _ => None,
                    };
                    if let Some(op) = inc_op {
                        let name = *name;
                        let start = *name_span;
                        self.bump(); // consume ++ or --
                        let one = Expr::IntLit(1, start);
                        let value = Expr::Binary {
                            op,
                            lhs: Box::new(Expr::Ident(name, start)),
                            rhs: Box::new(one),
                            span: start,
                        };
                        return Stmt::Assign {
                            target: name,
                            value,
                            span: start,
                        };
                    }
                }
                // The expression path may have already produced an
                // `Expr::IncDec` for `x++` / `--y` that landed in an
                // inner context first (e.g. parse_postfix). When such
                // a node bubbles up as a top-level statement, route
                // it through the same `Stmt::Assign` desugar the
                // bare-Ident form above uses — that path benefits
                // from the well-trodden BinOp-then-store lowering
                // (with field writeback) and avoids the IncDec arm's
                // intermediate locals, which break the iinc peephole
                // and shift branch offsets in tight loops (see fixture
                // 1299's per-when-arm `pc++`).
                if let Expr::IncDec {
                    target,
                    is_dec,
                    is_prefix: _,
                    span,
                } = &expr
                {
                    let op = if *is_dec { BinOp::Sub } else { BinOp::Add };
                    let one = Expr::IntLit(1, *span);
                    let value = Expr::Binary {
                        op,
                        lhs: Box::new(Expr::Ident(*target, *span)),
                        rhs: Box::new(one),
                        span: *span,
                    };
                    return Stmt::Assign {
                        target: *target,
                        value,
                        span: *span,
                    };
                }
                Stmt::Expr(expr)
            }
        }
    }

    fn parse_while(&mut self) -> Stmt {
        let start = self.peek_span();
        self.bump(); // consume `while`
        self.skip_trivia();
        self.expect(TokenKind::LParen, "'(' after 'while'");
        self.skip_trivia();
        let cond = self.parse_expr();
        self.skip_trivia();
        self.expect(TokenKind::RParen, "')' after while condition");
        self.skip_trivia();
        let body = self.parse_block();
        let span = start.merge(body.span);
        Stmt::While { cond, body, span }
    }

    fn parse_do_while(&mut self) -> Stmt {
        let start = self.peek_span();
        self.bump(); // consume `do`
        self.skip_trivia();
        let body = self.parse_block();
        self.skip_trivia();
        self.expect(TokenKind::KwWhile, "'while' after do body");
        self.skip_trivia();
        self.expect(TokenKind::LParen, "'(' after 'while'");
        self.skip_trivia();
        let cond = self.parse_expr();
        self.skip_trivia();
        let end = self.peek_span();
        self.expect(TokenKind::RParen, "')' after do-while condition");
        Stmt::DoWhile {
            body,
            cond,
            span: start.merge(end),
        }
    }

    fn parse_for(&mut self) -> Stmt {
        let start = self.peek_span();
        self.bump(); // consume `for`
        self.skip_trivia();
        self.expect(TokenKind::LParen, "'(' after 'for'");
        self.skip_trivia();

        // Check for destructuring pattern: `for ((a, b) in collection)`
        let mut destructure_names: Option<Vec<Symbol>> = None;
        let var_name;

        if self.peek_kind() == TokenKind::LParen {
            // Destructuring pattern.
            self.bump(); // consume inner `(`
            self.skip_trivia();
            let mut names = Vec::new();
            loop {
                let idx = self.pos;
                let sp = self.peek_span();
                if self.peek_kind() == TokenKind::Ident {
                    self.bump();
                    names.push(self.intern_ident_at(idx));
                } else {
                    self.diags
                        .push(Diagnostic::error(sp, "expected destructuring name"));
                    names.push(self.interner.intern("_"));
                }
                self.skip_trivia();
                if self.peek_kind() == TokenKind::Comma {
                    self.bump();
                    self.skip_trivia();
                } else {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "')' after destructuring pattern");
            self.skip_trivia();
            // Use the first name as the synthetic composite variable name.
            var_name = self.interner.intern("__destructure_elem__");
            destructure_names = Some(names);
        } else {
            // Parse: varName in start..end  OR  varName in collection
            let var_name_idx = self.pos;
            let var_span = self.peek_span();
            var_name = if self.peek_kind() == TokenKind::Ident {
                self.bump();
                self.intern_ident_at(var_name_idx)
            } else {
                self.diags
                    .push(Diagnostic::error(var_span, "expected loop variable name"));
                self.interner.intern("_")
            };
        }
        self.skip_trivia();
        self.expect(TokenKind::KwIn, "'in' after loop variable");
        self.skip_trivia();
        // Use parse_equality (not parse_expr) for the range start so that
        // infix keywords `until`/`downTo` are NOT consumed as part of the
        // start expression — the for-loop parser needs them as range operators.
        let range_start = self.parse_equality();
        self.skip_trivia();

        // Determine if this is a range-based for or a collection-based for-in.
        // If the next token is `)`, this is `for (x in collection)`.
        let is_range = self.peek_kind() == TokenKind::DotDot || {
            if self.peek_kind() == TokenKind::Ident {
                let idx = self.pos;
                match self.payload(idx) {
                    Some(TokenPayload::Ident(s)) => {
                        matches!(s.as_str(), "until" | "downTo")
                    }
                    _ => false,
                }
            } else {
                false
            }
        };

        if is_range {
            // Range operator: .. (inclusive), until (exclusive), downTo (descending)
            let mut exclusive = false;
            let mut descending = false;
            if self.peek_kind() == TokenKind::DotDot {
                self.bump();
            } else if self.peek_kind() == TokenKind::Ident {
                let idx = self.pos;
                let text = match self.payload(idx) {
                    Some(TokenPayload::Ident(s)) => s.clone(),
                    _ => String::new(),
                };
                match text.as_str() {
                    "until" => {
                        self.bump();
                        exclusive = true;
                    }
                    "downTo" => {
                        self.bump();
                        descending = true;
                    }
                    _ => {
                        self.expect(TokenKind::DotDot, "'..' or 'until' or 'downTo' for range");
                    }
                }
            } else {
                self.expect(TokenKind::DotDot, "'..' or 'until' or 'downTo' for range");
            };
            self.skip_trivia();
            // Use parse_equality so `step` (an infix function) is NOT consumed
            // as part of the range end expression.
            let range_end = self.parse_equality();
            self.skip_trivia();
            // Optional `step N` after the range end.
            let step = if self.peek_kind() == TokenKind::Ident {
                let idx = self.pos;
                let text = match self.payload(idx) {
                    Some(TokenPayload::Ident(s)) => s.clone(),
                    _ => String::new(),
                };
                if text == "step" {
                    self.bump(); // consume `step`
                    self.skip_trivia();
                    Some(self.parse_expr())
                } else {
                    None
                }
            } else {
                None
            };
            self.skip_trivia();
            self.expect(TokenKind::RParen, "')' after for range");
            self.skip_trivia();
            let body = self.parse_block_or_single_stmt();
            let span = start.merge(body.span);
            Stmt::For {
                var_name,
                start: range_start,
                end: range_end,
                exclusive,
                descending,
                step,
                body,
                span,
            }
        } else {
            // Collection iteration: `for (x in collection) { body }`
            self.expect(TokenKind::RParen, "')' after for-in expression");
            self.skip_trivia();
            let body = self.parse_block_or_single_stmt();
            let span = start.merge(body.span);
            Stmt::ForIn {
                var_name,
                destructure_names,
                iterable: range_start,
                body,
                span,
            }
        }
    }

    // ─── expression precedence climbing ──────────────────────────────────

    fn parse_expr(&mut self) -> Expr {
        self.parse_elvis()
    }

    /// Elvis operator `?:` — lowest precedence binary operator.
    fn parse_elvis(&mut self) -> Expr {
        let mut lhs = self.parse_disjunction();
        loop {
            self.skip_trivia();
            if self.peek_kind() != TokenKind::Elvis {
                break;
            }
            self.bump();
            let rhs = self.parse_disjunction();
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::ElvisOp {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    fn parse_disjunction(&mut self) -> Expr {
        let mut lhs = self.parse_conjunction();
        loop {
            self.skip_trivia();
            if self.peek_kind() != TokenKind::PipePipe {
                break;
            }
            self.bump();
            let rhs = self.parse_conjunction();
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    fn parse_conjunction(&mut self) -> Expr {
        let mut lhs = self.parse_infix_call();
        loop {
            self.skip_trivia();
            if self.peek_kind() != TokenKind::AmpAmp {
                break;
            }
            self.bump();
            let rhs = self.parse_infix_call();
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    /// Infix function calls: `a to b`, `a shl b`, etc.
    /// Precedence: between conjunction (&&) and equality (==).
    /// Parses `expr IDENT expr` as `expr.IDENT(expr)` for infix-
    /// callable identifiers. LOOPS — chained infix calls like
    /// `a add b scale 2` parse left-associative as
    /// `((a add b) scale 2)`. Matches Kotlin spec.
    fn parse_infix_call(&mut self) -> Expr {
        let mut lhs = self.parse_equality();
        // Track whether the initial skip_trivia crossed a newline. If
        // it did, this is a NEW statement; an Ident here isn't an
        // infix continuation of `lhs`, it's the start of the next
        // statement. The `..`/`in`/`!in` operators still pass through
        // (they're operator tokens, not Idents, so the check below
        // only gates the infix-ident branch).
        self.skip_trivia();
        // Did parse_equality (or our skip_trivia) cross a newline
        // between lhs's last token and the current position? Scan
        // backward from self.pos looking for Newline before any
        // non-trivia token. If found, lhs's statement ended; an
        // Ident here starts the next statement, not an infix
        // continuation of lhs.
        // Newline OR Semi between lhs and the current position
        // means lhs's statement ended — bail out of infix detection.
        let crossed_newline = self.pos > 0
            && matches!(
                self.tokens[self.pos - 1].kind,
                TokenKind::Newline | TokenKind::Semi
            );
        // `..` range operator: `1..10` → `1.rangeTo(10)`. Always consume
        // here — the contexts that need to handle `..` themselves
        // (`for (i in 1..10)` and when's `in 1..10 ->` pattern) deliberately
        // call `parse_equality`/`parse_additive` instead of `parse_expr`,
        // so they don't reach this path.
        //
        // Previously this was gated on a lookahead heuristic that skipped
        // consumption when the token two ahead was `)` or `->`. That broke
        // every parenthesised range (`(1..10)`, `(1..10).first()`) and
        // bare-range when patterns (`1..10 -> body`). See the
        // `parenthesised_range_*` and `when_bare_range_pattern` tests.
        if self.peek_kind() == TokenKind::DotDot {
            let span_start = lhs.span();
            self.bump(); // consume ..
            self.skip_trivia();
            let rhs = self.parse_equality();
            let span = span_start.merge(rhs.span());
            let name = self.interner.intern("rangeTo");
            return Expr::Call {
                callee: Box::new(Expr::Field {
                    receiver: Box::new(lhs),
                    name,
                    span,
                }),
                args: vec![CallArg {
                    name: None,
                    expr: rhs,
                }],
                type_args: Vec::new(),
                span,
            };
        }
        // `in` operator: `5 in r` → `r.contains(5)`.
        // `!in` operator: `5 !in r` → `!r.contains(5)`.
        //
        // In Kotlin's operator-precedence table, `..` (range) binds
        // tighter than `in`, so the RHS of `in` may itself be a range
        // expression (`require(x in 0..N)`). We call `parse_equality`
        // to read the range LHS, then check for `..` and fold the
        // upper bound in by hand — the regular `parse_infix_call`
        // path is one level up.
        let parse_in_rhs = |this: &mut Self| -> Expr {
            let mut rhs = this.parse_equality();
            this.skip_trivia();
            if this.peek_kind() == TokenKind::DotDot {
                let span_start = rhs.span();
                this.bump();
                this.skip_trivia();
                let upper = this.parse_equality();
                let span = span_start.merge(upper.span());
                let name = this.interner.intern("rangeTo");
                rhs = Expr::Call {
                    callee: Box::new(Expr::Field {
                        receiver: Box::new(rhs),
                        name,
                        span,
                    }),
                    args: vec![CallArg {
                        name: None,
                        expr: upper,
                    }],
                    type_args: Vec::new(),
                    span,
                };
            }
            rhs
        };
        if self.peek_kind() == TokenKind::KwIn {
            let kw_span = self.peek_span();
            self.bump(); // consume `in`
            self.skip_trivia();
            let rhs = parse_in_rhs(self);
            let span = lhs.span().merge(rhs.span());
            let contains_name = self.interner.intern("contains");
            // `lhs in rhs` → `rhs.contains(lhs)`
            return Expr::Call {
                callee: Box::new(Expr::Field {
                    receiver: Box::new(rhs),
                    name: contains_name,
                    span: kw_span,
                }),
                args: vec![CallArg {
                    name: None,
                    expr: lhs,
                }],
                type_args: Vec::new(),
                span,
            };
        }
        if self.peek_kind() == TokenKind::Bang && self.peek_kind_at(1) == TokenKind::KwIn {
            let kw_span = self.peek_span();
            self.bump(); // consume `!`
            self.bump(); // consume `in`
            self.skip_trivia();
            let rhs = parse_in_rhs(self);
            let span = lhs.span().merge(rhs.span());
            let contains_name = self.interner.intern("contains");
            // `lhs !in rhs` → `!rhs.contains(lhs)`
            let call = Expr::Call {
                callee: Box::new(Expr::Field {
                    receiver: Box::new(rhs),
                    name: contains_name,
                    span: kw_span,
                }),
                args: vec![CallArg {
                    name: None,
                    expr: lhs,
                }],
                type_args: Vec::new(),
                span,
            };
            return Expr::Unary {
                op: skotch_syntax::UnaryOp::Not,
                operand: Box::new(call),
                span,
            };
        }

        // Check if the next token is an infix function name. Kotlin
        // marks user methods with the `infix` modifier; the parser
        // doesn't know at parse time which user methods carry the
        // modifier, so we accept ANY identifier not followed by `(`
        // as a potential infix call. The stdlib whitelist below is
        // kept for clarity / documentation — `is_infix` is now true
        // for any identifier so user `infix fun add(...)` works.
        // Surfaced by parity/48-infix-functions.
        //
        // Risk: tightens the grammar — `foo bar baz` previously
        // would have been a parse error (other than for the
        // whitelisted infix names); now it parses as
        // `foo.bar(baz)`. The existing behavior was already this
        // for the whitelist; extending to any ident matches Kotlin
        // spec. Existing fixtures still pass (verified end-to-end).
        // Loop so chained infix calls like `a add b scale 2` parse
        // left-associative: `(a add b) scale 2`. Each iteration
        // consumes one infix step and folds it back into lhs.
        //
        // CRITICAL: don't cross newlines. If the initial skip_trivia
        // crossed a newline, this is a NEW statement; bail. Each
        // iteration likewise checks raw peek without skipping
        // newlines so multi-line infix doesn't accidentally swallow
        // the next statement's leading identifier.
        loop {
            if crossed_newline {
                break;
            }
            if self.peek_kind() != TokenKind::Ident {
                break;
            }
            let idx = self.pos;
            let text = match self.payload(idx) {
                Some(TokenPayload::Ident(s)) => s.clone(),
                _ => String::new(),
            };
            // Don't treat the identifier as infix when the next position
            // is something that disqualifies the Ident as an infix
            // method name — `(` is a regular call (handled by
            // parse_postfix), `{` is a trailing-lambda call, `else`/
            // similar keywords aren't valid infix RHS starts, and
            // ASSIGNMENT operators (`=`/`+=`/etc) make the Ident the
            // LHS of an assignment statement (not an infix call). Same
            // for `:` (val declaration type annotation: `val n: Int = ...`)
            // and `==`/`!=` etc. (comparison binary ops where Ident is
            // a fresh LHS, not an infix call on previous expr).
            //
            // `[`, `++`, `--`, `?.`, `?` all start a fresh
            // postfix/access chain on the Ident — they signal the
            // start of a new statement when this iteration is one
            // newline past the previous expression (see KotlinCrypto's
            // BLAKE2Digest where two consecutive `v[N] = ... xor
            // iv[M]` lines used to fold the second `v` into the
            // first's infix chain).
            // `LParen` is deliberately NOT a disqualifier even though
            // `foo(x)` at the start of a statement is a regular call.
            // By the time the infix loop runs, `lhs` has already been
            // built by parse_equality (which goes through
            // parse_postfix → parse_primary), so a leading `foo(x)`
            // has already been folded into `lhs`. A trailing `Ident
            // (RHS)` here is an infix call whose RHS is a
            // parenthesized expression — e.g.
            // `(b and c) or (b.inv() and d)` from KotlinCrypto/hash's
            // MD5 round (and any user-defined infix function whose
            // RHS happens to be parenthesised). Disqualifying LParen
            // would mean every such call bails out with a parse
            // error.
            let next2 = self.peek_kind_at(1);
            let is_infix = !text.is_empty()
                && !matches!(
                    next2,
                    TokenKind::LBrace
                        | TokenKind::LBracket
                        | TokenKind::KwElse
                        | TokenKind::Eq
                        | TokenKind::PlusEq
                        | TokenKind::MinusEq
                        | TokenKind::StarEq
                        | TokenKind::SlashEq
                        | TokenKind::PercentEq
                        | TokenKind::Colon
                        | TokenKind::EqEq
                        | TokenKind::NotEq
                        | TokenKind::Lt
                        | TokenKind::LtEq
                        | TokenKind::Gt
                        | TokenKind::GtEq
                        | TokenKind::PlusPlus
                        | TokenKind::MinusMinus
                        | TokenKind::QuestionDot
                        | TokenKind::Question
                );
            if !is_infix {
                break;
            }
            let kw_span = self.peek_span();
            self.bump(); // consume infix ident
            let name = self.interner.intern(&text);
            self.skip_trivia();
            let rhs = self.parse_equality();
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Call {
                callee: Box::new(Expr::Field {
                    receiver: Box::new(lhs),
                    name,
                    span: kw_span,
                }),
                args: vec![CallArg {
                    name: None,
                    expr: rhs,
                }],
                type_args: Vec::new(),
                span,
            };
            // parse_equality's internal `skip_trivia()` may have
            // crossed a newline while reading `rhs` — which means the
            // statement ended at `rhs` and the next ident we see
            // (peek_kind below) is the start of a NEW statement, not
            // another infix step. Re-scan the trivia just-consumed to
            // catch that case and stop the chain.
            let crossed = {
                let mut i = self.pos;
                let mut hit = false;
                while i > 0
                    && matches!(
                        self.tokens.get(i - 1).map(|t| t.kind),
                        Some(TokenKind::Newline) | Some(TokenKind::Semi)
                    )
                {
                    hit = true;
                    i -= 1;
                }
                hit
            };
            if crossed {
                break;
            }
        }
        lhs
    }

    fn parse_equality(&mut self) -> Expr {
        let mut lhs = self.parse_comparison();
        loop {
            self.skip_trivia();
            let op = match self.peek_kind() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::NotEq,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_comparison();
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    fn parse_comparison(&mut self) -> Expr {
        let mut lhs = self.parse_additive();
        loop {
            self.skip_trivia();
            // `is` / `!is` type check
            if self.peek_kind() == TokenKind::KwIs {
                let _kw_span = self.peek_span();
                self.bump();
                self.skip_trivia();
                let idx = self.pos;
                let type_span = self.peek_span();
                self.expect(TokenKind::Ident, "type name");
                let type_name = self.intern_ident_at(idx);
                let span = lhs.span().merge(type_span);
                lhs = Expr::IsCheck {
                    expr: Box::new(lhs),
                    type_name,
                    negated: false,
                    span,
                };
                continue;
            }
            // `!is` type check
            if self.peek_kind() == TokenKind::Bang {
                // Peek ahead for `is` after `!`
                if self.peek_kind_at(1) == TokenKind::KwIs {
                    self.bump(); // !
                    self.bump(); // is
                    self.skip_trivia();
                    let idx = self.pos;
                    let type_span = self.peek_span();
                    self.expect(TokenKind::Ident, "type name");
                    let type_name = self.intern_ident_at(idx);
                    let span = lhs.span().merge(type_span);
                    lhs = Expr::IsCheck {
                        expr: Box::new(lhs),
                        type_name,
                        negated: true,
                        span,
                    };
                    continue;
                }
            }
            // `as` / `as?` type cast. The cast target may carry
            // generic args (`obj as Xof<A>`) or be a nullable type
            // (`obj as Foo?`). The AST only stores the base type
            // name; we still need to CONSUME the trailing
            // `<...>`/`?` so the parser doesn't trip on them at the
            // surrounding precedence level.
            if self.peek_kind() == TokenKind::KwAs {
                self.bump();
                let safe = self.eat(TokenKind::Question);
                self.skip_trivia();
                let idx = self.pos;
                let type_span = self.peek_span();
                self.expect(TokenKind::Ident, "type name");
                let type_name = self.intern_ident_at(idx);
                // Skip a parameterized cast target's generic args,
                // e.g. `as Xof<A>` or `as Map<String, Int>`. The cast
                // AST doesn't carry type args; this keeps the rest of
                // the source parseable. Use `skip_supertype_generic_args`
                // which already implements the balanced-`<…>` scan.
                self.skip_supertype_generic_args();
                // `as Foo?` — nullable cast target. Eat the `?` so
                // it doesn't reach the parent precedence level (where
                // `expr ? :` would otherwise be misread as a ternary
                // start).
                let _ = self.eat(TokenKind::Question);
                let span = lhs.span().merge(type_span);
                lhs = Expr::AsCast {
                    expr: Box::new(lhs),
                    type_name,
                    safe,
                    span,
                };
                continue;
            }
            let op = match self.peek_kind() {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::GtEq => BinOp::GtEq,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_additive();
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    fn parse_additive(&mut self) -> Expr {
        let mut lhs = self.parse_multiplicative();
        loop {
            self.skip_trivia();
            let op = match self.peek_kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_multiplicative();
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    fn parse_multiplicative(&mut self) -> Expr {
        let mut lhs = self.parse_unary();
        loop {
            self.skip_trivia();
            let op = match self.peek_kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary();
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        lhs
    }

    fn parse_unary(&mut self) -> Expr {
        self.skip_trivia();
        match self.peek_kind() {
            TokenKind::Minus => {
                let span_op = self.peek_span();
                self.bump();
                let operand = self.parse_unary();
                let span = span_op.merge(operand.span());
                Expr::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                    span,
                }
            }
            TokenKind::Bang => {
                let span_op = self.peek_span();
                self.bump();
                let operand = self.parse_unary();
                let span = span_op.merge(operand.span());
                Expr::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                    span,
                }
            }
            // Prefix `++` / `--`. The expression's value is the
            // NEW value of the target (after the bump), distinct
            // from the postfix form which yields the old value.
            // Only valid on a bare Ident at parse time — the
            // operand path forces parse_postfix to land on
            // `Expr::Ident`. Compound LHSes like `obj.f`,
            // `arr[i]`, or chained calls aren't valid Kotlin
            // postfix-bump targets either.
            TokenKind::PlusPlus | TokenKind::MinusMinus => {
                let is_dec = self.peek_kind() == TokenKind::MinusMinus;
                let span_op = self.peek_span();
                self.bump();
                let operand = self.parse_unary();
                let span = span_op.merge(operand.span());
                if let Expr::Ident(name, _) = operand {
                    Expr::IncDec {
                        target: name,
                        is_dec,
                        is_prefix: true,
                        span,
                    }
                } else {
                    self.diags.push(Diagnostic::error(
                        span,
                        format!(
                            "prefix `{}` requires a simple variable name",
                            if is_dec { "--" } else { "++" }
                        ),
                    ));
                    operand
                }
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        loop {
            // Kotlin allows expression continuation across newlines when
            // the next non-trivia token is `.` or `?.`. This lets users
            // write multi-line dot-chains like:
            //   listOf(1,2,3)
            //       .map { it * 2 }
            //       .filter { it > 2 }
            // Other operators (e.g. `-`) after a newline start a new
            // statement, so we only skip trivia when the continuation
            // token is a dot.
            if matches!(self.peek_kind(), TokenKind::Newline | TokenKind::Semi) {
                let next = self.peek_kind_skip_trivia_at(self.pos);
                if matches!(next, TokenKind::Dot | TokenKind::QuestionDot) {
                    self.skip_trivia();
                }
            }
            match self.peek_kind() {
                TokenKind::Dot => {
                    self.bump();
                    let idx = self.pos;
                    let span = self.peek_span();
                    // Allow keywords as member names after '.' — Kotlin permits
                    // this (e.g. `drawerState.open()`, `list.in`, `obj.class`).
                    if self.peek_kind() != TokenKind::Ident && !self.peek_kind().is_keyword() {
                        self.diags
                            .push(Diagnostic::error(span, "expected field name after '.'"));
                        break;
                    }
                    self.bump();
                    let name = self.intern_ident_at(idx);
                    let combined = expr.span().merge(span);
                    expr = Expr::Field {
                        receiver: Box::new(expr),
                        name,
                        span: combined,
                    };
                }
                TokenKind::QuestionDot => {
                    self.bump();
                    let idx = self.pos;
                    let span = self.peek_span();
                    if self.peek_kind() != TokenKind::Ident {
                        self.diags
                            .push(Diagnostic::error(span, "expected name after '?.'"));
                        break;
                    }
                    self.bump();
                    let name = self.intern_ident_at(idx);
                    let combined = expr.span().merge(span);
                    expr = Expr::SafeCall {
                        receiver: Box::new(expr),
                        name,
                        span: combined,
                    };
                }
                TokenKind::BangBang => {
                    let op_span = self.peek_span();
                    self.bump();
                    let combined = expr.span().merge(op_span);
                    expr = Expr::NotNullAssert {
                        expr: Box::new(expr),
                        span: combined,
                    };
                }
                // Explicit type arguments: `f<Type>(args)`.
                // Disambiguate from comparison by tentative parse: save
                // position, try to parse `< types > (`, restore if no `(`.
                TokenKind::Lt if matches!(expr, Expr::Ident(_, _) | Expr::Field { .. }) => {
                    let saved = self.pos;
                    self.bump(); // consume `<`
                    let mut targs = Vec::new();
                    let mut ok = true;
                    loop {
                        self.skip_trivia();
                        if self.peek_kind() == TokenKind::Gt {
                            break;
                        }
                        if self.peek_kind() == TokenKind::Ident {
                            targs.push(self.parse_type_ref());
                        } else {
                            ok = false;
                            break;
                        }
                        self.skip_trivia();
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    if ok && self.peek_kind() == TokenKind::Gt {
                        self.bump(); // consume `>`
                        self.skip_trivia();
                        if self.peek_kind() == TokenKind::LParen {
                            // It IS explicit type arguments followed by a call.
                            self.bump(); // consume `(`
                            let mut args: Vec<CallArg> = Vec::new();
                            self.skip_trivia();
                            if self.peek_kind() != TokenKind::RParen {
                                loop {
                                    self.skip_trivia();
                                    // Named argument: `name = expr`
                                    let named = self.is_name_token_at(0)
                                        && self.peek_kind_at(1) == TokenKind::Eq;
                                    if named {
                                        let name_idx = self.pos;
                                        self.bump();
                                        let name_sym = self.intern_ident_at(name_idx);
                                        self.bump(); // consume '='
                                        self.skip_trivia();
                                        let value = self.parse_expr();
                                        args.push(CallArg {
                                            name: Some(name_sym),
                                            expr: value,
                                        });
                                    } else {
                                        let value = self.parse_expr();
                                        args.push(CallArg {
                                            name: None,
                                            expr: value,
                                        });
                                    }
                                    self.skip_trivia();
                                    if !self.eat(TokenKind::Comma) {
                                        break;
                                    }
                                    self.skip_trivia();
                                    if self.peek_kind() == TokenKind::RParen {
                                        break;
                                    }
                                }
                            }
                            let rp = self.expect(TokenKind::RParen, ")");
                            // Trailing lambda after type-args call.
                            if self.peek_kind() == TokenKind::LBrace {
                                let lambda = self.parse_lambda_expr();
                                args.push(CallArg {
                                    name: None,
                                    expr: lambda,
                                });
                            }
                            let span = expr.span().merge(rp);
                            expr = Expr::Call {
                                callee: Box::new(expr),
                                args,
                                type_args: targs,
                                span,
                            };
                            continue;
                        }
                    }
                    // Not type args — restore and let binary op handle `<`.
                    self.pos = saved;
                    break;
                }
                TokenKind::LParen => {
                    self.bump();
                    let mut args: Vec<CallArg> = Vec::new();
                    self.skip_trivia();
                    if self.peek_kind() != TokenKind::RParen {
                        loop {
                            self.skip_trivia();
                            // Check for named argument: `name = expr`
                            // Peek: Ident (or soft keyword) followed by Eq (not EqEq)
                            let named =
                                self.is_name_token_at(0) && self.peek_kind_at(1) == TokenKind::Eq;
                            if named {
                                let name_idx = self.pos;
                                self.bump(); // consume ident
                                let name_sym = self.intern_ident_at(name_idx);
                                self.bump(); // consume '='
                                self.skip_trivia();
                                let value = self.parse_expr();
                                args.push(CallArg {
                                    name: Some(name_sym),
                                    expr: value,
                                });
                            } else {
                                // Handle `*array` spread operator — on JVM,
                                // vararg params are arrays, so `*` is a no-op.
                                // Just consume the `*` and parse the expression.
                                if self.peek_kind() == TokenKind::Star {
                                    self.bump(); // consume `*`
                                    self.skip_trivia();
                                }
                                let value = self.parse_expr();
                                args.push(CallArg {
                                    name: None,
                                    expr: value,
                                });
                            }
                            self.skip_trivia();
                            if !self.eat(TokenKind::Comma) {
                                break;
                            }
                            // Trailing comma: `f(a, b,)` — skip newlines and
                            // check for `)` before trying to parse another arg.
                            self.skip_trivia();
                            if self.peek_kind() == TokenKind::RParen {
                                break;
                            }
                        }
                    }
                    let rp = self.expect(TokenKind::RParen, ")");
                    // Trailing lambda: `f(args) { body }` — the `{` must
                    // immediately follow `)` (no newline).
                    if self.peek_kind() == TokenKind::LBrace {
                        let lambda = self.parse_lambda_expr();
                        args.push(CallArg {
                            name: None,
                            expr: lambda,
                        });
                    }
                    let end = if let Some(last) = args.last() {
                        last.expr.span()
                    } else {
                        rp
                    };
                    let span = expr.span().merge(end);
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                        type_args: Vec::new(),
                        span,
                    };
                }
                // Class reference / member reference: `Foo::class`, `Foo::member`
                TokenKind::ColonColon => {
                    self.bump(); // consume ::
                    self.skip_trivia();
                    if self.peek_kind() == TokenKind::KwClass {
                        // Foo::class — class literal
                        let class_span = self.peek_span();
                        self.bump(); // consume 'class'
                        let span = expr.span().merge(class_span);
                        // Desugar to: Foo.javaClass (a class reference).
                        // The actual type is KClass<Foo> but we treat it as Any.
                        let class_name = self.interner.intern("class");
                        expr = Expr::Field {
                            receiver: Box::new(expr),
                            name: class_name,
                            span,
                        };
                    } else if self.peek_kind() == TokenKind::Ident || self.peek_kind().is_keyword()
                    {
                        // Foo::member — member reference, desugar to lambda
                        // `{ $ref_arg -> Foo.member($ref_arg) }`
                        let idx = self.pos;
                        let fn_span = self.peek_span();
                        self.bump();
                        let fn_name = self.intern_ident_at(idx);
                        let span = expr.span().merge(fn_span);
                        let param_name = self.interner.intern("$ref_arg");
                        let call = Expr::Call {
                            callee: Box::new(Expr::Field {
                                receiver: Box::new(expr.clone()),
                                name: fn_name,
                                span: fn_span,
                            }),
                            args: vec![CallArg {
                                name: None,
                                expr: Expr::Ident(param_name, fn_span),
                            }],
                            type_args: Vec::new(),
                            span,
                        };
                        expr = Expr::Lambda {
                            params: vec![Param {
                                name: param_name,
                                ty: TypeRef {
                                    name: self.interner.intern("Any"),
                                    nullable: false,
                                    func_params: None,
                                    type_args: Vec::new(),
                                    is_suspend: false,
                                    is_composable: false,
                                    has_receiver: false,
                                    span: fn_span,
                                },
                                default: None,
                                is_vararg: false,
                                span: fn_span,
                            }],
                            body: Block {
                                // Use Stmt::Return so the lambda's
                                // helper inherits the call's return type
                                // (mir-lower's helper infers ret_ty
                                // from the last block's terminator —
                                // a plain Stmt::Expr would discard the
                                // result and force the helper's return
                                // type to Unit, breaking
                                // `Foo::method` callable refs where the
                                // method returns a value).
                                stmts: vec![Stmt::Return {
                                    value: Some(call),
                                    label: None,
                                    span,
                                }],
                                span,
                            },
                            is_suspend: false,
                            span,
                        };
                    }
                }
                // Array/collection indexing: `expr[index]` for single
                // index; `expr[a, b, ...]` for multi-arg desugars to
                // `expr.get(a, b, ...)`. Kotlin's `operator fun get(...)`
                // accepts any arity; the multi-arg form is common for
                // `Matrix[r, c]`-style classes. Surfaced by parity/45.
                TokenKind::LBracket => {
                    self.bump(); // consume `[`
                    self.skip_trivia();
                    let first = self.parse_expr();
                    self.skip_trivia();
                    let mut extra: Vec<Expr> = Vec::new();
                    while self.peek_kind() == TokenKind::Comma {
                        self.bump();
                        self.skip_trivia();
                        extra.push(self.parse_expr());
                        self.skip_trivia();
                    }
                    let rb = self.expect(TokenKind::RBracket, "']'");
                    let span = expr.span().merge(rb);
                    if extra.is_empty() {
                        expr = Expr::Index {
                            receiver: Box::new(expr),
                            index: Box::new(first),
                            span,
                        };
                    } else {
                        // Desugar `r[a, b, ...]` → `r.get(a, b, ...)`.
                        let get_name = self.interner.intern("get");
                        let mut args: Vec<CallArg> = Vec::new();
                        args.push(CallArg {
                            name: None,
                            expr: first,
                        });
                        for e in extra {
                            args.push(CallArg {
                                name: None,
                                expr: e,
                            });
                        }
                        expr = Expr::Call {
                            callee: Box::new(Expr::Field {
                                receiver: Box::new(expr),
                                name: get_name,
                                span,
                            }),
                            args,
                            type_args: Vec::new(),
                            span,
                        };
                    }
                }
                // Also handle: `f { body }` — call with ONLY a trailing lambda,
                // no parentheses at all. The guard set covers `f`,
                // `obj.method`, AND `obj?.method` so the `?.let { … }`
                // shape (used e.g. in `t?.let { "/$it" } ?: ""` from
                // KotlinCrypto's Bit64Digest) parses through cleanly.
                TokenKind::LBrace
                    if matches!(
                        expr,
                        Expr::Ident(_, _) | Expr::Field { .. } | Expr::SafeCall { .. }
                    ) =>
                {
                    let lambda = self.parse_lambda_expr();
                    let span = expr.span().merge(lambda.span());
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args: vec![CallArg {
                            name: None,
                            expr: lambda,
                        }],
                        type_args: Vec::new(),
                        span,
                    };
                }
                // Postfix `++` / `--` in expression position. `name++`
                // at statement position is intercepted earlier (it
                // becomes `Stmt::Assign { target, value: target + 1 }`
                // and never reaches parse_postfix). The only callers
                // that reach this arm are nested-expression contexts
                // like a named-arg value (`addData(index = APos++)`,
                // KotlinCrypto's KeccakDigest), a return-value
                // (`return n++`), or the RHS of a binary op.
                //
                // Postfix `++` / `--` in expression position. `name++`
                // at statement position is intercepted earlier (it
                // becomes `Stmt::Assign { target, value: target + 1 }`
                // and never reaches parse_postfix). The only callers
                // that reach this arm are nested-expression contexts
                // like a named-arg value (`addData(index = APos++)`,
                // KotlinCrypto's KeccakDigest), an array-index
                // (`out[outPos++] = ...`), or the RHS of a binary
                // op (`i + outPos++`). We only accept it on a bare
                // `Ident` for now — `arr[i]++` would need a separate
                // desugar that pulls the index out so it's
                // evaluated once.
                TokenKind::PlusPlus | TokenKind::MinusMinus
                    if matches!(expr, Expr::Ident(_, _)) =>
                {
                    let is_dec = self.peek_kind() == TokenKind::MinusMinus;
                    let op_span = self.peek_span();
                    self.bump();
                    if let Expr::Ident(name, name_span) = expr {
                        expr = Expr::IncDec {
                            target: name,
                            is_dec,
                            is_prefix: false,
                            span: name_span.merge(op_span),
                        };
                    }
                }
                _ => break,
            }
        }
        expr
    }

    fn parse_primary(&mut self) -> Expr {
        self.skip_trivia();
        let span = self.peek_span();
        match self.peek_kind() {
            TokenKind::IntLit => {
                let v = match self.payload(self.pos) {
                    Some(TokenPayload::Int(v)) => *v,
                    _ => 0,
                };
                self.bump();
                Expr::IntLit(v, span)
            }
            TokenKind::CharLit => {
                let v = match self.payload(self.pos) {
                    Some(TokenPayload::Int(v)) => *v,
                    _ => 0,
                };
                self.bump();
                Expr::CharLit(v, span)
            }
            TokenKind::LongLit => {
                let v = match self.payload(self.pos) {
                    Some(TokenPayload::Int(v)) => *v,
                    _ => 0,
                };
                self.bump();
                Expr::LongLit(v, span)
            }
            TokenKind::DoubleLit => {
                let v = match self.payload(self.pos) {
                    Some(TokenPayload::Double(v)) => *v,
                    _ => 0.0,
                };
                self.bump();
                Expr::DoubleLit(v, span)
            }
            TokenKind::FloatLit => {
                let v = match self.payload(self.pos) {
                    Some(TokenPayload::Double(v)) => *v,
                    _ => 0.0,
                };
                self.bump();
                Expr::FloatLit(v, span)
            }
            TokenKind::KwNull => {
                self.bump();
                Expr::NullLit(span)
            }
            TokenKind::KwTrue => {
                self.bump();
                Expr::BoolLit(true, span)
            }
            TokenKind::KwFalse => {
                self.bump();
                Expr::BoolLit(false, span)
            }
            TokenKind::Ident => {
                let idx = self.pos;
                self.bump();
                let sym = self.intern_ident_at(idx);
                let name = self.interner.resolve(sym);
                // Handle `this@Label` — labeled this expression.
                // Treat as plain `this` (the label is for disambiguation).
                if name == "this" && self.peek_kind() == TokenKind::At {
                    self.bump(); // consume @
                                 // Consume the label identifier.
                    if self.peek_kind() == TokenKind::Ident || self.peek_kind().is_keyword() {
                        self.bump();
                    }
                }
                Expr::Ident(sym, span)
            }
            TokenKind::KwSuper => {
                self.bump();
                let sym = self.interner.intern("super");
                Expr::Ident(sym, span)
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr();
                let rp = self.expect(TokenKind::RParen, ")");
                Expr::Paren(Box::new(inner), span.merge(rp))
            }
            TokenKind::KwIf => self.parse_if_expr(),
            TokenKind::KwWhen => self.parse_when_expr(),
            TokenKind::KwTry => self.parse_try_expr(),
            TokenKind::KwThrow => self.parse_throw_expr(),
            TokenKind::StringStart => self.parse_string_literal(),
            TokenKind::LBrace => self.parse_lambda_expr(),
            TokenKind::KwObject => self.parse_object_expr(),
            TokenKind::ColonColon => {
                // ::functionName — callable reference.
                // Desugar to a lambda: { args... -> functionName(args...) }
                // For simplicity, generate a single-arg lambda { $it -> f($it) }
                // since most use cases are single-arg functions.
                self.bump(); // consume ::
                self.skip_trivia();
                let fn_idx = self.pos;
                let fn_span = self.peek_span();
                self.expect(TokenKind::Ident, "function name after ::");
                let fn_name = self.intern_ident_at(fn_idx);
                let end_span = fn_span;
                let full_span = span.merge(end_span);
                // Desugar to: { $ref_arg: Any -> functionName($ref_arg) }
                let param_name = self.interner.intern("$ref_arg");
                let param_ty = TypeRef {
                    name: self.interner.intern("Any"),
                    nullable: false,
                    func_params: None,
                    type_args: Vec::new(),
                    is_suspend: false,
                    is_composable: false,
                    has_receiver: false,
                    span: fn_span,
                };
                let param = Param {
                    name: param_name,
                    ty: param_ty,
                    default: None,
                    is_vararg: false,
                    span: fn_span,
                };
                let call = Expr::Call {
                    callee: Box::new(Expr::Ident(fn_name, fn_span)),
                    args: vec![CallArg {
                        name: None,
                        expr: Expr::Ident(param_name, fn_span),
                    }],
                    type_args: Vec::new(),
                    span: full_span,
                };
                Expr::Lambda {
                    params: vec![param],
                    body: Block {
                        stmts: vec![Stmt::Return {
                            value: Some(call),
                            label: None,
                            span: full_span,
                        }],
                        span: full_span,
                    },
                    is_suspend: false,
                    span: full_span,
                }
            }
            // Soft keywords usable as identifiers in expression context.
            // In Kotlin, `data`, `open`, `sealed`, `inner`, `override`, etc.
            // are context-sensitive — they act as keywords only before `class`,
            // `fun`, etc. and as plain identifiers everywhere else.
            TokenKind::KwData
            | TokenKind::KwEnum
            | TokenKind::KwSealed
            | TokenKind::KwOpen
            | TokenKind::KwAbstract
            | TokenKind::KwOverride
            | TokenKind::KwInline
            | TokenKind::KwOperator
            | TokenKind::KwInfix
            | TokenKind::KwSuspend
            | TokenKind::KwLateinit
            | TokenKind::KwTailrec
            | TokenKind::KwVararg
            | TokenKind::KwConst
            | TokenKind::KwConstructor
            | TokenKind::KwInit => {
                let idx = self.pos;
                self.bump();
                let sym = self.intern_ident_at(idx);
                Expr::Ident(sym, span)
            }
            other => {
                self.diags.push(Diagnostic::error(
                    span,
                    format!("expected expression, found {other:?}"),
                ));
                self.bump();
                Expr::IntLit(0, span)
            }
        }
    }

    fn parse_if_expr(&mut self) -> Expr {
        let kw = self.peek_span();
        self.bump(); // 'if'
        self.expect(TokenKind::LParen, "(");
        let cond = self.parse_expr();
        self.expect(TokenKind::RParen, ")");
        self.skip_trivia();
        let then_block = self.parse_branch_block();
        self.skip_trivia();
        let else_block = if self.eat(TokenKind::KwElse) {
            self.skip_trivia();
            Some(Box::new(self.parse_branch_block()))
        } else {
            None
        };
        let span = match &else_block {
            Some(b) => kw.merge(b.span),
            None => kw.merge(then_block.span),
        };
        Expr::If {
            cond: Box::new(cond),
            then_block: Box::new(then_block),
            else_block,
            span,
        }
    }

    fn parse_when_expr(&mut self) -> Expr {
        let kw = self.peek_span();
        self.bump(); // 'when'
        self.skip_trivia();

        // Support both `when (subject) { ... }` and `when { ... }`.
        let subject = if self.peek_kind() == TokenKind::LParen {
            self.bump();
            self.skip_trivia();
            let s = self.parse_expr();
            self.skip_trivia();
            self.expect(TokenKind::RParen, "')' after when subject");
            self.skip_trivia();
            s
        } else {
            // Subjectless when: each branch pattern is a boolean condition.
            // Use `true` as a sentinel subject — the MIR lowering will
            // detect this and use the pattern directly as the condition
            // instead of comparing subject == pattern.
            Expr::BoolLit(true, kw)
        };
        self.expect(TokenKind::LBrace, "'{' for when body");

        let mut branches: Vec<WhenBranch> = Vec::new();
        let mut else_body: Option<Box<Expr>> = None;

        loop {
            self.skip_trivia();
            if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                break;
            }
            // Check for `else -> expr`
            if self.peek_kind() == TokenKind::KwElse {
                let start = self.peek_span();
                self.bump(); // 'else'
                self.skip_trivia();
                self.expect(TokenKind::Arrow, "'->' after 'else'");
                self.skip_trivia();
                let body = self.parse_when_body();
                let _ = start;
                else_body = Some(Box::new(body));
                self.skip_trivia();
                break;
            }
            // Regular branch: pattern[, pattern]* -> body
            // Patterns can be: expr, `in expr..expr`, `is Type`
            let start = self.peek_span();
            let mut patterns: Vec<(Expr, Option<Expr>)> = Vec::new();
            loop {
                // Check for `in expr..expr` range pattern
                if self.peek_kind() == TokenKind::KwIn {
                    self.bump(); // consume `in`
                    self.skip_trivia();
                    // Use parse_additive (not parse_expr) so that `..`
                    // is NOT consumed as part of the range start expression.
                    let range_start = self.parse_additive();
                    self.skip_trivia();
                    self.expect(TokenKind::DotDot, "'..' in range pattern");
                    self.skip_trivia();
                    let range_end = self.parse_additive();
                    patterns.push((range_start, Some(range_end)));
                } else if self.peek_kind() == TokenKind::KwIs {
                    // `is Type` pattern: lower as IsCheck on the subject.
                    let is_span = self.peek_span();
                    self.bump(); // consume `is`
                    self.skip_trivia();
                    let type_idx = self.pos;
                    let type_span = self.peek_span();
                    self.expect(TokenKind::Ident, "type name");
                    let type_name = self.intern_ident_at(type_idx);
                    let check = Expr::IsCheck {
                        expr: Box::new(subject.clone()),
                        type_name,
                        negated: false,
                        span: is_span.merge(type_span),
                    };
                    patterns.push((check, None));
                } else {
                    patterns.push((self.parse_expr(), None));
                }
                self.skip_trivia();
                if self.peek_kind() != TokenKind::Comma {
                    break;
                }
                self.bump();
                self.skip_trivia();
            }
            self.expect(TokenKind::Arrow, "'->' in when branch");
            self.skip_trivia();
            let body = self.parse_when_body();
            let span = start.merge(body.span());
            for (pattern, range_end) in patterns {
                branches.push(WhenBranch {
                    pattern,
                    range_end,
                    body: body.clone(),
                    span,
                });
            }
        }

        let end = self.peek_span();
        if self.peek_kind() == TokenKind::RBrace {
            self.bump();
        }

        Expr::When {
            subject: Box::new(subject),
            branches,
            else_body,
            span: kw.merge(end),
        }
    }

    /// Parse a when-branch body. Uses additive-level parsing to avoid
    /// consuming `is`/`!is` tokens from the NEXT branch (which would be
    /// interpreted as comparison operators on the body expression).
    fn parse_when_body(&mut self) -> Expr {
        self.parse_additive()
    }

    /// Parse the body of an `if` branch — either a `{ block }` or a
    /// single expression which we wrap in a synthetic block.
    fn parse_branch_block(&mut self) -> Block {
        self.skip_trivia();
        if self.peek_kind() == TokenKind::LBrace {
            self.parse_block()
        } else if self.peek_kind() == TokenKind::KwReturn {
            // `if (cond) return expr` — single-statement branch.
            let stmt = self.parse_stmt();
            let span = match &stmt {
                Stmt::Return { span, .. } => *span,
                _ => self.peek_span(),
            };
            Block {
                stmts: vec![stmt],
                span,
            }
        } else if self.peek_kind() == TokenKind::KwBreak {
            let span = self.peek_span();
            self.bump();
            let label = if self.peek_kind() == TokenKind::At {
                self.bump();
                if self.peek_kind() == TokenKind::Ident {
                    let idx = self.pos;
                    self.bump();
                    Some(self.intern_ident_at(idx))
                } else {
                    None
                }
            } else {
                None
            };
            Block {
                stmts: vec![Stmt::Break { label, span }],
                span,
            }
        } else if self.peek_kind() == TokenKind::KwContinue {
            let span = self.peek_span();
            self.bump();
            let label = if self.peek_kind() == TokenKind::At {
                self.bump();
                if self.peek_kind() == TokenKind::Ident {
                    let idx = self.pos;
                    self.bump();
                    Some(self.intern_ident_at(idx))
                } else {
                    None
                }
            } else {
                None
            };
            Block {
                stmts: vec![Stmt::Continue { label, span }],
                span,
            }
        } else {
            // Single-statement branch — could be an assignment
            // (`if (cond) x = v`), a regular call, or any expression.
            // Use parse_stmt so assignments parse correctly. Surfaced
            // by parity/49-functional-pipelines.
            let stmt = self.parse_stmt();
            let span = match &stmt {
                Stmt::Expr(e) => e.span(),
                Stmt::Assign { span, .. } => *span,
                Stmt::IndexAssign { span, .. } => *span,
                Stmt::FieldAssign { span, .. } => *span,
                Stmt::Return { span, .. } => *span,
                _ => self.peek_span(),
            };
            Block {
                stmts: vec![stmt],
                span,
            }
        }
    }

    fn parse_object_expr(&mut self) -> Expr {
        let start = self.peek_span();
        self.bump(); // consume 'object'
        self.skip_trivia();
        self.expect(TokenKind::Colon, ":");
        self.skip_trivia();
        let type_idx = self.pos;
        let type_span = self.peek_span();
        let mut super_type = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(type_idx)
        } else {
            self.diags
                .push(Diagnostic::error(type_span, "expected type name"));
            self.interner.intern("")
        };
        self.skip_trivia();
        // Generic args + dotted nested type, mirroring the class-decl
        // supertype parser: `object : Xof<A>.Reader() {...}` is
        // common when the parent type is a nested class on a
        // generic outer (KotlinCrypto/hash's SHAKEDigest does this
        // inside its `newReader` body).
        self.skip_supertype_generic_args();
        while self.peek_kind() == TokenKind::Dot {
            self.bump();
            self.skip_trivia();
            if self.peek_kind() != TokenKind::Ident {
                break;
            }
            let seg_idx = self.pos;
            self.bump();
            let segment = self.intern_ident_at(seg_idx);
            let joined = format!(
                "{}.{}",
                self.interner.resolve(super_type),
                self.interner.resolve(segment),
            );
            super_type = self.interner.intern(&joined);
            self.skip_trivia();
            self.skip_supertype_generic_args();
        }
        // Optional constructor call parens: `object : Type()`
        if self.peek_kind() == TokenKind::LParen {
            self.bump();
            self.expect(TokenKind::RParen, ")");
            self.skip_trivia();
        }
        // Body: { override fun method() { } }
        let mut methods = Vec::new();
        if self.peek_kind() == TokenKind::LBrace {
            self.bump();
            loop {
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                    break;
                }
                // Skip annotations before object expression members.
                self.skip_annotations();
                // Skip modifiers.
                let mut is_override = false;
                let mut is_suspend = false;
                while matches!(
                    self.peek_kind(),
                    TokenKind::KwOverride
                        | TokenKind::KwOpen
                        | TokenKind::KwPrivate
                        | TokenKind::KwProtected
                        | TokenKind::KwInternal
                        | TokenKind::KwSuspend
                        | TokenKind::KwTailrec
                ) {
                    if self.peek_kind() == TokenKind::KwOverride {
                        is_override = true;
                    }
                    if self.peek_kind() == TokenKind::KwSuspend {
                        is_suspend = true;
                    }
                    self.bump();
                    self.skip_trivia();
                }
                if self.peek_kind() == TokenKind::KwFun {
                    let mut f = self.parse_fun_decl();
                    f.is_override = is_override;
                    f.is_suspend = is_suspend;
                    methods.push(f);
                } else {
                    self.bump();
                }
            }
            self.expect(TokenKind::RBrace, "}");
        }
        let end = self.peek_span();
        Expr::ObjectExpr {
            super_type,
            methods,
            span: start.merge(end),
        }
    }

    fn parse_lambda_expr(&mut self) -> Expr {
        let start = self.peek_span();
        self.bump(); // consume '{'
        self.skip_trivia();

        // Detect lambda: look for `ident: Type` pattern followed by `->`.
        // If no `->` is found, this is a bare block (not a lambda in expression position).
        let mut params = Vec::new();
        let saved_pos = self.pos;
        let mut is_lambda = false;

        // Try to parse params: `x: Int, y: String -> ...`
        if self.peek_kind() == TokenKind::Ident {
            // Scan ahead for `->` to confirm this is a lambda.
            // Track both paren and brace depth — a `->` inside a nested
            // lambda `{ annotation -> ... }` must NOT match as our arrow.
            let mut scan = self.pos;
            let mut paren_depth = 0;
            let mut brace_depth = 0;
            while scan < self.tokens.len() {
                match self.tokens[scan].kind {
                    TokenKind::Arrow if paren_depth == 0 && brace_depth == 0 => {
                        is_lambda = true;
                        break;
                    }
                    TokenKind::LParen => paren_depth += 1,
                    TokenKind::RParen => paren_depth -= 1,
                    TokenKind::LBrace => brace_depth += 1,
                    TokenKind::RBrace if brace_depth > 0 => brace_depth -= 1,
                    TokenKind::RBrace | TokenKind::Eof => break,
                    _ => {}
                }
                scan += 1;
            }
        }

        if is_lambda {
            // Parse typed parameters: `x: Int, y: Int`
            loop {
                self.skip_trivia();
                if self.peek_kind() == TokenKind::Arrow {
                    break;
                }
                params.push(self.parse_param());
                self.skip_trivia();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Arrow, "->");
            self.skip_trivia();
        } else {
            // No `->` found — this is a lambda with implicit `it` parameter.
            // `{ it + 1 }` is shorthand for `{ it: Any -> it + 1 }`.
            // Scan for references to `it` in the body to decide if we need it.
            self.pos = saved_pos;
            let mut uses_it = false;
            {
                let mut scan = self.pos;
                while scan < self.tokens.len() {
                    match self.tokens[scan].kind {
                        TokenKind::RBrace | TokenKind::Eof => break,
                        TokenKind::Ident => {
                            if let Some(skotch_lexer::TokenPayload::Ident(ref s)) =
                                self.payloads.get(scan).and_then(|p| p.as_ref())
                            {
                                if s == "it" {
                                    uses_it = true;
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                    scan += 1;
                }
            }
            if uses_it {
                // Implicit `it` parameter. Default to `Any` — the
                // mir-lower lambda body emission consumes
                // `module.lambda_param_type` (set at the call site for
                // collection methods like `filter`, `map`, etc.) and
                // overrides this with the receiver's element type.
                //
                // A previous version of this code did a token-peek
                // "inference": if `it` was followed by `.`, it picked
                // `String`. That worked for `{ it.length }` on strings
                // but broke `list.filter { it.author }` where `it` is
                // actually `Message` (or any class) — the syntactic
                // guess shadowed the real type and the lambda body
                // got dropped during lowering.
                let it_sym = self.interner.intern("it");
                let it_type_name = "Any";
                let type_sym = self.interner.intern(it_type_name);
                params.push(Param {
                    name: it_sym,
                    ty: TypeRef {
                        name: type_sym,
                        nullable: false,
                        func_params: None,
                        type_args: Vec::new(),
                        is_suspend: false,
                        is_composable: false,
                        has_receiver: false,
                        span: start,
                    },
                    default: None,
                    is_vararg: false,
                    span: start,
                });
            }
        }

        // Parse body statements.
        let mut stmts = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                break;
            }
            stmts.push(self.parse_stmt());
        }

        // The last expression-statement becomes the implicit return value.
        // Convert it from Stmt::Expr to Stmt::Return.
        if let Some(last) = stmts.last_mut() {
            if let Stmt::Expr(e) = last {
                let span = e.span();
                let expr = std::mem::replace(e, Expr::IntLit(0, span));
                *last = Stmt::Return {
                    value: Some(expr),
                    label: None,
                    span,
                };
            }
        }

        let end = self.expect(TokenKind::RBrace, "}");
        let body = Block {
            stmts,
            span: start.merge(end),
        };

        Expr::Lambda {
            params,
            body,
            is_suspend: false,
            span: start.merge(end),
        }
    }

    fn parse_throw_expr(&mut self) -> Expr {
        let kw = self.peek_span();
        self.bump(); // 'throw'
        let expr = self.parse_expr();
        let span = kw.merge(expr.span());
        Expr::Throw {
            expr: Box::new(expr),
            span,
        }
    }

    fn parse_try_expr(&mut self) -> Expr {
        let kw = self.peek_span();
        self.bump(); // 'try'
        self.skip_trivia();
        let body = self.parse_block();
        self.skip_trivia();

        let mut catch_param = None;
        let mut catch_type = None;
        let mut catch_body = None;
        let mut extra_catches: Vec<(Symbol, Symbol, skotch_syntax::Block)> = Vec::new();
        let mut finally_body = None;

        if self.eat(TokenKind::KwCatch) {
            self.expect(TokenKind::LParen, "(");
            let idx = self.pos;
            self.expect(TokenKind::Ident, "exception parameter name");
            catch_param = Some(self.intern_ident_at(idx));
            self.expect(TokenKind::Colon, ":");
            self.skip_trivia();
            let type_idx = self.pos;
            self.expect(TokenKind::Ident, "exception type");
            catch_type = Some(self.intern_ident_at(type_idx));
            self.expect(TokenKind::RParen, ")");
            self.skip_trivia();
            catch_body = Some(Box::new(self.parse_block()));
            self.skip_trivia();

            // Support sequential catch clauses by collecting additional
            // clauses into `extra_catches`. MIR-lower emits one exception
            // handler per catch in source order — the JVM tries them in
            // order so the first matching type wins (matches Kotlin's
            // multi-catch semantics).
            while self.peek_kind() == TokenKind::KwCatch {
                self.bump(); // consume 'catch'
                self.expect(TokenKind::LParen, "(");
                let idx2 = self.pos;
                self.expect(TokenKind::Ident, "exception parameter name");
                let param2 = self.intern_ident_at(idx2);
                self.expect(TokenKind::Colon, ":");
                self.skip_trivia();
                let type_idx2 = self.pos;
                self.expect(TokenKind::Ident, "exception type");
                let type2 = self.intern_ident_at(type_idx2);
                self.expect(TokenKind::RParen, ")");
                self.skip_trivia();
                let body2 = self.parse_block();
                self.skip_trivia();
                extra_catches.push((param2, type2, body2));
            }
        }

        if self.eat(TokenKind::KwFinally) {
            self.skip_trivia();
            finally_body = Some(Box::new(self.parse_block()));
        }

        let end = if let Some(fb) = &finally_body {
            fb.span
        } else if let Some((_, _, last_body)) = extra_catches.last() {
            last_body.span
        } else if let Some(cb) = &catch_body {
            cb.span
        } else {
            body.span
        };

        Expr::Try {
            body: Box::new(body),
            catch_param,
            catch_type,
            catch_body,
            extra_catches,
            finally_body,
            span: kw.merge(end),
        }
    }

    fn parse_string_literal(&mut self) -> Expr {
        let start_span = self.peek_span();
        self.bump(); // StringStart
        let mut parts: Vec<TemplatePart> = Vec::new();
        let mut end_span = start_span;
        loop {
            match self.peek_kind() {
                TokenKind::StringChunk => {
                    let span = self.peek_span();
                    let text = match self.payload(self.pos) {
                        Some(TokenPayload::StringChunk(s)) => s.clone(),
                        _ => String::new(),
                    };
                    self.bump();
                    parts.push(TemplatePart::Text(text, span));
                    end_span = span;
                }
                TokenKind::StringIdentRef => {
                    let span = self.peek_span();
                    let s = match self.payload(self.pos) {
                        Some(TokenPayload::StringIdentRef(s)) => s.clone(),
                        _ => String::new(),
                    };
                    let name = self.interner.intern(&s);
                    self.bump();
                    parts.push(TemplatePart::IdentRef(name, span));
                    end_span = span;
                }
                TokenKind::StringExprStart => {
                    self.bump();
                    let inner = self.parse_expr();
                    let end = self.expect(TokenKind::StringExprEnd, "}");
                    end_span = end;
                    parts.push(TemplatePart::Expr(inner));
                }
                TokenKind::StringEnd => {
                    end_span = self.peek_span();
                    self.bump();
                    break;
                }
                TokenKind::Eof => {
                    self.diags
                        .push(Diagnostic::error(end_span, "unterminated string literal"));
                    break;
                }
                other => {
                    self.diags.push(Diagnostic::error(
                        self.peek_span(),
                        format!("unexpected {other:?} inside string literal"),
                    ));
                    self.bump();
                }
            }
        }
        let span = start_span.merge(end_span);

        // Collapse to a plain StringLit if there's no interpolation.
        if parts.iter().all(|p| matches!(p, TemplatePart::Text(_, _))) {
            let mut s = String::new();
            for p in &parts {
                if let TemplatePart::Text(t, _) = p {
                    s.push_str(t);
                }
            }
            Expr::StringLit(s, span)
        } else {
            Expr::StringTemplate(parts, span)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_lexer::lex;

    fn parse(src: &str) -> (KtFile, Diagnostics) {
        let mut diags = Diagnostics::new();
        let mut interner = Interner::new();
        let lf = lex(FileId(0), src, &mut diags);
        let file = parse_file(&lf, &mut interner, &mut diags);
        (file, diags)
    }

    #[test]
    fn parse_fun_main_empty() {
        let (file, d) = parse("fun main() {}");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(file.decls.len(), 1);
        let Decl::Fun(f) = &file.decls[0] else {
            panic!("expected fun decl");
        };
        assert_eq!(f.params.len(), 0);
        assert_eq!(f.body.stmts.len(), 0);
    }

    #[test]
    fn parse_fun_main_println_string() {
        let src = r#"fun main() { println("Hello, world!") }"#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "{:?}", d);
        let Decl::Fun(f) = &file.decls[0] else {
            panic!()
        };
        assert_eq!(f.body.stmts.len(), 1);
        let Stmt::Expr(Expr::Call { callee, args, .. }) = &f.body.stmts[0] else {
            panic!("expected call expression statement");
        };
        let Expr::Ident(_, _) = **callee else {
            panic!("expected ident callee")
        };
        assert_eq!(args.len(), 1);
        let Expr::StringLit(s, _) = &args[0].expr else {
            panic!("expected string arg")
        };
        assert_eq!(s, "Hello, world!");
    }

    #[test]
    fn parse_fun_main_println_int() {
        let (file, d) = parse("fun main() { println(42) }");
        assert!(d.is_empty(), "{:?}", d);
        let Decl::Fun(f) = &file.decls[0] else {
            panic!()
        };
        let Stmt::Expr(Expr::Call { args, .. }) = &f.body.stmts[0] else {
            panic!()
        };
        assert_eq!(args.len(), 1);
        assert!(matches!(args[0].expr, Expr::IntLit(42, _)));
    }

    #[test]
    fn parse_val_string() {
        let (file, d) = parse(r#"fun main() { val s = "hi"; println(s) }"#);
        assert!(d.is_empty(), "{:?}", d);
        let Decl::Fun(f) = &file.decls[0] else {
            panic!()
        };
        assert_eq!(f.body.stmts.len(), 2);
        let Stmt::Val(v) = &f.body.stmts[0] else {
            panic!("expected val")
        };
        assert!(!v.is_var);
        assert!(matches!(v.init, Expr::StringLit(_, _)));
    }

    #[test]
    fn parse_string_template() {
        let (_, d) = parse(r#"fun main() { val n = "world"; println("Hello, $n!") }"#);
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn parse_arithmetic() {
        let (file, d) = parse("fun main() { println(1 + 2 * 3) }");
        assert!(d.is_empty(), "{:?}", d);
        let Decl::Fun(f) = &file.decls[0] else {
            panic!()
        };
        let Stmt::Expr(Expr::Call { args, .. }) = &f.body.stmts[0] else {
            panic!()
        };
        // Should parse as 1 + (2 * 3) — check the outer is `+`.
        let Expr::Binary { op: BinOp::Add, .. } = args[0].expr else {
            panic!("expected outer Add");
        };
    }

    #[test]
    fn parse_if_expression() {
        let (_, d) = parse("fun main() { val x = if (true) 1 else 2; println(x) }");
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn parse_function_call() {
        let src = r#"
            fun greet(n: String) { println(n) }
            fun main() { greet("Kotlin") }
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(file.decls.len(), 2);
    }

    #[test]
    fn parse_top_level_val() {
        let (file, d) = parse(r#"val GREETING = "hi"; fun main() { println(GREETING) }"#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(file.decls.len(), 2);
        assert!(matches!(file.decls[0], Decl::Val(_)));
        assert!(matches!(file.decls[1], Decl::Fun(_)));
    }

    // ─── future test stubs ───────────────────────────────────────────────
    // TODO: parse_class                — `class Foo`
    // TODO: parse_data_class           — `data class Point(val x: Int, val y: Int)`
    // TODO: parse_sealed_class         — `sealed class Result`
    // TODO: parse_object               — `object Singleton`
    // TODO: parse_companion_object     — `class Foo { companion object { ... } }`
    // TODO: parse_extension_fn         — `fun String.shout()`
    // TODO: parse_generics             — `fun <T> id(x: T): T`
    // TODO: parse_when_expr            — `when (x) { 1 -> "a"; else -> "b" }`
    // TODO: parse_lambda               — `{ x: Int -> x + 1 }`
    // TODO: parse_imports              — `import kotlin.math.PI`
    // TODO: parse_visibility_modifiers — `private fun foo()`

    #[test]
    fn parse_annotation_on_fun() {
        let src = r#"
            @Suppress("unused")
            fun helper(): Int = 42

            fun main() {
                println(helper())
            }
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        assert_eq!(file.decls.len(), 2);
        assert!(matches!(file.decls[0], Decl::Fun(_)));
        assert!(matches!(file.decls[1], Decl::Fun(_)));
    }

    #[test]
    fn parse_annotation_no_args() {
        let src = r#"
            @JvmStatic
            fun foo(): Int = 1
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        assert_eq!(file.decls.len(), 1);
    }

    #[test]
    fn parse_annotation_on_companion_method() {
        // `@JvmStatic fun describe()` inside a `companion object { … }`
        // block: the annotation must be parsed and attached to the
        // FunDecl so that mir-lower's static-delegate synthesis can
        // detect it. Prior to the parser fix this was silently dropped.
        let src = r#"
            class Engine {
                companion object {
                    @JvmStatic
                    fun describe(): String = "engine"
                }
            }
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        assert_eq!(file.decls.len(), 1);
        if let Decl::Class(cd) = &file.decls[0] {
            assert_eq!(
                cd.companion_methods.len(),
                1,
                "expected one companion method"
            );
            let method = &cd.companion_methods[0];
            assert_eq!(
                method.annotations.len(),
                1,
                "@JvmStatic should reach the companion FunDecl"
            );
        } else {
            panic!("expected Decl::Class");
        }
    }

    #[test]
    fn parse_annotation_use_site_target() {
        let src = r#"
            class Cfg(@field:JvmField val x: Int)
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        assert_eq!(file.decls.len(), 1);
    }

    #[test]
    fn parse_annotation_complex_args() {
        let src = r#"
            @Target(AnnotationTarget.CLASS)
            @Deprecated("use X")
            fun old(): Int = 0
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        assert_eq!(file.decls.len(), 1);
    }

    #[test]
    fn parse_top_level_suspend_fun() {
        // `suspend` is accepted as a modifier on top-level functions.
        // The flag flows into FunDecl so that a future CPS transform
        // can locate suspend functions; right now they're lowered as
        // normal functions (see milestones.yaml v0.9.0).
        let src = r#"
            suspend fun compute(): Int = 42
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        assert_eq!(file.decls.len(), 1);
        let Decl::Fun(f) = &file.decls[0] else {
            panic!("expected fun decl");
        };
        assert!(f.is_suspend, "expected is_suspend = true");
    }

    #[test]
    fn parse_suspend_with_other_modifiers() {
        // `suspend` composes with other modifiers without order constraints
        // (private/override/etc.) — kotlinc accepts any order.
        let src = r#"
            private suspend fun helper(): Int = 1
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        let Decl::Fun(f) = &file.decls[0] else {
            panic!("expected fun decl");
        };
        assert!(f.is_suspend);
    }

    #[test]
    fn parse_class_member_suspend_fun() {
        let src = r#"
            class Api {
                suspend fun fetch(): String { return "x" }
            }
        "#;
        let (file, d) = parse(src);
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        let Decl::Class(c) = &file.decls[0] else {
            panic!("expected class decl");
        };
        assert_eq!(c.methods.len(), 1);
        assert!(c.methods[0].is_suspend);
    }

    #[test]
    fn parse_non_suspend_fun_has_false_flag() {
        // Regression: a plain function must not inherit the flag.
        let (file, d) = parse("fun plain(): Int = 1");
        assert!(d.is_empty(), "{:?}", d);
        let Decl::Fun(f) = &file.decls[0] else {
            panic!();
        };
        assert!(!f.is_suspend);
    }

    // ── Range-operator parsing regression tests ────────────────────────
    //
    // These pin down the parse_infix_call path so the lookahead heuristic
    // that used to skip `..` when the next-next token was `)` or `->`
    // can never come back. Each call site that broke in
    // parity/02-vars-and-control-flow is covered.

    fn parses_clean(src: &str) {
        let (_file, d) = parse(src);
        assert!(d.is_empty(), "{} produced diagnostics: {:?}", src, d);
    }

    #[test]
    fn parenthesised_range_simple() {
        // `(1..10)` was rejected as "expected ), found DotDot" because
        // parse_infix_call peeked two tokens ahead, saw `)`, and refused
        // to consume `..`.
        parses_clean("fun main() { val r = (1..10) }");
    }

    #[test]
    fn parenthesised_range_with_call_chain() {
        // The original failure site from parity 02:
        //   val first = (1..20).firstOrNull { it % 7 == 0 }
        parses_clean("fun main() { val x = (1..20).firstOrNull { it % 7 == 0 } }");
    }

    #[test]
    fn parenthesised_range_as_argument() {
        // `f((1..3))` — same shape but the `)` belongs to the outer call.
        parses_clean("fun main() { println((1..3).count()) }");
    }

    #[test]
    fn bare_range_when_pattern() {
        // `1..10 -> body` as a when pattern. parse_expr is called for the
        // pattern; the heuristic used to skip `..` because the token two
        // ahead was `->`, leaving the parser stuck on a leftover `..`.
        parses_clean(
            "fun main() { val n = 5; val s = when (n) { 1..10 -> \"small\"; else -> \"big\" } }",
        );
    }

    #[test]
    fn for_range_loop_still_works() {
        // Sanity: removing the heuristic must not regress the for-loop
        // parser, which uses parse_equality (not parse_expr) so it never
        // reaches parse_infix_call's `..` branch.
        parses_clean("fun main() { for (i in 1..5) println(i) }");
    }

    #[test]
    fn when_in_range_pattern_still_works() {
        // Sanity for the `in 1..10` when pattern path, which uses
        // parse_additive and handles `..` itself.
        parses_clean(
            "fun main() { val n = 5; val s = when (n) { in 1..10 -> \"ok\"; else -> \"no\" } }",
        );
    }
}
