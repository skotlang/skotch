//! Minimal Language Server Protocol implementation for skotch.
//!
//! **Status**: stubbed during the legacy-AST removal. The previous
//! implementation operated on `skotch_syntax::ast::KtFile` and is
//! pending rewrite against the typed (SIL-backed) AST. Tracked as
//! task #28 in the migration log.
//!
//! What this stub does:
//!   - Implements enough of the LSP protocol to advertise capabilities
//!     and respond to lifecycle messages (`initialize`, `shutdown`).
//!   - Emits an empty diagnostics list on `didOpen` / `didChange`.
//!   - Returns no semantic tokens, no hover, no completions.
//!
//! That's sufficient for `skotch lsp` to start under a client without
//! errors. Re-implementing the analysis features against the typed AST
//! is follow-up work.
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

pub struct SkotchLanguageServer {
    #[allow(dead_code)]
    client: Client,
}

impl SkotchLanguageServer {
    fn new(client: Client) -> Self {
        Self { client }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for SkotchLanguageServer {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "skotch-lsp (stub)".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

/// Run the LSP server on stdin/stdout. Called by `skotch lsp`.
pub async fn run_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(SkotchLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
