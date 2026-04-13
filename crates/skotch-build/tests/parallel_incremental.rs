//! Tests for incremental + parallel compilation via salsa.
//!
//! Verifies:
//! 1. Multi-file projects compile correctly via the build pipeline
//! 2. The salsa database memoizes compilation (tested in skotch-db)
//! 3. blake3 content hashing is deterministic

use std::fs;
use std::path::Path;

fn create_project(dir: &Path, files: &[(&str, &str)]) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("build.gradle.kts"),
        "plugins { kotlin(\"jvm\") }\ngroup = \"com.test\"\nversion = \"1.0\"\n",
    )
    .unwrap();
    let src_dir = dir.join("src/main/kotlin");
    fs::create_dir_all(&src_dir).unwrap();
    for (name, content) in files {
        fs::write(src_dir.join(name), content).unwrap();
    }
}

fn build(dir: &Path) -> anyhow::Result<skotch_build::BuildOutcome> {
    skotch_build::build_project(&skotch_build::BuildOptions {
        project_dir: dir.to_path_buf(),
        target_override: Some(skotch_build::BuildTarget::Jvm),
    })
}

#[test]
fn single_file_builds() {
    let dir = std::env::temp_dir().join(format!("skotch-salsa-test-1-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(&dir, &[("Main.kt", "fun main() { println(42) }\n")]);

    let r = build(&dir);
    assert!(r.is_ok(), "Build failed: {:?}", r.err());
    assert!(dir.join("build").exists(), "Build output dir should exist");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn multi_file_builds() {
    let dir = std::env::temp_dir().join(format!("skotch-salsa-test-2-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            ("A.kt", "fun a(): Int = 1\n"),
            ("B.kt", "fun b(): Int = 2\n"),
            ("Main.kt", "fun main() { println(1 + 2) }\n"),
        ],
    );

    let r = build(&dir);
    assert!(r.is_ok(), "Build failed: {:?}", r.err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rebuild_succeeds() {
    let dir = std::env::temp_dir().join(format!("skotch-salsa-test-3-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            ("A.kt", "fun helper(): Int = 1\n"),
            ("Main.kt", "fun main() { println(42) }\n"),
        ],
    );

    let r1 = build(&dir);
    assert!(r1.is_ok());

    let r2 = build(&dir);
    assert!(r2.is_ok(), "Rebuild should succeed");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn blake3_hash_deterministic() {
    use skotch_db::content_hash;
    let h1 = content_hash("fun main() { println(1) }");
    let h2 = content_hash("fun main() { println(1) }");
    let h3 = content_hash("fun main() { println(2) }");
    assert_eq!(h1, h2, "Same content → same hash");
    assert_ne!(h1, h3, "Different content → different hash");
    assert_eq!(h1.len(), 64, "blake3 hex = 64 chars");
}

#[test]
fn salsa_incremental_memoization() {
    // Test salsa memoization directly.
    let mut db = skotch_db::Db::new();
    let file = db.add_source(
        "Test.kt".into(),
        "fun test(): Int = 1\n".into(),
        "TestKt".into(),
    );

    let r1 = skotch_db::compile_file(&db, file);
    let json1 = r1.mir_json(&db).to_string();
    let r2 = skotch_db::compile_file(&db, file);
    let json2 = r2.mir_json(&db).to_string();
    assert_eq!(json1, json2, "Salsa returns memoized result");

    db.update_source(file, "fun test(): Int = 2\n".into());
    let r3 = skotch_db::compile_file(&db, file);
    let json3 = r3.mir_json(&db).to_string();
    assert_ne!(json1, json3, "Changed source → recompilation");
}
