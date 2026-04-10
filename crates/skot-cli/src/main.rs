//! `skot` command-line entry point.
//!
//! PR #1 ships exactly one fully-implemented subcommand: `skot emit`,
//! which compiles a single Kotlin source file directly to one of the
//! supported target formats. The `build`, `test`, and `repl`
//! subcommands are scaffolded so the help text reflects the long-term
//! roadmap, but their bodies print "not yet implemented" and exit
//! cleanly.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use skot_driver::{emit, EmitOptions, Target};

#[derive(Parser, Debug)]
#[command(name = "skot", version, about = "Kotlin 2 toolchain", long_about = None)]
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
    /// Build a project (orchestration; lands in PR #4).
    Build,
    /// Run tests (lands in PR #6).
    Test,
    /// Start the interactive REPL (lands in PR #7).
    Repl,
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
        Command::Build => {
            eprintln!("`skot build` is not yet implemented (planned for PR #4).");
        }
        Command::Test => {
            eprintln!("`skot test` is not yet implemented (planned for PR #6).");
        }
        Command::Repl => {
            eprintln!("`skot repl` is not yet implemented (planned for PR #7).");
        }
    }
    Ok(())
}
