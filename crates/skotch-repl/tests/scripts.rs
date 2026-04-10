//! Integration tests for the `.kts` script runner.
//!
//! Iterates every fixture under `tests/fixtures/scripts/` and runs
//! it through `skotch_repl::run_script`. Each fixture directory is
//! expected to contain:
//!
//! - `script.kts` — the source to execute
//! - `expected.stdout` — the captured output to assert against
//!
//! Both files are committed to git so the tests don't need any
//! external toolchain at fixture-load time.
//!
//! ## Tooling gating
//!
//! `skotch-repl` shells out to `java` for the actual execution,
//! and the JVM target's emit step needs nothing else. The test is
//! gated on `java` being on `PATH` or `$JAVA_HOME` being set —
//! every CI runner has both, but a contributor without a JDK
//! installed gets a `[skip]` line and a passing test rather than
//! a hard failure.
//!
//! ## Line-ending normalization
//!
//! Both `expected.stdout` (a text file pinned to LF by
//! `.gitattributes`) and the JVM subprocess's stdout (which on
//! Windows uses CRLF for `println`) are stripped of `\r` before
//! comparison, mirroring the same trick used by `e2e_jvm.rs`.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR for skotch-repl is `crates/skotch-repl/`.
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

fn scripts_dir() -> PathBuf {
    workspace_root().join("tests/fixtures/scripts")
}

/// Strip `\r` from a string. Defends against Windows CRLF line
/// endings on both the committed text fixture and the JVM
/// subprocess's stdout.
fn strip_cr(s: &str) -> String {
    s.replace('\r', "")
}

#[test]
fn script_fixtures_match_expected_stdout() {
    if skotch_repl::locate_java().is_none() {
        eprintln!("[skip] java not on PATH and JAVA_HOME unset");
        return;
    }

    // Discover fixtures by walking the scripts directory. Sorting by
    // name keeps the test output stable.
    let mut entries: Vec<PathBuf> = std::fs::read_dir(scripts_dir())
        .expect("scripts fixture dir exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();

    let mut failures: Vec<String> = Vec::new();
    let mut count = 0;

    for dir in &entries {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let script = dir.join("script.kts");
        let expected_path = dir.join("expected.stdout");
        if !script.exists() || !expected_path.exists() {
            failures.push(format!("{name}: missing script.kts or expected.stdout"));
            continue;
        }
        count += 1;
        let expected = strip_cr(&std::fs::read_to_string(&expected_path).unwrap());
        match skotch_repl::run_script(&script) {
            Ok(actual) => {
                let actual = strip_cr(&actual);
                if actual != expected {
                    failures.push(format!(
                        "{name}: stdout mismatch\n  expected: {expected:?}\n  actual:   {actual:?}"
                    ));
                }
            }
            Err(e) => {
                failures.push(format!("{name}: run_script failed: {e:#}"));
            }
        }
    }

    assert!(
        count > 0,
        "no script fixtures found under {}",
        scripts_dir().display()
    );
    if !failures.is_empty() {
        panic!(
            "{} of {} script fixtures failed:\n  - {}",
            failures.len(),
            count,
            failures.join("\n  - ")
        );
    }
}

#[test]
fn run_script_str_returns_captured_stdout() {
    if skotch_repl::locate_java().is_none() {
        eprintln!("[skip] java not on PATH and JAVA_HOME unset");
        return;
    }
    let stdout = skotch_repl::run_script_str(r#"println("hi")"#).unwrap();
    assert_eq!(strip_cr(&stdout), "hi\n");
}

#[test]
fn run_script_str_handles_arithmetic() {
    if skotch_repl::locate_java().is_none() {
        eprintln!("[skip] java not on PATH and JAVA_HOME unset");
        return;
    }
    let stdout = skotch_repl::run_script_str(r#"println(1 + 2 * 3)"#).unwrap();
    assert_eq!(strip_cr(&stdout), "7\n");
}

#[test]
fn run_script_str_propagates_compile_errors() {
    if skotch_repl::locate_java().is_none() {
        eprintln!("[skip] java not on PATH and JAVA_HOME unset");
        return;
    }
    // `class` declarations are still unsupported, so this should
    // produce a compile error rather than running anything.
    let result = skotch_repl::run_script_str("class Foo");
    assert!(result.is_err(), "expected a compile error, got {result:?}");
}
