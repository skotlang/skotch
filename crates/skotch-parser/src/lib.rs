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
                } else if self.peek_kind() == TokenKind::Eq {
                    // Named argument: `name = "value"` — skip the name, parse value.
                    self.bump(); // consume '='
                    self.skip_trivia();
                    self.parse_annotation_arg()
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
            _ => String::new(),
        };
        self.interner.intern(&s)
    }

    // ─── grammar ─────────────────────────────────────────────────────────

    fn parse_file(&mut self) -> KtFile {
        let mut package = None;
        let mut imports = Vec::new();
        let mut decls = Vec::new();

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
            let mut visibility = Visibility::Public;
            // Check for `annotation class` and `value class` soft keywords.
            if self.peek_kind() == TokenKind::Ident {
                let kw = self.lexeme_str(self.pos).to_string();
                if kw == "annotation" && self.peek_kind_at(1) == TokenKind::KwClass {
                    is_annotation_class = true;
                    self.bump();
                    self.skip_trivia();
                } else if kw == "value" && self.peek_kind_at(1) == TokenKind::KwClass {
                    is_value_class = true;
                    self.bump();
                    self.skip_trivia();
                }
            }
            while matches!(
                self.peek_kind(),
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
                match self.peek_kind() {
                    TokenKind::KwData => is_data = true,
                    TokenKind::KwEnum => is_enum = true,
                    TokenKind::KwOpen => is_open = true,
                    TokenKind::KwAbstract => is_abstract = true,
                    TokenKind::KwSealed => is_sealed = true,
                    TokenKind::KwSuspend => is_suspend = true,
                    TokenKind::KwInline => is_inline = true,
                    TokenKind::KwPrivate => visibility = Visibility::Private,
                    TokenKind::KwProtected => visibility = Visibility::Protected,
                    TokenKind::KwInternal => visibility = Visibility::Internal,
                    _ => {}
                }
                self.bump();
                self.skip_trivia();
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
            if self.peek_kind() != TokenKind::Ident {
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
            if self.peek_kind() != TokenKind::Ident {
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

        // Primary constructor parameters.
        let mut constructor_params = Vec::new();
        if self.peek_kind() == TokenKind::LParen {
            self.bump();
            self.skip_trivia();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    self.skip_annotations();
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
                let parent_name = self.intern_ident_at(parent_name_idx);
                self.skip_trivia();
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
                // Skip annotations before class members.
                self.skip_annotations();
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
                                        self.skip_annotations();
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
                                            companion_methods.push(self.parse_fun_decl());
                                        } else if matches!(
                                            self.peek_kind(),
                                            TokenKind::KwVal | TokenKind::KwVar
                                        ) {
                                            companion_properties.push(self.parse_property_decl());
                                        } else {
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

        // Parse parameter list.
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
            }
        }
        self.expect(TokenKind::RParen, ")");
        self.skip_trivia();

        // Parse optional delegation: `: this(args)`.
        let mut delegate_args = Vec::new();
        let mut has_delegation = false;
        if self.eat(TokenKind::Colon) {
            has_delegation = true;
            self.skip_trivia();
            // Expect `this`.
            if self.peek_kind() == TokenKind::Ident || self.peek_kind() == TokenKind::KwConstructor
            {
                // In Kotlin, `this` is a keyword-like identifier here.
                // The lexer emits it as Ident. Consume it.
                self.bump();
                self.skip_trivia();
            }
            // Parse delegate call args.
            self.expect(TokenKind::LParen, "(");
            self.skip_trivia();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    delegate_args.push(self.parse_expr());
                    self.skip_trivia();
                    if !self.eat(TokenKind::Comma) {
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
        let name = if self.peek_kind() == TokenKind::Ident {
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
        let delegate = if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "by"
        {
            self.bump(); // consume `by`
            self.skip_trivia();
            // Parse the delegate expression. For `lazy { ... }` this is
            // just the block. For `Cached { ... }` it's a constructor call
            // with trailing lambda. For any other expression, parse it.
            if self.peek_kind() == TokenKind::Ident && self.lexeme_str(self.pos) == "lazy" {
                self.bump(); // consume `lazy`
                self.skip_trivia();
                if self.peek_kind() == TokenKind::LBrace {
                    Some(Box::new(self.parse_block()))
                } else {
                    None
                }
            } else {
                // Generic delegate: parse as an expression.
                // `Cached { "Hello" }` → Call(Cached, [Lambda])
                let expr = self.parse_expr();
                // Wrap in a synthetic block with a single expression stmt.
                Some(Box::new(Block {
                    stmts: vec![Stmt::Expr(expr)],
                    span: self.peek_span(),
                }))
            }
        } else {
            None
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
            getter,
            setter,
            span: start.merge(name_span),
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

        // Check for extension function: `fun Type.name(...)`.
        // If the next ident is followed by `.`, it's a receiver type.
        let mut receiver_ty = None;
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            let first_ident = self.intern_ident_at(name_idx);
            if self.peek_kind() == TokenKind::Dot {
                // Extension function: first_ident is the receiver type.
                let recv_name = first_ident;
                let recv_span = name_span;
                receiver_ty = Some(TypeRef {
                    name: recv_name,
                    nullable: false,
                    func_params: None,
                    type_args: Vec::new(),
                    is_suspend: false,
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
        let name_idx = self.pos;
        let name_span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident {
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
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(idx)
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

        // Parse getter body: `get() = expr` or `get() { stmts }` or just `= expr`
        let body = if self.peek_kind() == TokenKind::Ident {
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
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(name_idx)
        } else {
            self.diags.push(Diagnostic::error(
                name_span,
                "expected name in val/var declaration",
            ));
            self.interner.intern("")
        };
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
                    // Generic delegate: parse as expression.
                    self.parse_expr()
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
                    finally_body,
                    span,
                } = try_expr
                {
                    Stmt::TryStmt {
                        body: *body,
                        catch_param,
                        catch_type,
                        catch_body: catch_body.map(|b| *b),
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
        let range_start = self.parse_expr();
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
            let range_end = self.parse_expr();
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
    /// Parses `expr IDENT expr` as `expr.IDENT(expr)` for known infix
    /// keywords. Does NOT loop — infix calls are right-to-left single.
    fn parse_infix_call(&mut self) -> Expr {
        let lhs = self.parse_equality();
        self.skip_trivia();
        // `..` range operator: `1..10` → rangeTo call.
        // Only consume `..` when NOT inside for-loop/when range context
        // (those parsers handle `..` themselves). We detect this by
        // checking that the token AFTER `..` and the range end is NOT
        // `)` (for-loop end) or `->` (when pattern end).
        if self.peek_kind() == TokenKind::DotDot {
            // Peek ahead: if after `.. expr` we see `)` or `->`, this
            // is a for-loop or when range — let the caller handle `..`.
            // Otherwise consume it as a rangeTo expression.
            //
            // Simple heuristic: if the TWO tokens ahead include `)` or
            // `->`, skip. This isn't perfect but handles the common cases.
            let after_dotdot = self.peek_kind_at(2); // token after `.. <number>`
            if after_dotdot != TokenKind::RParen && after_dotdot != TokenKind::Arrow {
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
        }
        // `in` operator: `5 in r` → `r.contains(5)`.
        // `!in` operator: `5 !in r` → `!r.contains(5)`.
        if self.peek_kind() == TokenKind::KwIn {
            let kw_span = self.peek_span();
            self.bump(); // consume `in`
            self.skip_trivia();
            let rhs = self.parse_equality();
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
            let rhs = self.parse_equality();
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

        // Check if the next token is a known infix function name that is
        // NOT followed by `(` (which would be a regular call, already
        // handled by parse_postfix).
        if self.peek_kind() == TokenKind::Ident {
            let idx = self.pos;
            let text = match self.payload(idx) {
                Some(TokenPayload::Ident(s)) => s.clone(),
                _ => String::new(),
            };
            let is_infix = matches!(
                text.as_str(),
                "to" | "and" | "or" | "xor" | "shl" | "shr" | "ushr" | "contains" | "zip"
            );
            if is_infix && self.peek_kind_at(1) != TokenKind::LParen {
                let kw_span = self.peek_span();
                self.bump(); // consume infix ident
                let name = self.interner.intern(&text);
                self.skip_trivia();
                let rhs = self.parse_equality();
                let span = lhs.span().merge(rhs.span());
                return Expr::Call {
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
            // `as` / `as?` type cast
            if self.peek_kind() == TokenKind::KwAs {
                self.bump();
                let safe = self.eat(TokenKind::Question);
                self.skip_trivia();
                let idx = self.pos;
                let type_span = self.peek_span();
                self.expect(TokenKind::Ident, "type name");
                let type_name = self.intern_ident_at(idx);
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
                    if self.peek_kind() != TokenKind::Ident {
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
                                    let value = self.parse_expr();
                                    args.push(CallArg {
                                        name: None,
                                        expr: value,
                                    });
                                    self.skip_trivia();
                                    if !self.eat(TokenKind::Comma) {
                                        break;
                                    }
                                }
                            }
                            let rp = self.expect(TokenKind::RParen, ")");
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
                            // Peek: Ident followed by Eq (not EqEq)
                            let named = self.peek_kind() == TokenKind::Ident
                                && self.peek_kind_at(1) == TokenKind::Eq;
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
                // Array/collection indexing: `expr[index]`.
                TokenKind::LBracket => {
                    self.bump(); // consume `[`
                    self.skip_trivia();
                    let index = self.parse_expr();
                    self.skip_trivia();
                    let rb = self.expect(TokenKind::RBracket, "']'");
                    let span = expr.span().merge(rb);
                    expr = Expr::Index {
                        receiver: Box::new(expr),
                        index: Box::new(index),
                        span,
                    };
                }
                // Also handle: `f { body }` — call with ONLY a trailing lambda,
                // no parentheses at all. Only if current expr is an identifier.
                TokenKind::LBrace if matches!(expr, Expr::Ident(_, _) | Expr::Field { .. }) => {
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
            let expr = self.parse_expr();
            let span = expr.span();
            Block {
                stmts: vec![Stmt::Expr(expr)],
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
        let super_type = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(type_idx)
        } else {
            self.diags
                .push(Diagnostic::error(type_span, "expected type name"));
            self.interner.intern("")
        };
        self.skip_trivia();
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
            let mut scan = self.pos;
            let mut depth = 0;
            while scan < self.tokens.len() {
                match self.tokens[scan].kind {
                    TokenKind::Arrow if depth == 0 => {
                        is_lambda = true;
                        break;
                    }
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => depth -= 1,
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
                // Infer `it` type from usage in the body:
                // - If `it` appears next to `.` (method call) → String
                // - Otherwise → Int (most common case)
                // TODO: proper inference from val annotation / function param type.
                let it_sym = self.interner.intern("it");
                let mut it_type_name = "Any";
                {
                    let mut scan = self.pos;
                    while scan < self.tokens.len() {
                        match self.tokens[scan].kind {
                            TokenKind::RBrace | TokenKind::Eof => break,
                            TokenKind::Ident => {
                                if let Some(skotch_lexer::TokenPayload::Ident(ref s)) =
                                    self.payloads.get(scan).and_then(|p| p.as_ref())
                                {
                                    if s == "it"
                                        && scan + 1 < self.tokens.len()
                                        && self.tokens[scan + 1].kind == TokenKind::Dot
                                    {
                                        it_type_name = "String";
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }
                        scan += 1;
                    }
                }
                let type_sym = self.interner.intern(it_type_name);
                params.push(Param {
                    name: it_sym,
                    ty: TypeRef {
                        name: type_sym,
                        nullable: false,
                        func_params: None,
                        type_args: Vec::new(),
                        is_suspend: false,
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

            // Support sequential catch clauses:
            // try { } catch (e: A) { } catch (e: B) { }
            // Nest as: try { } catch (e: A) { } → wrapped, with the
            // second catch applied to the whole try-catch as a new try-catch.
            // This is handled by the statement-level TryStmt which already
            // supports multiple catch blocks. For the expression-level Try,
            // we parse additional catches into the same block by wrapping
            // the catch body in another try-catch.
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
                // Nest: wrap the original try { body } catch(A) { catch_body }
                // as a new try-catch around the whole thing.
                // Actually, Kotlin semantics: multiple catches are tried in
                // order. We update to the new catch, keeping the original
                // body. This is a simplification — real multi-catch requires
                // multiple exception table entries. For now, replace the
                // catch (only the LAST catch is effective).
                catch_param = Some(param2);
                catch_type = Some(type2);
                catch_body = Some(Box::new(body2));
            }
        }

        if self.eat(TokenKind::KwFinally) {
            self.skip_trivia();
            finally_body = Some(Box::new(self.parse_block()));
        }

        let end = if let Some(fb) = &finally_body {
            fb.span
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
}
