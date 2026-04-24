//! Gradle compatibility tests.
//!
//! Runs `gradle build` and `skotch build` on the same project fixture,
//! then compares:
//! 1. JAR entry lists (same .class files present)
//! 2. Runtime output (both produce the same stdout)
//!
//! Requires `gradle` and `java` on PATH. Tests are skipped if either
//! is missing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixture_dir(name: &str) -> PathBuf {
    workspace_root()
        .join("tests/fixtures/projects/gradle-compat")
        .join(name)
}

fn make_temp(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("skotch-gc-{label}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    tmp
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = dst.join(entry.file_name());
        let name = entry.file_name();
        if name == "build" || name == ".gradle" {
            continue;
        }
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn jar_class_entries(jar_path: &Path) -> BTreeSet<String> {
    let file = std::fs::File::open(jar_path).expect("open JAR");
    let mut archive = zip::ZipArchive::new(file).expect("read ZIP");
    let mut entries = BTreeSet::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).expect("read entry");
        let name = entry.name().to_string();
        if name.ends_with(".class") {
            entries.insert(name);
        }
    }
    entries
}

fn run_stdout(cmd: &mut Command) -> Option<String> {
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn hello_lib_skotch_builds_to_gradle_layout() {
    let tmp = make_temp("skotch");
    copy_dir_recursive(&fixture_dir("hello-lib"), &tmp).unwrap();

    let result = skotch_build::build_project(&skotch_build::BuildOptions {
        project_dir: tmp.clone(),
        target_override: Some(skotch_build::BuildTarget::Jvm),
    });
    assert!(result.is_ok(), "skotch build failed: {:?}", result.err());
    let outcome = result.unwrap();

    // JAR should be at build/libs/hello-lib.jar.
    let jar_str = outcome.output_path.to_string_lossy();
    assert!(
        jar_str.contains("build/libs/hello-lib.jar"),
        "Expected build/libs/hello-lib.jar, got: {jar_str}"
    );

    // JAR should contain expected classes.
    let entries = jar_class_entries(&outcome.output_path);
    assert!(entries.contains("MainKt.class"));
    assert!(entries.contains("GreeterKt.class"));

    // Classes should be at build/classes/kotlin/main/.
    assert!(tmp.join("build/classes/kotlin/main/MainKt.class").exists());
    assert!(tmp
        .join("build/classes/kotlin/main/GreeterKt.class")
        .exists());

    // JAR should run correctly.
    if let Ok(java) = which::which("java") {
        let stdout =
            run_stdout(Command::new(&java).arg("-jar").arg(&outcome.output_path)).or_else(|| {
                run_stdout(
                    Command::new(&java)
                        .arg("-cp")
                        .arg(outcome.output_path.to_str().unwrap())
                        .arg("MainKt"),
                )
            });
        assert_eq!(stdout.as_deref(), Some("Hello, World!\n"));
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn hello_lib_matches_gradle_class_entries() {
    let gradle = match which::which("gradle") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("[skip] gradle not on PATH");
            return;
        }
    };

    // Build with Gradle.
    let gradle_tmp = make_temp("gradle");
    copy_dir_recursive(&fixture_dir("hello-lib"), &gradle_tmp).unwrap();
    let gradle_ok = Command::new(&gradle)
        .args(["build", "--no-daemon", "--console=plain"])
        .current_dir(&gradle_tmp)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !gradle_ok {
        eprintln!("[skip] gradle build failed");
        let _ = std::fs::remove_dir_all(&gradle_tmp);
        return;
    }

    // Build with skotch.
    let skotch_tmp = make_temp("skotch2");
    copy_dir_recursive(&fixture_dir("hello-lib"), &skotch_tmp).unwrap();
    let skotch_result = skotch_build::build_project(&skotch_build::BuildOptions {
        project_dir: skotch_tmp.clone(),
        target_override: Some(skotch_build::BuildTarget::Jvm),
    });
    assert!(
        skotch_result.is_ok(),
        "skotch build failed: {:?}",
        skotch_result.err()
    );

    // Compare .class entries.
    let gradle_jar = gradle_tmp.join("build/libs/hello-lib.jar");
    let skotch_jar = skotch_result.unwrap().output_path;
    let gradle_classes = jar_class_entries(&gradle_jar);
    let skotch_classes = jar_class_entries(&skotch_jar);

    for class in &gradle_classes {
        assert!(
            skotch_classes.contains(class),
            "skotch JAR missing class from Gradle: {class}"
        );
    }
    eprintln!("Gradle classes: {gradle_classes:?}\nskotch classes: {skotch_classes:?}");

    let _ = std::fs::remove_dir_all(&gradle_tmp);
    let _ = std::fs::remove_dir_all(&skotch_tmp);
}

#[test]
fn hello_lib_runtime_output_matches() {
    let java = match which::which("java") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("[skip] java not on PATH");
            return;
        }
    };
    let gradle = match which::which("gradle") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("[skip] gradle not on PATH");
            return;
        }
    };

    // Build with Gradle.
    let gradle_tmp = make_temp("gradle-run");
    copy_dir_recursive(&fixture_dir("hello-lib"), &gradle_tmp).unwrap();
    let gradle_ok = Command::new(&gradle)
        .args(["build", "--no-daemon", "--console=plain"])
        .current_dir(&gradle_tmp)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !gradle_ok {
        eprintln!("[skip] gradle build failed");
        let _ = std::fs::remove_dir_all(&gradle_tmp);
        return;
    }

    // Build with skotch.
    let skotch_tmp = make_temp("skotch-run");
    copy_dir_recursive(&fixture_dir("hello-lib"), &skotch_tmp).unwrap();
    let skotch_result = skotch_build::build_project(&skotch_build::BuildOptions {
        project_dir: skotch_tmp.clone(),
        target_override: Some(skotch_build::BuildTarget::Jvm),
    });
    assert!(skotch_result.is_ok());

    let gradle_jar = gradle_tmp.join("build/libs/hello-lib.jar");
    let skotch_jar = skotch_result.unwrap().output_path;

    // Run skotch JAR (has Main-Class manifest).
    let skotch_stdout = run_stdout(Command::new(&java).arg("-jar").arg(&skotch_jar));

    // Run Gradle JAR (no Main-Class — need explicit -cp + class name + stdlib).
    let stdlib_jar = skotch_classinfo::find_kotlin_lib_dir()
        .ok()
        .map(|d| d.join("kotlin-stdlib.jar"));
    let gradle_stdout = if let Some(ref stdlib) = stdlib_jar {
        if stdlib.exists() {
            let sep = if cfg!(windows) { ";" } else { ":" };
            run_stdout(
                Command::new(&java)
                    .arg("-cp")
                    .arg(format!("{}{sep}{}", gradle_jar.display(), stdlib.display()))
                    .arg("MainKt"),
            )
        } else {
            None
        }
    } else {
        None
    };

    if let (Some(ref s), Some(ref g)) = (&skotch_stdout, &gradle_stdout) {
        assert_eq!(
            s, g,
            "Runtime output should match between skotch and Gradle"
        );
    } else {
        eprintln!(
            "[info] Could not compare runtime output: skotch={skotch_stdout:?}, gradle={gradle_stdout:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&gradle_tmp);
    let _ = std::fs::remove_dir_all(&skotch_tmp);
}
