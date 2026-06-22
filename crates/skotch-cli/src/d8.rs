//! `skotch d8` — native reimplementation of the Android SDK `d8` dexer.
//!
//! A thin CLI over [`skotch_d8`]. Supports the core of the `d8` option grammar
//! (`--output`, `--min-api`, `--release`/`--debug`, `@argfile`, multiple
//! `.class`/`.jar`/`.zip` inputs). The dexer is byte-identical to d8 8.10.x for
//! the implemented subset and errors loudly outside it (see
//! `docs/skotch-d8-design.md`).

use anyhow::{bail, Result};
use skotch_d8::{D8Options, Mode};
use std::path::PathBuf;

const VERSION: &str = "skotch-d8 8.10.9-compatible";

pub fn run(args: &[String]) -> Result<()> {
    let mut opts = D8Options::default();
    let mut output: Option<PathBuf> = None;

    // Expand @argfile arguments (one arg per line).
    let mut expanded: Vec<String> = Vec::new();
    for a in args {
        if let Some(file) = a.strip_prefix('@') {
            let content = std::fs::read_to_string(file)?;
            expanded.extend(
                content
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty()),
            );
        } else {
            expanded.push(a.clone());
        }
    }

    let mut i = 0;
    while i < expanded.len() {
        let arg = &expanded[i];
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{}", HELP);
                return Ok(());
            }
            "--version" => {
                println!("{VERSION}");
                return Ok(());
            }
            "--release" => opts.mode = Mode::Release,
            "--debug" => opts.mode = Mode::Debug,
            "--output" => {
                i += 1;
                output = Some(PathBuf::from(&expanded[i]));
            }
            "--min-api" => {
                i += 1;
                opts.min_api = expanded[i]
                    .parse()
                    .map_err(|_| anyhow::anyhow!("bad --min-api"))?;
            }
            // Accepted but not yet acted upon.
            "--lib" | "--classpath" | "--pg-map" | "--main-dex-list" | "--main-dex-rules"
            | "--thread-count" | "--desugared-lib" | "--globals" | "--globals-output" => {
                i += 1; // skip the value
            }
            "--no-desugaring"
            | "--intermediate"
            | "--file-per-class"
            | "--file-per-class-file"
            | "--android-platform-build" => {}
            other if other.starts_with('-') => {
                bail!(
                    "unsupported d8 option: {other} (see docs/skotch-d8-design.md for the roadmap)"
                );
            }
            _ => opts.inputs.push(PathBuf::from(arg)),
        }
        i += 1;
    }

    opts.output = output.unwrap_or_else(|| PathBuf::from("."));
    if opts.inputs.is_empty() {
        bail!("no input files");
    }
    skotch_d8::run(&opts)
}

const HELP: &str = "\
Usage: skotch d8 [options] <input-files>
  <input-files>   .class, .jar, .zip, or .apk files
  --output <dir>  Output directory for classes.dex (default: .)
  --min-api <n>   Minimum Android API level (default: 1)
  --release       Compile without debugging information
  --debug         Compile with debugging information (default)
  --version       Print version
  --help          Print this message

Native reimplementation of the Android SDK d8 dexer. Byte-identical to d8
8.10.x for the implemented subset; see docs/skotch-d8-design.md for the
roadmap (full SSA IR + register allocation, desugaring, multidex).
";
