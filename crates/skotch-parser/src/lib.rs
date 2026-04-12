//! Hand-rolled recursive-descent parser for the Kotlin 2 subset skotch
//! currently accepts.
//!
//! ## Why hand-rolled (not chumsky)?
//!
//! The original architectural plan named `chumsky` 0.10 as the parser
//! library. We deferred that swap to a later PR for two reasons:
//!
//! 1. Chumsky 0.10 has a non-trivial learning curve and a fluid API; for
//!    the ~10 fixture-driven productions PR #1 needs, a hand-rolled
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
//! Kotlin's grammar is newline-sensitive, but the PR #1 fixtures all
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
    BinOp, Block, CallArg, ClassDecl, ConstructorParam, Decl, Expr, FunDecl, ImportDecl, KtFile,
    PackageDecl, Param, PropertyDecl, Stmt, TemplatePart, Token, TokenKind, TypeRef, UnaryOp,
    ValDecl, WhenBranch,
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

    /// Consume one token regardless of kind.
    fn bump(&mut self) -> Token {
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
            // Skip modifier keywords that we recognize but don't enforce.
            let mut is_data = false;
            while matches!(
                self.peek_kind(),
                TokenKind::KwConst
                    | TokenKind::KwOpen
                    | TokenKind::KwAbstract
                    | TokenKind::KwPrivate
                    | TokenKind::KwProtected
                    | TokenKind::KwInternal
                    | TokenKind::KwOverride
                    | TokenKind::KwData
            ) {
                if self.peek_kind() == TokenKind::KwData {
                    is_data = true;
                }
                self.bump();
                self.skip_trivia();
            }
            match self.peek_kind() {
                TokenKind::KwFun => {
                    let f = self.parse_fun_decl();
                    decls.push(Decl::Fun(f));
                }
                TokenKind::KwVal | TokenKind::KwVar => {
                    let v = self.parse_val_decl();
                    decls.push(Decl::Val(v));
                }
                TokenKind::KwClass => {
                    let mut cd = self.parse_class_decl();
                    cd.is_data = is_data;
                    decls.push(Decl::Class(cd));
                }
                TokenKind::KwObject => {
                    let span = self.peek_span();
                    self.diags.push(Diagnostic::error(
                        span,
                        "object declarations are not yet supported",
                    ));
                    decls.push(Decl::Unsupported {
                        what: "object",
                        span,
                    });
                    self.recover_to_top_level();
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
        ImportDecl {
            path,
            is_wildcard,
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

        // Primary constructor parameters.
        let mut constructor_params = Vec::new();
        if self.peek_kind() == TokenKind::LParen {
            self.bump();
            self.skip_trivia();
            if self.peek_kind() != TokenKind::RParen {
                loop {
                    self.skip_trivia();
                    let param_start = self.peek_span();
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

        // Class body.
        let mut properties = Vec::new();
        let mut methods = Vec::new();
        let mut init_blocks = Vec::new();
        if self.peek_kind() == TokenKind::LBrace {
            self.bump();
            loop {
                self.skip_trivia();
                if self.peek_kind() == TokenKind::RBrace || self.peek_kind() == TokenKind::Eof {
                    break;
                }
                // Skip modifier keywords before members.
                while matches!(
                    self.peek_kind(),
                    TokenKind::KwOverride
                        | TokenKind::KwOpen
                        | TokenKind::KwAbstract
                        | TokenKind::KwPrivate
                        | TokenKind::KwProtected
                        | TokenKind::KwInternal
                ) {
                    self.bump();
                    self.skip_trivia();
                }
                match self.peek_kind() {
                    TokenKind::KwFun => {
                        methods.push(self.parse_fun_decl());
                    }
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let prop = self.parse_property_decl();
                        properties.push(prop);
                    }
                    TokenKind::KwInit => {
                        self.bump(); // consume 'init'
                        self.skip_trivia();
                        init_blocks.push(self.parse_block());
                    }
                    _ => {
                        self.bump(); // skip unknown token
                    }
                }
            }
            self.expect(TokenKind::RBrace, "}");
        }

        ClassDecl {
            is_data: false, // set by caller if `data` modifier present
            name,
            name_span,
            constructor_params,
            properties,
            methods,
            init_blocks,
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
        self.skip_trivia();
        let init = if self.eat(TokenKind::Eq) {
            self.skip_trivia();
            Some(self.parse_expr())
        } else {
            None
        };
        PropertyDecl {
            is_var,
            name,
            name_span,
            ty,
            init,
            span: start.merge(name_span),
        }
    }

    fn parse_fun_decl(&mut self) -> FunDecl {
        let kw = self.expect(TokenKind::KwFun, "fun");
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
        // Support both block body `{ ... }` and expression body `= expr`.
        let body = if self.peek_kind() == TokenKind::Eq {
            self.bump(); // consume `=`
            self.skip_trivia();
            let expr = self.parse_expr();
            let span = expr.span();
            Block {
                stmts: vec![Stmt::Return {
                    value: Some(expr),
                    span,
                }],
                span,
            }
        } else {
            self.parse_block()
        };
        FunDecl {
            name,
            name_span,
            params,
            return_ty,
            receiver_ty,
            span: kw.merge(rparen).merge(body.span),
            body,
        }
    }

    fn parse_param(&mut self) -> Param {
        self.skip_trivia();
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
        self.expect(TokenKind::Colon, ":");
        let ty = self.parse_type_ref();
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
            span: name_span.merge(end),
            ty,
        }
    }

    fn parse_type_ref(&mut self) -> TypeRef {
        self.skip_trivia();
        let idx = self.pos;
        let span = self.peek_span();
        let name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(idx)
        } else {
            self.diags
                .push(Diagnostic::error(span, "expected type name"));
            self.interner.intern("")
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
            span: span.merge(end),
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
        self.expect(TokenKind::Eq, "=");
        let init = self.parse_expr();
        ValDecl {
            is_var,
            name,
            name_span,
            ty,
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

    fn parse_stmt(&mut self) -> Stmt {
        self.skip_trivia();
        match self.peek_kind() {
            TokenKind::KwVal | TokenKind::KwVar => Stmt::Val(self.parse_val_decl()),
            TokenKind::KwReturn => {
                let kw = self.peek_span();
                self.bump();
                self.skip_trivia();
                // A naked `return` ends at a `}` or `Newline` (already skipped).
                let value = if matches!(self.peek_kind(), TokenKind::RBrace | TokenKind::Eof) {
                    None
                } else {
                    Some(self.parse_expr())
                };
                let span = match &value {
                    Some(v) => kw.merge(v.span()),
                    None => kw,
                };
                Stmt::Return { value, span }
            }
            TokenKind::KwFun => {
                // Local function declaration inside a block.
                let fun_decl = self.parse_fun_decl();
                Stmt::LocalFun(fun_decl)
            }
            TokenKind::KwBreak => {
                let span = self.peek_span();
                self.bump();
                Stmt::Break(span)
            }
            TokenKind::KwContinue => {
                let span = self.peek_span();
                self.bump();
                Stmt::Continue(span)
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
        // Parse: varName in start..end
        let var_name_idx = self.pos;
        let var_span = self.peek_span();
        let var_name = if self.peek_kind() == TokenKind::Ident {
            self.bump();
            self.intern_ident_at(var_name_idx)
        } else {
            self.diags
                .push(Diagnostic::error(var_span, "expected loop variable name"));
            self.interner.intern("_")
        };
        self.skip_trivia();
        self.expect(TokenKind::KwIn, "'in' after loop variable");
        self.skip_trivia();
        let range_start = self.parse_expr();
        self.skip_trivia();
        self.expect(TokenKind::DotDot, "'..' for range");
        self.skip_trivia();
        let range_end = self.parse_expr();
        self.skip_trivia();
        self.expect(TokenKind::RParen, "')' after for range");
        self.skip_trivia();
        let body = self.parse_block();
        let span = start.merge(body.span);
        Stmt::For {
            var_name,
            start: range_start,
            end: range_end,
            body,
            span,
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
        let mut lhs = self.parse_equality();
        loop {
            self.skip_trivia();
            if self.peek_kind() != TokenKind::AmpAmp {
                break;
            }
            self.bump();
            let rhs = self.parse_equality();
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
            // Postfix `.field` and `(args)` cannot have a newline before
            // them — that would be a new statement. We don't skip
            // newlines here.
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
                    let span = expr.span().merge(rp);
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
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
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr();
                let rp = self.expect(TokenKind::RParen, ")");
                Expr::Paren(Box::new(inner), span.merge(rp))
            }
            TokenKind::KwIf => self.parse_if_expr(),
            TokenKind::KwWhen => self.parse_when_expr(),
            TokenKind::KwThrow => self.parse_throw_expr(),
            TokenKind::StringStart => self.parse_string_literal(),
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
            // Patterns can be: expr, in expr..expr (range test)
            let start = self.peek_span();
            let mut patterns: Vec<(Expr, Option<Expr>)> = Vec::new();
            loop {
                // Check for `in expr..expr` range pattern
                if self.peek_kind() == TokenKind::KwIn {
                    self.bump(); // consume `in`
                    self.skip_trivia();
                    let range_start = self.parse_expr();
                    self.skip_trivia();
                    self.expect(TokenKind::DotDot, "'..' in range pattern");
                    self.skip_trivia();
                    let range_end = self.parse_expr();
                    patterns.push((range_start, Some(range_end)));
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

    /// Parse a when-branch body — currently single expressions only.
    fn parse_when_body(&mut self) -> Expr {
        self.parse_expr()
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
        } else {
            let expr = self.parse_expr();
            let span = expr.span();
            Block {
                stmts: vec![Stmt::Expr(expr)],
                span,
            }
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
        let Expr::StringLit(s, _) = &args[0] else {
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
        assert!(matches!(args[0], Expr::IntLit(42, _)));
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
        let Expr::Binary { op: BinOp::Add, .. } = args[0] else {
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
    // TODO: parse_annotation           — `@Test fun foo() {}`
    // TODO: parse_visibility_modifiers — `private fun foo()`
}
