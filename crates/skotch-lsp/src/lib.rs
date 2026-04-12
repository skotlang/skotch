//! Language Server Protocol implementation for skotch.
//!
//! Provides real-time diagnostics, semantic tokens, hover information,
//! go-to-definition, and completions for Kotlin files edited in any
//! LSP-compatible editor.
//!
//! # Architecture
//!
//! The server holds a `DashMap<Url, DocumentState>` keyed by file URI.
//! On every `didOpen` / `didChange`, the full front-end pipeline runs
//! (lex → parse → resolve → typecheck) and diagnostics are published.
//! Semantic tokens, hover, go-to-definition, and completions read from
//! the cached analysis state.

use dashmap::DashMap;
use std::path::PathBuf;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use skotch_diagnostics::Diagnostics;
use skotch_intern::{Interner, Symbol};
use skotch_lexer::lex;
use skotch_parser::parse_file;
use skotch_resolve::{resolve_file, DefId, ResolvedFile};
use skotch_span::{FileId, SourceMap, Span};
use skotch_syntax::ast::KtFile;
use skotch_syntax::token::{Token, TokenKind};
use skotch_typeck::{type_check, TypedFile};

// ─── Analysis state ─────────────────────────────────────────────────────────

/// Cached analysis for a single open document.
#[allow(dead_code)]
struct DocumentState {
    source: String,
    version: i32,
    tokens: Vec<Token>,
    ast: KtFile,
    resolved: ResolvedFile,
    typed: TypedFile,
    interner: Interner,
    source_map: SourceMap,
    diags: Diagnostics,
}

// ─── Server ─────────────────────────────────────────────────────────────────

pub struct SkotchLanguageServer {
    client: Client,
    documents: DashMap<Url, DocumentState>,
}

impl SkotchLanguageServer {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
        }
    }

    /// Run the full front-end pipeline and cache the results.
    fn analyze(&self, uri: &Url, source: String, version: i32) -> Vec<Diagnostic> {
        let path = uri_to_path(uri);
        let mut sm = SourceMap::new();
        let file_id = sm.add(path, source.clone());

        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();

        let lexed = lex(file_id, &source, &mut diags);
        let tokens = lexed.tokens.clone();
        let ast = parse_file(&lexed, &mut interner, &mut diags);
        let resolved = resolve_file(&ast, &mut interner, &mut diags);
        let typed = type_check(&ast, &resolved, &mut interner, &mut diags);

        let lsp_diags = to_lsp_diagnostics(&diags, &sm);

        self.documents.insert(
            uri.clone(),
            DocumentState {
                source,
                version,
                tokens,
                ast,
                resolved,
                typed,
                interner,
                source_map: sm,
                diags,
            },
        );

        lsp_diags
    }
}

fn uri_to_path(uri: &Url) -> PathBuf {
    uri.to_file_path()
        .unwrap_or_else(|_| PathBuf::from(uri.path()))
}

fn to_lsp_diagnostics(diags: &Diagnostics, sm: &SourceMap) -> Vec<Diagnostic> {
    diags
        .iter()
        .map(|d| {
            let file = sm.get(d.primary.span.file);
            let (start_line, start_col) = file.line_col(d.primary.span.start);
            let (end_line, end_col) = file.line_col(d.primary.span.end);
            Diagnostic {
                range: Range {
                    start: Position::new(start_line.saturating_sub(1), start_col.saturating_sub(1)),
                    end: Position::new(end_line.saturating_sub(1), end_col.saturating_sub(1)),
                },
                severity: Some(match d.severity {
                    skotch_diagnostics::Severity::Error => DiagnosticSeverity::ERROR,
                    skotch_diagnostics::Severity::Warning => DiagnosticSeverity::WARNING,
                    skotch_diagnostics::Severity::Note => DiagnosticSeverity::INFORMATION,
                }),
                source: Some("skotch".into()),
                message: d.message.clone(),
                related_information: if d.secondary.is_empty() {
                    None
                } else {
                    Some(
                        d.secondary
                            .iter()
                            .map(|s| {
                                let f = sm.get(s.span.file);
                                let (sl, sc) = f.line_col(s.span.start);
                                let (el, ec) = f.line_col(s.span.end);
                                DiagnosticRelatedInformation {
                                    location: Location {
                                        uri: Url::from_file_path(&f.path).unwrap_or_else(|_| {
                                            Url::parse("file:///unknown").unwrap()
                                        }),
                                        range: Range {
                                            start: Position::new(
                                                sl.saturating_sub(1),
                                                sc.saturating_sub(1),
                                            ),
                                            end: Position::new(
                                                el.saturating_sub(1),
                                                ec.saturating_sub(1),
                                            ),
                                        },
                                    },
                                    message: s.message.clone(),
                                }
                            })
                            .collect(),
                    )
                },
                ..Default::default()
            }
        })
        .collect()
}

// ─── Semantic tokens ────────────────────────────────────────────────────────

const SEMANTIC_TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,   // 0
    SemanticTokenType::STRING,    // 1
    SemanticTokenType::NUMBER,    // 2
    SemanticTokenType::OPERATOR,  // 3
    SemanticTokenType::FUNCTION,  // 4
    SemanticTokenType::VARIABLE,  // 5
    SemanticTokenType::PARAMETER, // 6
    SemanticTokenType::TYPE,      // 7
    SemanticTokenType::NAMESPACE, // 8
];

fn token_type_index(kind: TokenKind) -> Option<u32> {
    match kind {
        // Keywords
        TokenKind::KwFun
        | TokenKind::KwVal
        | TokenKind::KwVar
        | TokenKind::KwIf
        | TokenKind::KwElse
        | TokenKind::KwReturn
        | TokenKind::KwTrue
        | TokenKind::KwFalse
        | TokenKind::KwNull
        | TokenKind::KwWhile
        | TokenKind::KwDo
        | TokenKind::KwWhen
        | TokenKind::KwFor
        | TokenKind::KwIn
        | TokenKind::KwBreak
        | TokenKind::KwContinue
        | TokenKind::KwClass
        | TokenKind::KwObject
        | TokenKind::KwPackage
        | TokenKind::KwImport
        | TokenKind::KwConst
        | TokenKind::KwThrow
        | TokenKind::KwTry
        | TokenKind::KwCatch
        | TokenKind::KwFinally
        | TokenKind::KwIs
        | TokenKind::KwAs
        | TokenKind::KwInit
        | TokenKind::KwData
        | TokenKind::KwEnum
        | TokenKind::KwOverride
        | TokenKind::KwOpen
        | TokenKind::KwAbstract
        | TokenKind::KwPrivate
        | TokenKind::KwProtected
        | TokenKind::KwInternal => Some(0),

        // Strings
        TokenKind::StringLit
        | TokenKind::StringStart
        | TokenKind::StringChunk
        | TokenKind::StringEnd => Some(1),

        // Numbers
        TokenKind::IntLit | TokenKind::LongLit | TokenKind::DoubleLit => Some(2),

        // Operators
        TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent
        | TokenKind::EqEq
        | TokenKind::NotEq
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::LtEq
        | TokenKind::GtEq
        | TokenKind::AmpAmp
        | TokenKind::PipePipe
        | TokenKind::DotDot
        | TokenKind::PlusEq
        | TokenKind::MinusEq
        | TokenKind::StarEq
        | TokenKind::SlashEq
        | TokenKind::PercentEq
        | TokenKind::Arrow
        | TokenKind::Bang
        | TokenKind::QuestionDot
        | TokenKind::Elvis
        | TokenKind::BangBang => Some(3),

        _ => None,
    }
}

fn build_semantic_tokens(
    tokens: &[Token],
    resolved: &ResolvedFile,
    interner: &Interner,
    sm: &SourceMap,
    file_id: FileId,
) -> Vec<SemanticToken> {
    let file = sm.get(file_id);
    let mut result = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    // Build a set of ref spans → DefId for enriching identifiers.
    let mut ref_map = std::collections::HashMap::new();
    for rf in &resolved.functions {
        for r in &rf.body_refs {
            ref_map.insert((r.span.start, r.span.end), r.def);
        }
    }
    for tv in &resolved.top_vals {
        for r in &tv.init_refs {
            ref_map.insert((r.span.start, r.span.end), r.def);
        }
    }

    // Collect function name symbols for identification.
    let fn_names: Vec<Symbol> = resolved.functions.iter().map(|f| f.name).collect();

    for tok in tokens {
        if tok.kind == TokenKind::Newline || tok.kind == TokenKind::Eof {
            continue;
        }

        let (line, col) = file.line_col(tok.span.start);
        let line = line.saturating_sub(1);
        let col = col.saturating_sub(1);
        let length = tok.span.end - tok.span.start;

        let type_idx = if tok.kind == TokenKind::Ident {
            // Check if this identifier has a resolved reference.
            if let Some(def) = ref_map.get(&(tok.span.start, tok.span.end)) {
                match def {
                    DefId::Function(_) => {
                        // Check if it's being used as a type or function.
                        let name_str = &file.text[tok.span.start as usize..tok.span.end as usize];
                        if name_str.starts_with(|c: char| c.is_uppercase()) {
                            Some(7) // type
                        } else {
                            Some(4) // function
                        }
                    }
                    DefId::PrintlnIntrinsic => Some(4), // function
                    DefId::Param(_, _) => Some(6),      // parameter
                    DefId::Local(_, _) => Some(5),      // variable
                    DefId::TopLevelVal(_) => Some(5),   // variable
                    DefId::PossibleExternal(_) => Some(7), // type/class
                    DefId::Error => None,
                }
            } else {
                // Unreferenced ident — check if it's a function declaration name.
                let name_str = &file.text[tok.span.start as usize..tok.span.end as usize];
                if fn_names.iter().any(|&s| interner.resolve(s) == name_str) {
                    if name_str.starts_with(|c: char| c.is_uppercase()) {
                        Some(7) // type (class name)
                    } else {
                        Some(4) // function declaration
                    }
                } else if name_str.starts_with(|c: char| c.is_uppercase()) {
                    Some(7) // type reference
                } else {
                    Some(5) // variable
                }
            }
        } else if tok.kind == TokenKind::StringIdentRef {
            Some(5) // variable in string template
        } else {
            token_type_index(tok.kind)
        };

        if let Some(type_idx) = type_idx {
            let delta_line = line - prev_line;
            let delta_start = if delta_line == 0 {
                col - prev_start
            } else {
                col
            };

            result.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type: type_idx,
                token_modifiers_bitset: 0,
            });

            prev_line = line;
            prev_start = col;
        }
    }

    result
}

// ─── Hover ──────────────────────────────────────────────────────────────────

fn find_token_at_position(
    tokens: &[Token],
    sm: &SourceMap,
    file_id: FileId,
    pos: Position,
) -> Option<Token> {
    let file = sm.get(file_id);
    // Convert LSP position (0-based) to byte offset.
    let target_line = pos.line + 1;
    let target_col = pos.character + 1;

    let byte_offset = position_to_byte_offset(&file.text, target_line, target_col)?;

    tokens
        .iter()
        .find(|t| t.span.file == file_id && byte_offset >= t.span.start && byte_offset < t.span.end)
        .copied()
}

fn position_to_byte_offset(text: &str, target_line: u32, target_col: u32) -> Option<u32> {
    let mut line = 1u32;
    let mut col = 1u32;
    for (i, ch) in text.char_indices() {
        if line == target_line && col == target_col {
            return Some(i as u32);
        }
        if ch == '\n' {
            if line == target_line {
                return Some(i as u32);
            }
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    if line == target_line && col == target_col {
        Some(text.len() as u32)
    } else {
        None
    }
}

fn find_ref_at_span(resolved: &ResolvedFile, span: Span) -> Option<DefId> {
    for rf in &resolved.functions {
        for r in &rf.body_refs {
            if r.span.start == span.start && r.span.end == span.end {
                return Some(r.def);
            }
        }
    }
    for tv in &resolved.top_vals {
        for r in &tv.init_refs {
            if r.span.start == span.start && r.span.end == span.end {
                return Some(r.def);
            }
        }
    }
    None
}

fn hover_for_def(
    def: DefId,
    resolved: &ResolvedFile,
    typed: &TypedFile,
    interner: &Interner,
    ast: &KtFile,
) -> Option<String> {
    match def {
        DefId::Function(idx) => {
            if let Some(sig) = typed.top_signatures.get(&DefId::Function(idx)) {
                // Find the function name from the AST.
                let mut fn_count = 0u32;
                for decl in &ast.decls {
                    match decl {
                        skotch_syntax::ast::Decl::Fun(f) => {
                            if fn_count == idx {
                                let name = interner.resolve(f.name);
                                let params: Vec<String> = f
                                    .params
                                    .iter()
                                    .enumerate()
                                    .map(|(i, p)| {
                                        let pname = interner.resolve(p.name);
                                        let pty = sig
                                            .params
                                            .get(i)
                                            .map(|t| t.display_name())
                                            .unwrap_or_else(|| "?");
                                        format!("{pname}: {pty}")
                                    })
                                    .collect();
                                let ret = sig.ret.display_name();
                                let ret_str = if ret == "Unit" {
                                    String::new()
                                } else {
                                    format!(": {ret}")
                                };
                                return Some(format!("fun {name}({}){ret_str}", params.join(", ")));
                            }
                            fn_count += 1;
                        }
                        skotch_syntax::ast::Decl::Class(c) => {
                            // Classes get high-index DefIds.
                            let name = interner.resolve(c.name);
                            if typed.top_signatures.contains_key(&def) {
                                return Some(format!("class {name}"));
                            }
                        }
                        _ => {}
                    }
                }
                None
            } else {
                None
            }
        }
        DefId::PrintlnIntrinsic => Some("fun println(message: Any?): Unit".into()),
        DefId::Param(fn_idx, param_idx) => {
            // Look up the function's param type.
            if let Some(sig) = typed.top_signatures.get(&DefId::Function(fn_idx)) {
                let ty = sig
                    .params
                    .get(param_idx as usize)
                    .map(|t| t.display_name())
                    .unwrap_or_else(|| "?");
                // Get param name from AST.
                let mut fn_count = 0u32;
                for decl in &ast.decls {
                    if let skotch_syntax::ast::Decl::Fun(f) = decl {
                        if fn_count == fn_idx {
                            let offset = if f.receiver_ty.is_some() { 1 } else { 0 };
                            if param_idx >= offset {
                                if let Some(p) = f.params.get((param_idx - offset) as usize) {
                                    let name = interner.resolve(p.name);
                                    return Some(format!("(parameter) {name}: {ty}"));
                                }
                            }
                            if param_idx == 0 && f.receiver_ty.is_some() {
                                return Some(format!("(receiver) this: {ty}"));
                            }
                        }
                        fn_count += 1;
                    }
                }
                Some(format!("(parameter): {ty}"))
            } else {
                None
            }
        }
        DefId::Local(fn_idx, local_idx) => {
            // Look up the local's type from the typed function.
            let typed_fn = typed.functions.iter().find(|tf| tf.name_index == fn_idx);
            if let Some(tf) = typed_fn {
                let ty = tf
                    .local_tys
                    .get(local_idx as usize)
                    .map(|t| t.display_name())
                    .unwrap_or_else(|| "?");
                // Get local name from resolver.
                if let Some(rf) = resolved.functions.get(fn_idx as usize) {
                    if let Some(&sym) = rf.locals.get(local_idx as usize) {
                        let name = interner.resolve(sym);
                        return Some(format!("val {name}: {ty}"));
                    }
                }
                Some(format!("(local): {ty}"))
            } else {
                None
            }
        }
        DefId::TopLevelVal(idx) => {
            if let Some(tv) = typed.top_vals.get(idx as usize) {
                // Get name from resolved.
                if let Some(rtv) = resolved.top_vals.get(idx as usize) {
                    let name = interner.resolve(rtv.name);
                    return Some(format!("val {name}: {}", tv.ty.display_name()));
                }
                Some(format!("val: {}", tv.ty.display_name()))
            } else {
                None
            }
        }
        DefId::PossibleExternal(sym) => {
            let name = interner.resolve(sym);
            Some(format!("(external) {name}"))
        }
        DefId::Error => None,
    }
}

// ─── Go-to-definition ───────────────────────────────────────────────────────

fn definition_for_def(
    def: DefId,
    ast: &KtFile,
    resolved: &ResolvedFile,
    sm: &SourceMap,
) -> Option<Location> {
    match def {
        DefId::Function(idx) => {
            let mut fn_count = 0u32;
            for decl in &ast.decls {
                match decl {
                    skotch_syntax::ast::Decl::Fun(f) => {
                        if fn_count == idx {
                            return Some(span_to_location(f.name_span, sm));
                        }
                        fn_count += 1;
                    }
                    skotch_syntax::ast::Decl::Class(c) => {
                        // Check if this class got this index.
                        if resolved.top_level.values().any(|&d| d == def) {
                            return Some(span_to_location(c.name_span, sm));
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        DefId::Param(fn_idx, param_idx) => {
            let mut fn_count = 0u32;
            for decl in &ast.decls {
                if let skotch_syntax::ast::Decl::Fun(f) = decl {
                    if fn_count == fn_idx {
                        let offset = if f.receiver_ty.is_some() { 1 } else { 0 };
                        if param_idx >= offset {
                            if let Some(p) = f.params.get((param_idx - offset) as usize) {
                                return Some(span_to_location(p.span, sm));
                            }
                        }
                    }
                    fn_count += 1;
                }
            }
            None
        }
        DefId::Local(_fn_idx, _local_idx) => {
            // Local declarations don't currently store their declaration span
            // in the resolver. We'd need to extend ResolvedFunction to store
            // per-local spans. For now, return None.
            None
        }
        DefId::TopLevelVal(idx) => {
            let mut val_count = 0u32;
            for decl in &ast.decls {
                if let skotch_syntax::ast::Decl::Val(v) = decl {
                    if val_count == idx {
                        return Some(span_to_location(v.name_span, sm));
                    }
                    val_count += 1;
                }
            }
            None
        }
        DefId::PrintlnIntrinsic | DefId::PossibleExternal(_) | DefId::Error => None,
    }
}

fn span_to_location(span: Span, sm: &SourceMap) -> Location {
    let file = sm.get(span.file);
    let (sl, sc) = file.line_col(span.start);
    let (el, ec) = file.line_col(span.end);
    Location {
        uri: Url::from_file_path(&file.path)
            .unwrap_or_else(|_| Url::parse("file:///unknown").unwrap()),
        range: Range {
            start: Position::new(sl.saturating_sub(1), sc.saturating_sub(1)),
            end: Position::new(el.saturating_sub(1), ec.saturating_sub(1)),
        },
    }
}

// ─── Completions ────────────────────────────────────────────────────────────

const KOTLIN_KEYWORDS: &[&str] = &[
    "fun", "val", "var", "if", "else", "return", "when", "while", "do", "for", "in", "break",
    "continue", "class", "object", "package", "import", "true", "false", "null",
];

fn build_completions(
    resolved: &ResolvedFile,
    typed: &TypedFile,
    interner: &Interner,
    ast: &KtFile,
    _source: &str,
    _pos: Position,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // Keywords.
    for &kw in KOTLIN_KEYWORDS {
        items.push(CompletionItem {
            label: kw.into(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }

    // println intrinsic.
    items.push(CompletionItem {
        label: "println".into(),
        kind: Some(CompletionItemKind::FUNCTION),
        detail: Some("fun println(message: Any?): Unit".into()),
        ..Default::default()
    });

    // Top-level functions.
    let mut fn_idx = 0u32;
    for decl in &ast.decls {
        match decl {
            skotch_syntax::ast::Decl::Fun(f) => {
                let name = interner.resolve(f.name).to_string();
                let detail = if let Some(sig) = typed.top_signatures.get(&DefId::Function(fn_idx)) {
                    let params: Vec<String> = f
                        .params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let pname = interner.resolve(p.name);
                            let pty = sig
                                .params
                                .get(i)
                                .map(|t| t.display_name())
                                .unwrap_or_else(|| "?");
                            format!("{pname}: {pty}")
                        })
                        .collect();
                    let ret = sig.ret.display_name();
                    Some(format!("fun {name}({}): {ret}", params.join(", ")))
                } else {
                    None
                };
                items.push(CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail,
                    ..Default::default()
                });
                fn_idx += 1;
            }
            skotch_syntax::ast::Decl::Val(v) => {
                let name = interner.resolve(v.name).to_string();
                items.push(CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::VARIABLE),
                    ..Default::default()
                });
            }
            skotch_syntax::ast::Decl::Class(c) => {
                let name = interner.resolve(c.name).to_string();
                items.push(CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::CLASS),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }

    // Locals and params in scope at the cursor position — use resolver data.
    // We look at all functions and include their locals/params. A more
    // precise implementation would check which function the cursor is in.
    for rf in &resolved.functions {
        for &sym in &rf.params {
            let name = interner.resolve(sym).to_string();
            if !items.iter().any(|i| i.label == name) {
                items.push(CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::VARIABLE),
                    ..Default::default()
                });
            }
        }
        for &sym in &rf.locals {
            let name = interner.resolve(sym).to_string();
            if !items.iter().any(|i| i.label == name) {
                items.push(CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::VARIABLE),
                    ..Default::default()
                });
            }
        }
    }

    // Well-known Java classes for dot-completion context.
    for class in &["System", "Math", "Integer", "Long", "String", "Thread"] {
        items.push(CompletionItem {
            label: (*class).into(),
            kind: Some(CompletionItemKind::CLASS),
            detail: Some(format!("java.lang.{class}")),
            ..Default::default()
        });
    }

    items
}

// ─── LanguageServer trait ───────────────────────────────────────────────────

#[tower_lsp::async_trait]
impl LanguageServer for SkotchLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: SEMANTIC_TOKEN_TYPES.to_vec(),
                                token_modifiers: vec![],
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".into(), ":".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "skotch-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "skotch language server initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let source = params.text_document.text;
        let version = params.text_document.version;
        let diags = self.analyze(&uri, source, version);
        self.client
            .publish_diagnostics(uri, diags, Some(version))
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        // We requested FULL sync, so there's exactly one change with the full text.
        if let Some(change) = params.content_changes.into_iter().next() {
            let diags = self.analyze(&uri, change.text, version);
            self.client
                .publish_diagnostics(uri, diags, Some(version))
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.remove(&uri);
        // Clear diagnostics for the closed file.
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let doc = match self.documents.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file_id = doc.ast.file;
        let tokens = build_semantic_tokens(
            &doc.tokens,
            &doc.resolved,
            &doc.interner,
            &doc.source_map,
            file_id,
        );
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: tokens,
        })))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = match self.documents.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file_id = doc.ast.file;

        let tok = match find_token_at_position(&doc.tokens, &doc.source_map, file_id, pos) {
            Some(t) if t.kind == TokenKind::Ident || t.kind == TokenKind::StringIdentRef => t,
            _ => return Ok(None),
        };

        let def = match find_ref_at_span(&doc.resolved, tok.span) {
            Some(d) => d,
            None => {
                // Could be a declaration name — check if it matches a top-level name.
                let name_str = &doc.source[tok.span.start as usize..tok.span.end as usize];
                let sym = doc.interner.get(name_str);
                match sym.and_then(|s| doc.resolved.top_level.get(&s).copied()) {
                    Some(d) => d,
                    None => return Ok(None),
                }
            }
        };

        let info = hover_for_def(def, &doc.resolved, &doc.typed, &doc.interner, &doc.ast);
        match info {
            Some(text) => Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("```kotlin\n{text}\n```"),
                }),
                range: Some(span_to_range(tok.span, &doc.source_map)),
            })),
            None => Ok(None),
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = match self.documents.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let file_id = doc.ast.file;

        let tok = match find_token_at_position(&doc.tokens, &doc.source_map, file_id, pos) {
            Some(t) if t.kind == TokenKind::Ident => t,
            _ => return Ok(None),
        };

        let def = match find_ref_at_span(&doc.resolved, tok.span) {
            Some(d) => d,
            None => return Ok(None),
        };

        match definition_for_def(def, &doc.ast, &doc.resolved, &doc.source_map) {
            Some(loc) => Ok(Some(GotoDefinitionResponse::Scalar(loc))),
            None => Ok(None),
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = match self.documents.get(uri) {
            Some(d) => d,
            None => return Ok(None),
        };

        let items = build_completions(
            &doc.resolved,
            &doc.typed,
            &doc.interner,
            &doc.ast,
            &doc.source,
            pos,
        );
        Ok(Some(CompletionResponse::Array(items)))
    }
}

fn span_to_range(span: Span, sm: &SourceMap) -> Range {
    let file = sm.get(span.file);
    let (sl, sc) = file.line_col(span.start);
    let (el, ec) = file.line_col(span.end);
    Range {
        start: Position::new(sl.saturating_sub(1), sc.saturating_sub(1)),
        end: Position::new(el.saturating_sub(1), ec.saturating_sub(1)),
    }
}

// ─── Public entry point ─────────────────────────────────────────────────────

/// Run the LSP server on stdin/stdout. Called by `skotch lsp`.
pub async fn run_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(SkotchLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_source(src: &str) -> DocumentState {
        let path = PathBuf::from("/test.kt");
        let mut sm = SourceMap::new();
        let file_id = sm.add(path, src.to_string());
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let lexed = lex(file_id, src, &mut diags);
        let tokens = lexed.tokens.clone();
        let ast = parse_file(&lexed, &mut interner, &mut diags);
        let resolved = resolve_file(&ast, &mut interner, &mut diags);
        let typed = type_check(&ast, &resolved, &mut interner, &mut diags);
        DocumentState {
            source: src.to_string(),
            version: 1,
            tokens,
            ast,
            resolved,
            typed,
            interner,
            source_map: sm,
            diags,
        }
    }

    #[test]
    fn diagnostics_for_valid_code() {
        let state = analyze_source(r#"fun main() { println("hello") }"#);
        assert!(!state.diags.has_errors());
        let lsp_diags = to_lsp_diagnostics(&state.diags, &state.source_map);
        assert!(lsp_diags.is_empty());
    }

    #[test]
    fn diagnostics_for_unresolved_ident() {
        let state = analyze_source("fun main() { println(undefined) }");
        assert!(state.diags.has_errors());
        let lsp_diags = to_lsp_diagnostics(&state.diags, &state.source_map);
        assert!(!lsp_diags.is_empty());
        assert!(lsp_diags[0].message.contains("unresolved"));
    }

    #[test]
    fn semantic_tokens_keywords() {
        let state = analyze_source("fun main() { val x = 42 }");
        let tokens = build_semantic_tokens(
            &state.tokens,
            &state.resolved,
            &state.interner,
            &state.source_map,
            state.ast.file,
        );
        // Should have tokens for: fun, main, val, x, 42
        assert!(tokens.len() >= 4);
        // First token should be keyword `fun` (type 0).
        assert_eq!(tokens[0].token_type, 0);
    }

    #[test]
    fn hover_println() {
        let state = analyze_source(r#"fun main() { println("hi") }"#);
        // Find println's span in the resolved refs.
        let def = DefId::PrintlnIntrinsic;
        let info = hover_for_def(
            def,
            &state.resolved,
            &state.typed,
            &state.interner,
            &state.ast,
        );
        assert_eq!(info, Some("fun println(message: Any?): Unit".into()));
    }

    #[test]
    fn hover_local_val() {
        let state = analyze_source(r#"fun main() { val name = "world"; println(name) }"#);
        // Local(0, 0) should be the val `name`.
        let def = DefId::Local(0, 0);
        let info = hover_for_def(
            def,
            &state.resolved,
            &state.typed,
            &state.interner,
            &state.ast,
        );
        assert_eq!(info, Some("val name: String".into()));
    }

    #[test]
    fn hover_function_def() {
        let src = "fun greet(n: String): String { return n }\nfun main() { greet(\"hi\") }";
        let state = analyze_source(src);
        let def = DefId::Function(0);
        let info = hover_for_def(
            def,
            &state.resolved,
            &state.typed,
            &state.interner,
            &state.ast,
        );
        assert_eq!(info, Some("fun greet(n: String): String".into()));
    }

    #[test]
    fn hover_parameter() {
        let state = analyze_source("fun greet(name: String) { println(name) }");
        let def = DefId::Param(0, 0);
        let info = hover_for_def(
            def,
            &state.resolved,
            &state.typed,
            &state.interner,
            &state.ast,
        );
        assert_eq!(info, Some("(parameter) name: String".into()));
    }

    #[test]
    fn completions_include_keywords_and_functions() {
        let state = analyze_source("fun greet() { }\nfun main() { }");
        let items = build_completions(
            &state.resolved,
            &state.typed,
            &state.interner,
            &state.ast,
            &state.source,
            Position::new(1, 13),
        );
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"fun"));
        assert!(labels.contains(&"val"));
        assert!(labels.contains(&"println"));
        assert!(labels.contains(&"greet"));
        assert!(labels.contains(&"main"));
    }

    #[test]
    fn position_to_byte_offset_basic() {
        let text = "fun main() {\n    println()\n}";
        // Line 1 (0-based), col 4 → 'p' in println
        assert_eq!(position_to_byte_offset(text, 2, 5), Some(17));
    }

    #[test]
    fn definition_for_function() {
        let src = "fun greet() { }\nfun main() { greet() }";
        let state = analyze_source(src);
        let def = DefId::Function(0);
        let loc = definition_for_def(def, &state.ast, &state.resolved, &state.source_map);
        assert!(loc.is_some());
        let loc = loc.unwrap();
        // greet is at line 1, col 4 (0-based).
        assert_eq!(loc.range.start.line, 0);
        assert_eq!(loc.range.start.character, 4);
    }

    #[test]
    fn diagnostics_for_double_literal() {
        let state = analyze_source("fun main() { val pi = 3.14; println(pi) }");
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_null_literal() {
        let state = analyze_source("fun main() { val x = null; println(x) }");
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn hover_double_local() {
        let state = analyze_source("fun main() { val pi = 3.14; println(pi) }");
        let def = DefId::Local(0, 0);
        let info = hover_for_def(
            def,
            &state.resolved,
            &state.typed,
            &state.interner,
            &state.ast,
        );
        assert_eq!(info, Some("val pi: Double".into()));
    }

    #[test]
    fn diagnostics_for_try_finally() {
        let state =
            analyze_source("fun main() { try { println(\"ok\") } finally { println(\"done\") } }");
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_when_expression() {
        let src = r#"fun classify(x: Int): String = when { x < 0 -> "neg" else -> "pos" }"#;
        let state = analyze_source(src);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_elvis_operator() {
        let state = analyze_source(r#"fun f(x: String?): String = x ?: "default""#);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_return_in_if() {
        let state = analyze_source("fun abs(x: Int): Int { if (x < 0) return -x; return x }");
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_string_methods() {
        let state = analyze_source(r#"fun main() { val s = "hello"; println(s.length) }"#);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_init_block() {
        let src = r#"class Foo(val x: Int) { init { println(x) } } fun main() { val f = Foo(1) }"#;
        let state = analyze_source(src);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_default_params() {
        let src = r#"fun greet(name: String = "World") { println(name) } fun main() { greet() }"#;
        let state = analyze_source(src);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_print_builtin() {
        let state = analyze_source(r#"fun main() { print("hello"); println() }"#);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_data_class() {
        let src = "data class Point(val x: Int, val y: Int)\nfun main() { println(Point(1, 2)) }";
        let state = analyze_source(src);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_maxof() {
        let state = analyze_source("fun main() { println(maxOf(1, 2)) }");
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_named_args() {
        let src = "fun f(a: Int, b: Int) { println(a + b) }\nfun main() { f(b = 2, a = 1) }";
        let state = analyze_source(src);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_string_toint() {
        let state = analyze_source(r#"fun main() { val x = "42".toInt(); println(x + 1) }"#);
        assert!(!state.diags.has_errors());
    }

    #[test]
    fn diagnostics_for_increment() {
        let state = analyze_source("fun main() { var x = 0; x++; println(x) }");
        assert!(!state.diags.has_errors());
    }
}
