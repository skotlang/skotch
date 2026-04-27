//! Build Server Protocol (BSP 2.2) server for skotch.
//!
//! The BSP server wraps skotch-build to let IDEs drive Kotlin/Android
//! builds through the standardised BSP JSON-RPC protocol. It shares the
//! same project model as the LSP server.
//!
//! # Protocol
//!
//! BSP uses JSON-RPC 2.0 over stdin/stdout with `Content-Length` framing
//! (same as LSP). Method names follow the `build/*`, `buildTarget/*`,
//! and `workspace/*` namespace conventions.
//!
//! # Supported Methods
//!
//! - `build/initialize` / `build/initialized` / `build/shutdown` / `build/exit`
//! - `workspace/buildTargets`
//! - `buildTarget/sources`
//! - `buildTarget/compile`

mod transport;
mod types;

use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub use types::*;

/// Run the BSP server on stdin/stdout.
pub fn run_server() -> Result<()> {
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    let mut server = BspServer::new(stdout);
    server.run(stdin)
}

/// Generate the `.bsp/skotch.json` connection file for IDE auto-discovery.
pub fn generate_connection_file(project_dir: &Path) -> Result<PathBuf> {
    let bsp_dir = project_dir.join(".bsp");
    std::fs::create_dir_all(&bsp_dir)?;
    let conn_file = bsp_dir.join("skotch.json");
    let skotch_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("skotch"));
    let content = serde_json::json!({
        "name": "skotch",
        "version": env!("CARGO_PKG_VERSION"),
        "bspVersion": "2.2.0",
        "languages": ["kotlin"],
        "argv": [skotch_path.to_string_lossy(), "bsp"]
    });
    std::fs::write(&conn_file, serde_json::to_string_pretty(&content)?)?;
    Ok(conn_file)
}

// ─── Server ─────────────────────────────────────────────────────────────────

struct BspServer<W: Write> {
    writer: W,
    project_dir: Option<PathBuf>,
    initialized: bool,
}

impl<W: Write> BspServer<W> {
    fn new(writer: W) -> Self {
        Self {
            writer,
            project_dir: None,
            initialized: false,
        }
    }

    fn run<R: BufRead>(&mut self, reader: R) -> Result<()> {
        let mut reader = reader;
        loop {
            let msg = match transport::read_message(&mut reader) {
                Ok(Some(msg)) => msg,
                Ok(None) => break, // EOF
                Err(e) => {
                    eprintln!("BSP read error: {e}");
                    continue;
                }
            };

            let id = msg.get("id").cloned();
            let method = msg
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();

            match method.as_str() {
                "build/initialize" => {
                    let params = msg.get("params").cloned().unwrap_or_default();
                    let response = self.handle_initialize(&params);
                    if let Some(id) = id {
                        self.send_response(id, response)?;
                    }
                }
                "build/initialized" => {
                    self.initialized = true;
                }
                "build/shutdown" => {
                    if let Some(id) = id {
                        self.send_response(id, serde_json::Value::Null)?;
                    }
                }
                "build/exit" => break,
                "workspace/buildTargets" => {
                    let response = self.handle_build_targets();
                    if let Some(id) = id {
                        self.send_response(id, response)?;
                    }
                }
                "buildTarget/sources" => {
                    let params = msg.get("params").cloned().unwrap_or_default();
                    let response = self.handle_sources(&params);
                    if let Some(id) = id {
                        self.send_response(id, response)?;
                    }
                }
                "buildTarget/compile" => {
                    let params = msg.get("params").cloned().unwrap_or_default();
                    let response = self.handle_compile(&params);
                    if let Some(id) = id {
                        self.send_response(id, response)?;
                    }
                }
                _ => {
                    // Unknown method — respond with method not found.
                    if let Some(id) = id {
                        self.send_error(id, -32601, &format!("Method not found: {method}"))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn send_response(&mut self, id: serde_json::Value, result: serde_json::Value) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        });
        transport::write_message(&mut self.writer, &msg)
    }

    fn send_error(&mut self, id: serde_json::Value, code: i32, message: &str) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        });
        transport::write_message(&mut self.writer, &msg)
    }

    #[allow(dead_code)]
    fn send_notification(&mut self, method: &str, params: serde_json::Value) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        transport::write_message(&mut self.writer, &msg)
    }

    // ─── Request handlers ───────────────────────────────────────────────

    fn handle_initialize(&mut self, params: &serde_json::Value) -> serde_json::Value {
        // Extract rootUri from the initialize params.
        if let Some(root) = params
            .get("rootUri")
            .and_then(|u| u.as_str())
            .and_then(|u| u.strip_prefix("file://"))
        {
            self.project_dir = Some(PathBuf::from(root));
        }

        serde_json::json!({
            "displayName": "skotch",
            "version": env!("CARGO_PKG_VERSION"),
            "bspVersion": "2.2.0",
            "capabilities": {
                "compileProvider": { "languageIds": ["kotlin"] },
                "testProvider": { "languageIds": ["kotlin"] },
                "runProvider": { "languageIds": ["kotlin"] },
                "canReload": true
            }
        })
    }

    fn handle_build_targets(&self) -> serde_json::Value {
        let project_dir = match &self.project_dir {
            Some(d) => d.clone(),
            None => {
                return serde_json::json!({ "targets": [] });
            }
        };

        let mut targets = Vec::new();

        // Parse settings to discover modules.
        let settings_path = project_dir.join("settings.gradle.kts");
        let mut interner = skotch_intern::Interner::new();
        let modules: Vec<String> = if settings_path.exists() {
            let text = std::fs::read_to_string(&settings_path).unwrap_or_default();
            let parsed =
                skotch_buildscript::parse_settings(&text, skotch_span::FileId(0), &mut interner);
            if parsed.settings.included_modules.is_empty() {
                vec![String::new()] // single-module project
            } else {
                parsed.settings.included_modules
            }
        } else {
            vec![String::new()]
        };

        for module_path in &modules {
            let module_name = if module_path.is_empty() {
                project_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project")
                    .to_string()
            } else {
                module_path.trim_start_matches(':').to_string()
            };
            let module_dir = if module_path.is_empty() {
                project_dir.clone()
            } else {
                project_dir.join(module_path.trim_start_matches(':'))
            };
            let uri = format!("file://{}", module_dir.display());

            // Main target
            targets.push(serde_json::json!({
                "id": { "uri": format!("{uri}?id=main") },
                "displayName": format!("{module_name} [main]"),
                "baseDirectory": uri,
                "tags": ["library"],
                "languageIds": ["kotlin"],
                "capabilities": {
                    "canCompile": true,
                    "canRun": true,
                    "canTest": false
                },
                "dataKind": "jvm",
                "data": { "javaHome": "", "javaVersion": "17" }
            }));

            // Test target (if test dir exists)
            let test_dir = module_dir.join("src/test/kotlin");
            if test_dir.exists() {
                targets.push(serde_json::json!({
                    "id": { "uri": format!("{uri}?id=test") },
                    "displayName": format!("{module_name} [test]"),
                    "baseDirectory": uri,
                    "tags": ["test"],
                    "languageIds": ["kotlin"],
                    "dependencies": [{ "uri": format!("{uri}?id=main") }],
                    "capabilities": {
                        "canCompile": true,
                        "canRun": false,
                        "canTest": true
                    },
                    "dataKind": "jvm",
                    "data": { "javaHome": "", "javaVersion": "17" }
                }));
            }
        }

        serde_json::json!({ "targets": targets })
    }

    fn handle_sources(&self, params: &serde_json::Value) -> serde_json::Value {
        let targets = params
            .get("targets")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let mut items = Vec::new();

        for target in &targets {
            let uri = target.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            // Parse URI: "file:///path?id=main" → ("/path", "main")
            let (base_path, source_set) = if let Some((path, query)) = uri.split_once('?') {
                let set = query.strip_prefix("id=").unwrap_or("main");
                (path.strip_prefix("file://").unwrap_or(path), set)
            } else {
                (uri.strip_prefix("file://").unwrap_or(uri), "main")
            };

            let src_dir = if source_set == "test" {
                format!("{base_path}/src/test/kotlin")
            } else {
                format!("{base_path}/src/main/kotlin")
            };

            items.push(serde_json::json!({
                "target": target,
                "sources": [{
                    "uri": format!("file://{src_dir}"),
                    "kind": 1, // directory
                    "generated": false
                }]
            }));
        }

        serde_json::json!({ "items": items })
    }

    fn handle_compile(&mut self, params: &serde_json::Value) -> serde_json::Value {
        let targets = params
            .get("targets")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

        let project_dir = match &self.project_dir {
            Some(d) => d.clone(),
            None => {
                return serde_json::json!({ "statusCode": 2 }); // error
            }
        };

        // For each target, run the build.
        for _target in &targets {
            match skotch_build::build_project(&skotch_build::BuildOptions {
                project_dir: project_dir.clone(),
                target_override: Some(skotch_buildscript::BuildTarget::Jvm),
            }) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("BSP compile error: {e}");
                    return serde_json::json!({ "statusCode": 2 });
                }
            }
        }

        serde_json::json!({ "statusCode": 1 }) // OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_response_has_capabilities() {
        let mut buf = Vec::new();
        let mut server = BspServer::new(&mut buf);
        let params = serde_json::json!({
            "rootUri": "file:///tmp/test-project",
            "displayName": "test-ide",
            "version": "1.0",
            "bspVersion": "2.2.0",
            "capabilities": {}
        });
        let resp = server.handle_initialize(&params);
        assert_eq!(resp["displayName"], "skotch");
        assert_eq!(resp["bspVersion"], "2.2.0");
        assert!(resp["capabilities"]["compileProvider"].is_object());
        assert_eq!(server.project_dir, Some(PathBuf::from("/tmp/test-project")));
    }

    #[test]
    fn build_targets_empty_for_missing_project() {
        let server = BspServer::new(Vec::<u8>::new());
        let resp = server.handle_build_targets();
        let targets = resp["targets"].as_array().unwrap();
        assert!(targets.is_empty());
    }

    #[test]
    fn connection_file_generates_valid_json() {
        let tmp = std::env::temp_dir().join("skotch-bsp-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = generate_connection_file(&tmp).unwrap();
        assert!(path.exists());
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["name"], "skotch");
        assert_eq!(content["bspVersion"], "2.2.0");
        assert!(content["languages"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("kotlin")));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
