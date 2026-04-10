//! Architectural test: the shipping `skotch` binary must contain none
//! of the strings `kotlinc`, `javac`, `d8`, or `dx`.
//!
//! This enforces the hard rule that skotch does not invoke external
//! Kotlin / Java / Android compilers at runtime. Reference outputs in
//! `tests/fixtures/expected/` are produced by the separate `xtask`
//! binary, which *is* allowed to shell out to those tools.
//!
//! ## Why a substring check works
//!
//! If skotch ever calls `Command::new("kotlinc")`, the literal `"kotlinc"`
//! will appear verbatim in the binary's data section. Same for `"javac"`,
//! `"d8"`, and `"dx"`. The check is surprisingly robust because the
//! workspace doesn't reference those names anywhere else: no module
//! is named after them, no comment string ends up in the read-only
//! data segment.
//!
//! False positives can theoretically come from a Rust string literal
//! that *mentions* the forbidden tool — e.g., a help message saying
//! "use `kotlinc` for X". The test prints the matching offsets so a
//! contributor can audit them.

use std::path::PathBuf;
use std::process::Command;

/// Strings the shipping skotch binary must NOT contain.
///
/// Only long, unambiguous tool names go in this list. We deliberately
/// do not check for `d8` or `dx` because those two-character strings
/// collide with random byte sequences anywhere in the binary (constant
/// pool fragments, panic offsets, etc.). When the DEX backend lands in
/// PR #3 we'll add `"d8 "` (with a trailing space) and `"d8.bat"` as
/// additional needles, which avoid the false-positive problem.
const FORBIDDEN: &[&str] = &["kotlinc", "javac"];

#[test]
fn skotch_binary_contains_no_external_compiler_names() {
    // Build the release binary so we don't accidentally check
    // panic-strings from debug-mode formatting.
    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--release")
        .arg("-p")
        .arg("skotch-cli")
        .status()
        .expect("invoke cargo build");
    assert!(status.success(), "cargo build --release failed");

    // Locate the just-built `skotch` binary. We respect `CARGO_TARGET_DIR`
    // if set (CI sometimes overrides it) and fall back to the workspace's
    // `target/` directory. The binary name has the platform-specific
    // executable suffix appended (`.exe` on Windows, empty elsewhere).
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = here.parent().unwrap().parent().unwrap();
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let exe = target_dir
        .join("release")
        .join(format!("skotch{}", std::env::consts::EXE_SUFFIX));
    let bytes = std::fs::read(&exe).unwrap_or_else(|e| {
        panic!(
            "could not read {} : {e}\n  CARGO_MANIFEST_DIR={}\n  workspace={}\n  target_dir={}",
            exe.display(),
            here.display(),
            workspace.display(),
            target_dir.display(),
        );
    });

    let mut hits: Vec<(String, usize)> = Vec::new();
    for &needle in FORBIDDEN {
        if let Some(pos) = find_subslice(&bytes, needle.as_bytes()) {
            hits.push((needle.to_string(), pos));
        }
    }
    if !hits.is_empty() {
        let report: Vec<String> = hits
            .into_iter()
            .map(|(n, p)| format!("  - {n} at offset 0x{p:x}"))
            .collect();
        panic!(
            "skotch binary contains forbidden tool name(s):\n{}\n\nThe shipping skotch binary must not invoke kotlinc/javac/d8/dx. \
            Move that code into the xtask crate.",
            report.join("\n")
        );
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
