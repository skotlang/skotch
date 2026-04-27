//! `skotch` command-line entry point.
//!
//! Subcommands:
//!
//! - `emit`  — compile one `.kt` file to a target format
//! - `repl`  — interactive Kotlin REPL backed by `skotch-repl`
//! - `run`   — execute a `.kts` script via the same backend
//! - `build` — full project build (stub, lands later)
//! - `test`  — test runner (stub, lands later)

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::{self, BufReader, IsTerminal};
use std::path::PathBuf;

use skotch_driver::{emit, EmitOptions, Target};

#[derive(Parser, Debug)]
#[command(name = "skotch", version, about = "Kotlin 2 toolchain", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Compile a single Kotlin source file to a target format.
    Emit {
        /// Target format: jvm, dex, llvm, or wasm.
        #[arg(long, value_name = "TARGET")]
        target: String,
        /// Output file path.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: PathBuf,
        /// Optional path to also write the normalized text form.
        #[arg(long = "norm-out", value_name = "FILE")]
        norm_out: Option<PathBuf>,
        /// Input `.kt` source file.
        input: PathBuf,
    },
    /// Start the interactive Kotlin REPL.
    ///
    /// With no flags, opens an interactive prompt. With `--exec` or
    /// `--file`, runs the given code first. By default the REPL
    /// stays open after executing `--exec`/`--file` input so you
    /// can inspect the resulting state; pass `--exit-after` to quit
    /// immediately after execution instead.
    Repl {
        /// Execute the given Kotlin snippet before entering the
        /// interactive prompt. The snippet is processed line by line
        /// as if the user had typed it.
        ///
        /// Example: `skotch repl --exec 'val x = 5; println(x)'`
        #[arg(short = 'e', long = "exec", value_name = "CODE")]
        exec: Option<String>,

        /// Read and execute a script file before entering the
        /// interactive prompt. Each line of the file is processed as
        /// a REPL turn (declarations accumulate, expressions run).
        ///
        /// Example: `skotch repl --file setup.kts`
        #[arg(short = 'f', long = "file", value_name = "PATH")]
        file: Option<PathBuf>,

        /// Exit immediately after executing `--exec` / `--file`
        /// input instead of dropping into the interactive prompt.
        /// Ignored when neither `--exec` nor `--file` is given.
        ///
        /// Example: `skotch repl --exec 'println("hi")' --exit-after`
        #[arg(long = "exit-after")]
        exit_after: bool,

        /// When to index classes on the classpath for tab completion.
        ///
        /// - `background` (default): scan in a background thread;
        ///   the prompt appears immediately.
        /// - `eager`: scan before the first prompt and report timing.
        /// - `lazy`: defer until the first Tab keypress.
        /// - `none`: disable classpath indexing entirely.
        #[arg(long = "scanlib", value_name = "MODE", default_value = "background")]
        scanlib: String,

        /// Show extra diagnostic output (history path, classpath
        /// scan timing, etc.).
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },
    /// Execute a Kotlin script (`.kts`) file.
    ///
    /// The whole file is wrapped in a synthetic `fun main()`,
    /// compiled to JVM bytecode, and run via `java`. Top-level
    /// `val`/`var` become locals; top-level `fun` is not yet
    /// supported in `.kts` files.
    Run {
        /// Path to the `.kts` script file.
        script: PathBuf,
    },
    /// Build a project from build.gradle.kts.
    ///
    /// Discovers source files, compiles them, and packages the result
    /// into a JAR (JVM) or APK (Android).
    Build {
        /// Project directory (default: current directory).
        #[arg(short = 'C', long = "project-dir", value_name = "DIR")]
        project_dir: Option<PathBuf>,
        /// Override target: jvm or android (default: infer from build file).
        #[arg(long = "target", value_name = "TARGET")]
        target: Option<String>,
    },
    /// Start the Language Server Protocol server (stdin/stdout).
    ///
    /// Used by editors (VS Code, Neovim, etc.) for real-time diagnostics,
    /// completions, hover, and go-to-definition.
    Lsp,
    /// Run tests via JUnit Platform Console Launcher.
    ///
    /// Compiles test sources, resolves JUnit dependencies, and runs
    /// tests discovered from `src/test/kotlin/` (or custom sourceSets).
    Test {
        /// Project directory (default: current directory).
        #[arg(short = 'C', long = "project-dir", value_name = "DIR")]
        project_dir: Option<PathBuf>,
    },
    /// Start the Build Server Protocol (BSP 2.2) server on stdin/stdout.
    ///
    /// Used by IDEs (IntelliJ, VS Code) for build target discovery,
    /// compilation, and test execution through the standardised BSP
    /// protocol. Auto-discovered via `.bsp/skotch.json`.
    Bsp,
    /// Generate the `.bsp/skotch.json` connection file for IDE discovery.
    Init,
}

fn main() -> Result<()> {
    // ── Shebang support ─────────────────────────────────────────────
    // When invoked as `#!/usr/bin/env skotch`, the OS passes the script
    // path as the first argument: `skotch myscript.kts`. Detect this
    // and route to the `run` subcommand automatically.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 {
        let first_arg = &args[1];
        if !first_arg.starts_with('-')
            && !matches!(
                first_arg.as_str(),
                "emit" | "repl" | "run" | "build" | "lsp" | "test" | "bsp" | "init" | "help"
            )
            && (first_arg.ends_with(".kts") || first_arg.ends_with(".kt"))
        {
            let path = std::path::PathBuf::from(first_arg);
            if path.exists() {
                let captured = skotch_repl::run_script(&path)?;
                print!("{captured}");
                return Ok(());
            }
        }
    }

    let cli = Cli::parse();
    match cli.cmd {
        Command::Emit {
            target,
            output,
            norm_out,
            input,
        } => {
            let target = Target::from_name(&target).context("parsing --target")?;
            emit(&EmitOptions {
                input,
                output,
                target,
                norm_out,
            })?;
        }
        Command::Repl {
            exec,
            file,
            exit_after,
            scanlib,
            verbose,
        } => {
            let scan_mode: skotch_repl::ScanMode = scanlib
                .parse()
                .map_err(|e: String| anyhow::anyhow!("{e}"))?;
            let has_prelude = exec.is_some() || file.is_some();

            // Build the prelude source from --exec and/or --file.
            // Both can be given at the same time; --file runs first,
            // then --exec, matching how shell `-c` and script-file
            // flags compose in other REPLs.
            let mut prelude = String::new();
            if let Some(path) = &file {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                prelude.push_str(&content);
                if !prelude.ends_with('\n') {
                    prelude.push('\n');
                }
            }
            if let Some(code) = &exec {
                // The user may pass multiple statements separated
                // by `;`. Split them into separate lines so the REPL
                // processes each as an independent turn (declarations
                // accumulate, expressions run).
                for part in code.split(';') {
                    let trimmed = part.trim();
                    if !trimmed.is_empty() {
                        prelude.push_str(trimmed);
                        prelude.push('\n');
                    }
                }
            }

            if has_prelude && exit_after {
                // Non-interactive: run the prelude through the piped
                // REPL and exit. No reedline prompt.
                let input = BufReader::new(prelude.as_bytes());
                skotch_repl::run_repl(input, io::stdout().lock())?;
            } else if has_prelude {
                // Run the prelude first through the piped path
                // (which processes each line as a REPL turn), then
                // drop into the interactive prompt so the user can
                // inspect the resulting state.
                //
                // TODO: the current ReplState is not shared between
                // the piped run and the interactive session. For a
                // future PR, skotch_repl should expose a ReplState
                // that both paths can feed into. For now the prelude
                // output is printed but its declarations are NOT
                // visible in the subsequent interactive session.
                let input = BufReader::new(prelude.as_bytes());
                skotch_repl::run_repl(input, io::stdout().lock())?;
                let stdin = io::stdin();
                if stdin.is_terminal() {
                    skotch_repl::run_repl_interactive(scan_mode, verbose)?;
                }
            } else {
                // No prelude — pure interactive REPL.
                let stdin = io::stdin();
                if stdin.is_terminal() {
                    skotch_repl::run_repl_interactive(scan_mode, verbose)?;
                } else {
                    skotch_repl::run_repl(BufReader::new(stdin.lock()), io::stdout().lock())?;
                }
            }
        }
        Command::Run { script } => {
            let captured = skotch_repl::run_script(&script)?;
            // Print the captured stdout straight through to ours.
            // We don't add a trailing newline; if the script's
            // last println already produced one, the user gets
            // exactly what `java` printed.
            print!("{captured}");
        }
        Command::Build {
            project_dir,
            target,
        } => {
            let dir = project_dir.unwrap_or_else(|| std::env::current_dir().unwrap());
            let target_override = target.map(|t| match t.as_str() {
                "android" => skotch_build::BuildTarget::Android,
                "native" => skotch_build::BuildTarget::Native,
                _ => skotch_build::BuildTarget::Jvm,
            });
            let opts = skotch_build::BuildOptions {
                project_dir: dir,
                target_override,
            };
            skotch_build::build_project(&opts)?;
        }
        Command::Lsp => {
            tokio::runtime::Runtime::new()
                .expect("failed to create tokio runtime")
                .block_on(skotch_lsp::run_server());
        }
        Command::Test { project_dir } => {
            let project_dir = project_dir
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let result = skotch_build::run_tests(&skotch_build::TestOptions { project_dir })?;
            eprintln!(
                "\n  {} tests, {} passed, {} failed",
                result.tests_found, result.tests_passed, result.tests_failed
            );
            if !result.success {
                std::process::exit(1);
            }
        }
        Command::Bsp => {
            skotch_bsp::run_server()?;
        }
        Command::Init => {
            let project_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let path = skotch_bsp::generate_connection_file(&project_dir)?;
            eprintln!("Generated {}", path.display());
        }
    }
    Ok(())
}
