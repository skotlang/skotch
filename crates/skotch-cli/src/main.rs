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
    /// Each line is wrapped in a synthetic `fun main()` (for
    /// expressions) or appended to the accumulated declaration
    /// history (for `val`/`var`/`fun`), compiled to JVM bytecode,
    /// and executed in a `java` subprocess from `$JAVA_HOME`.
    Repl,
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
    /// Build a project (orchestration; lands in a later PR).
    Build,
    /// Run tests (lands in a later PR).
    Test,
}

fn main() -> Result<()> {
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
        Command::Repl => {
            let stdin = io::stdin();
            if stdin.is_terminal() {
                // Interactive terminal: use reedline for line editing,
                // command history, and Ctrl-R search.
                skotch_repl::run_repl_interactive()?;
            } else {
                // Piped input (e.g. `echo 'println(1)' | skotch repl`):
                // use the plain BufRead path which doesn't touch the
                // terminal and produces machine-readable output.
                skotch_repl::run_repl(BufReader::new(stdin.lock()), io::stdout().lock())?;
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
        Command::Build => {
            eprintln!("`skotch build` is not yet implemented.");
        }
        Command::Test => {
            eprintln!("`skotch test` is not yet implemented.");
        }
    }
    Ok(())
}
