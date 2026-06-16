//! ART execution harness — the FUNCTIONAL-correctness oracle (Phase 4 foundation).
//!
//! For each `tests/art/<Name>.class` fixture (a class with a deterministic `public static void
//! main`) plus a `<Name>.expected` stdout file, this dexes the class with skotch, runs it on a
//! connected ART device/emulator via `adb … dalvikvm`, and asserts the output matches the JVM
//! reference captured in `.expected`. This validates that skotch's dex is SEMANTICALLY correct
//! even where it diverges byte-for-byte from d8 (register allocation / coalescing choices) — the
//! validation the byte-identity bar can't give once we relax it for the bailing-feature work.
//!
//! Skips (passes) when no adb device is connected, so it doesn't break device-less CI.

use skotch_d8::{dex_classes, D8Options, Mode};
use std::path::PathBuf;
use std::process::Command;

fn adb_path() -> Option<PathBuf> {
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(sdk) = std::env::var(var) {
            let p = PathBuf::from(sdk).join("platform-tools/adb");
            if p.exists() {
                return Some(p);
            }
        }
    }
    let home = std::env::var("HOME").ok()?;
    let p = PathBuf::from(home).join("Library/Android/sdk/platform-tools/adb");
    p.exists().then_some(p)
}

/// Returns true iff exactly-one (or at least one) device line `<serial>\tdevice` is present.
fn has_device(adb: &PathBuf) -> bool {
    Command::new(adb)
        .arg("devices")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim_end().ends_with("\tdevice")))
        .unwrap_or(false)
}

#[test]
fn art_execution() {
    // Opt-in: this pushes to a device and runs dalvikvm (~1 min/fixture), so it stays out of the
    // default suite. Run with `SKOTCH_ART=1 cargo test -p skotch-d8 --test art_exec`.
    if std::env::var("SKOTCH_ART").is_err() {
        eprintln!("SKIP art_execution: set SKOTCH_ART=1 to run (needs a connected ART device)");
        return;
    }
    let Some(adb) = adb_path() else {
        eprintln!("SKIP art_execution: no adb found");
        return;
    };
    if !has_device(&adb) {
        eprintln!("SKIP art_execution: no adb device connected");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art");
    let mut ran = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("class") {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        // A nested/helper class (`Main$Inner.class`) is dexed alongside its top-level class, not on
        // its own — skip it here.
        if name.contains('$') {
            continue;
        }
        let expected = std::fs::read_to_string(dir.join(format!("{name}.expected"))).unwrap();

        // Dex the top-level class + any `<name>$*.class` helpers (e.g. a fixture's own functional
        // interface for a ctor reference) together into one dex.
        let mut cfs = vec![skotch_classfile::parse_class_file(&path).unwrap()];
        let prefix = format!("{name}$");
        let mut helpers: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()) == Some("class")
                    && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with(&prefix))
            })
            .collect();
        helpers.sort();
        for h in &helpers {
            cfs.push(skotch_classfile::parse_class_file(h).unwrap());
        }
        let dex = dex_classes(&cfs, &D8Options { min_api: 1, mode: Mode::Release, ..Default::default() })
            .unwrap_or_else(|e| panic!("{name}: skotch dex failed: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));

        // Push and run on ART.
        let tmp = std::env::temp_dir().join(format!("skotch_art_{name}.dex"));
        std::fs::write(&tmp, &dex).unwrap();
        let remote = format!("/data/local/tmp/skotch_art_{name}.dex");
        assert!(Command::new(&adb).args(["push", tmp.to_str().unwrap(), &remote]).output().unwrap().status.success());
        let out = Command::new(&adb)
            .args(["shell", &format!("cd /data/local/tmp && dalvikvm -cp {remote} {name}")])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            stdout.trim_end(),
            expected.trim_end(),
            "{name}: skotch dex produced WRONG runtime output on ART (a functional MISCOMPILE)"
        );
        let _ = Command::new(&adb).args(["shell", "rm", "-f", &remote]).output();
        ran += 1;
    }
    eprintln!("art_execution: {ran} fixture(s) ran correctly on ART");
    assert!(ran > 0, "no ART fixtures found");
}
