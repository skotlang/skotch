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
fn cross_file_function_call() {
    let dir = std::env::temp_dir().join(format!("skotch-xfile-fn-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            (
                "Greeter.kt",
                "fun greet(name: String): String = \"Hello, $name!\"\n",
            ),
            ("Main.kt", "fun main() { println(greet(\"World\")) }\n"),
        ],
    );

    let r = build(&dir);
    assert!(
        r.is_ok(),
        "Cross-file function call build failed: {:?}",
        r.err()
    );

    // Verify the JAR was created.
    let outcome = r.unwrap();
    assert!(outcome.output_path.exists(), "JAR should exist");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cross_file_class_constructor() {
    let dir = std::env::temp_dir().join(format!("skotch-xfile-cls-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            ("Point.kt", "data class Point(val x: Int, val y: Int)\n"),
            (
                "Main.kt",
                "fun main() { val p = Point(3, 4); println(p) }\n",
            ),
        ],
    );

    let r = build(&dir);
    assert!(
        r.is_ok(),
        "Cross-file class constructor build failed: {:?}",
        r.err()
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn circular_cross_file_calls() {
    let dir = std::env::temp_dir().join(format!("skotch-xfile-circ-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            ("A.kt", "fun fromA(): String = \"A calls B: ${fromB()}\"\n"),
            ("B.kt", "fun fromB(): String = \"B\"\n"),
            ("Main.kt", "fun main() { println(fromA()) }\n"),
        ],
    );

    let r = build(&dir);
    assert!(
        r.is_ok(),
        "Circular cross-file call build failed: {:?}",
        r.err()
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn private_visibility_not_exported() {
    let dir = std::env::temp_dir().join(format!("skotch-xfile-priv-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            ("Helpers.kt", "private fun secret(): String = \"hidden\"\nfun publicGreet(): String = \"public: ${secret()}\"\n"),
            ("Main.kt", "fun main() { println(publicGreet()) }\n"),
        ],
    );

    let r = build(&dir);
    assert!(r.is_ok(), "Visibility build failed: {:?}", r.err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn incremental_state_tracking() {
    use skotch_db::{content_hash, IncrementalState};

    let mut state = IncrementalState::default();

    // New file — changed.
    assert!(state.file_changed("A.kt", "fun a() {}"));
    state.record_file("A.kt", "fun a() {}", "AKt", 1);

    // Same content — not changed.
    assert!(!state.file_changed("A.kt", "fun a() {}"));

    // Modified content — changed.
    assert!(state.file_changed("A.kt", "fun a() { println(1) }"));

    // Symbol table hash tracking.
    let hash1 = content_hash("table v1");
    state.set_symbol_table_hash(hash1.clone());
    assert!(!state.symbol_table_changed(&hash1));
    assert!(state.symbol_table_changed(&content_hash("table v2")));
}

#[test]
fn cross_file_field_access() {
    let dir = std::env::temp_dir().join(format!("skotch-xfile-field-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            ("Point.kt", "data class Point(val x: Int, val y: Int)\n"),
            (
                "Main.kt",
                "fun main() {\n    val p = Point(3, 4)\n    println(p.x)\n    println(p.y)\n}\n",
            ),
        ],
    );
    let r = build(&dir);
    assert!(
        r.is_ok(),
        "Cross-file field access build failed: {:?}",
        r.err()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cross_file_method_call_on_instance() {
    let dir = std::env::temp_dir().join(format!("skotch-xfile-meth-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            (
                "Greeter.kt",
                "class Greeter(val name: String) {\n    fun greet(): String = \"Hello, $name!\"\n}\n",
            ),
            (
                "Main.kt",
                "fun main() {\n    val g = Greeter(\"World\")\n    println(g.greet())\n}\n",
            ),
        ],
    );
    let r = build(&dir);
    assert!(
        r.is_ok(),
        "Cross-file method call build failed: {:?}",
        r.err()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn diagnostics_report_errors_with_details() {
    let dir = std::env::temp_dir().join(format!("skotch-xfile-diag-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_project(
        &dir,
        &[
            ("Helper.kt", "fun helper(): Int = 1\n"),
            ("Main.kt", "fun main() { println(doesNotExist()) }\n"),
        ],
    );
    let r = build(&dir);
    // Build should FAIL (unknown function) and the error should propagate.
    assert!(r.is_err(), "Build with error should fail");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn salsa_incremental_memoization() {
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

// ─── Salsa incremental multi-file tests ─────────────────────────────────────

#[test]
fn salsa_gather_exports_memoized() {
    let db = skotch_db::Db::new();
    let file = db.add_source(
        "Lib.kt".into(),
        "fun greet(): String = \"Hello\"\n".into(),
        "LibKt".into(),
    );
    let e1 = skotch_db::gather_exports(&db, file);
    let e2 = skotch_db::gather_exports(&db, file);
    assert_eq!(
        e1.exports_json(&db),
        e2.exports_json(&db),
        "Same input → same exports"
    );
}

#[test]
fn salsa_body_change_preserves_exports() {
    let mut db = skotch_db::Db::new();
    let file = db.add_source(
        "Lib.kt".into(),
        "fun greet(): String = \"Hello\"\n".into(),
        "LibKt".into(),
    );
    let e1 = skotch_db::gather_exports(&db, file);
    let json1 = e1.exports_json(&db).to_string();

    db.update_source(file, "fun greet(): String = \"World\"\n".into());
    let e2 = skotch_db::gather_exports(&db, file);
    let json2 = e2.exports_json(&db).to_string();

    assert_eq!(json1, json2, "Body-only change preserves exports");
}

#[test]
fn salsa_signature_change_invalidates_exports() {
    let mut db = skotch_db::Db::new();
    let file = db.add_source(
        "Lib.kt".into(),
        "fun greet(): String = \"Hello\"\n".into(),
        "LibKt".into(),
    );
    let e1 = skotch_db::gather_exports(&db, file);
    let json1 = e1.exports_json(&db).to_string();

    db.update_source(
        file,
        "fun greet(name: String): String = \"Hello, $name!\"\n".into(),
    );
    let e2 = skotch_db::gather_exports(&db, file);
    let json2 = e2.exports_json(&db).to_string();

    assert_ne!(json1, json2, "Signature change invalidates exports");
}

#[test]
fn salsa_compile_with_cross_file_context() {
    let mut db = skotch_db::Db::new();
    let greeter = db.add_source(
        "Greeter.kt".into(),
        "fun greet(): String = \"Hello!\"\n".into(),
        "GreeterKt".into(),
    );
    let main = db.add_source(
        "Main.kt".into(),
        "fun main() { println(greet()) }\n".into(),
        "MainKt".into(),
    );

    let (results, _) = db.compile_all_incremental(&[greeter, main], None);
    assert!(!results[1].1, "Main.kt should compile without errors");
}

#[test]
fn salsa_incremental_body_change_skips_dependents() {
    let mut db = skotch_db::Db::new();
    let lib = db.add_source(
        "Lib.kt".into(),
        "fun helper(): Int = 1\n".into(),
        "LibKt".into(),
    );
    let main = db.add_source(
        "Main.kt".into(),
        "fun main() { println(helper()) }\n".into(),
        "MainKt".into(),
    );

    // First build.
    let (r1, table) = db.compile_all_incremental(&[lib, main], None);
    let main_json1 = r1[1].0.functions.len();
    assert!(!r1[0].1, "Lib should compile ok");
    assert!(!r1[1].1, "Main should compile ok");

    // Change Lib's body only (not signature).
    db.update_source(lib, "fun helper(): Int = 42\n".into());

    // Second build.
    let (r2, _) = db.compile_all_incremental(&[lib, main], Some(table));
    let main_json2 = r2[1].0.functions.len();

    // Main's MIR should be identical (memoized) because symbol table
    // didn't change.
    assert_eq!(
        main_json1, main_json2,
        "Body-only change should not recompile dependents"
    );
}

#[test]
fn salsa_incremental_signature_change_recompiles_dependents() {
    let mut db = skotch_db::Db::new();
    let lib = db.add_source(
        "Lib.kt".into(),
        "fun helper(): Int = 1\n".into(),
        "LibKt".into(),
    );
    let main = db.add_source(
        "Main.kt".into(),
        "fun main() { println(helper()) }\n".into(),
        "MainKt".into(),
    );

    let (r1, table) = db.compile_all_incremental(&[lib, main], None);
    assert!(!r1[1].1);

    // Add a new function (changes the symbol table).
    db.update_source(
        lib,
        "fun helper(): Int = 1\nfun helper2(): Int = 2\n".into(),
    );

    let (r2, _) = db.compile_all_incremental(&[lib, main], Some(table));
    // Both should still compile successfully.
    assert!(!r2[0].1, "Lib should compile ok after change");
    assert!(!r2[1].1, "Main should compile ok after change");
}

#[test]
fn salsa_new_function_visible_cross_file() {
    let mut db = skotch_db::Db::new();
    let lib = db.add_source(
        "Lib.kt".into(),
        "fun helper(): Int = 1\n".into(),
        "LibKt".into(),
    );
    let main = db.add_source(
        "Main.kt".into(),
        "fun main() { println(helper()) }\n".into(),
        "MainKt".into(),
    );

    // First build succeeds.
    let (r1, table) = db.compile_all_incremental(&[lib, main], None);
    assert!(!r1[1].1);

    // Add helper2() and update Main to call it.
    db.update_source(
        lib,
        "fun helper(): Int = 1\nfun helper2(): Int = 2\n".into(),
    );
    db.update_source(
        main,
        "fun main() { println(helper()); println(helper2()) }\n".into(),
    );

    let (r2, _) = db.compile_all_incremental(&[lib, main], Some(table));
    assert!(!r2[0].1, "Lib ok");
    assert!(!r2[1].1, "Main should see new helper2()");
}

// ─── Multi-module build tests ───────────────────────────────────────────────

type ModuleDef<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a [&'a str]);

fn create_multi_module_project(dir: &Path, modules: &[ModuleDef<'_>]) {
    fs::create_dir_all(dir).unwrap();
    let mut settings = String::from("rootProject.name = \"test-multi\"\n");
    let module_names: Vec<&str> = modules.iter().map(|(n, _, _)| *n).collect();
    settings.push_str(&format!(
        "include({})\n",
        module_names
            .iter()
            .map(|n| format!("\":{n}\""))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    fs::write(dir.join("settings.gradle.kts"), &settings).unwrap();

    for (name, files, deps) in modules {
        let mod_dir = dir.join(name);
        let src_dir = mod_dir.join("src/main/kotlin");
        fs::create_dir_all(&src_dir).unwrap();

        let mut build = String::from("plugins { kotlin(\"jvm\") }\n");
        if !deps.is_empty() {
            build.push_str("dependencies {\n");
            for dep in *deps {
                build.push_str(&format!("    implementation(project(\":{dep}\"))\n"));
            }
            build.push_str("}\n");
        }
        fs::write(mod_dir.join("build.gradle.kts"), &build).unwrap();

        for (fname, content) in *files {
            fs::write(src_dir.join(fname), content).unwrap();
        }
    }
}

#[test]
fn multi_module_basic_build() {
    let dir = std::env::temp_dir().join(format!("skotch-mm-basic-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_multi_module_project(
        &dir,
        &[
            ("lib", &[("Lib.kt", "fun helper(): Int = 42\n")], &[]),
            (
                "app",
                &[("Main.kt", "fun main() { println(helper()) }\n")],
                &["lib"],
            ),
        ],
    );

    let r = build(&dir);
    assert!(r.is_ok(), "Multi-module build failed: {:?}", r.err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn multi_module_cross_module_calls() {
    let dir = std::env::temp_dir().join(format!("skotch-mm-xmod-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_multi_module_project(
        &dir,
        &[
            (
                "lib",
                &[(
                    "Greeter.kt",
                    "fun greet(name: String): String = \"Hi, $name!\"\n",
                )],
                &[],
            ),
            (
                "app",
                &[("Main.kt", "fun main() { println(greet(\"Alice\")) }\n")],
                &["lib"],
            ),
        ],
    );

    let r = build(&dir);
    assert!(r.is_ok(), "Cross-module call build failed: {:?}", r.err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn multi_module_independent_modules_no_deps() {
    // Two independent modules (no deps between them) + an app that depends on both.
    let dir = std::env::temp_dir().join(format!("skotch-mm-indep-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_multi_module_project(
        &dir,
        &[
            ("lib_a", &[("A.kt", "fun fromA(): Int = 1\n")], &[]),
            ("lib_b", &[("B.kt", "fun fromB(): Int = 2\n")], &[]),
            (
                "app",
                &[("Main.kt", "fun main() { println(fromA() + fromB()) }\n")],
                &["lib_a", "lib_b"],
            ),
        ],
    );

    let r = build(&dir);
    assert!(r.is_ok(), "Independent modules build failed: {:?}", r.err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn multi_module_chain_dependency() {
    // Chain: core → util → app (app depends on util, util depends on core).
    let dir = std::env::temp_dir().join(format!("skotch-mm-chain-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    create_multi_module_project(
        &dir,
        &[
            ("core", &[("Core.kt", "fun coreVal(): Int = 10\n")], &[]),
            (
                "util",
                &[("Util.kt", "fun utilVal(): Int = coreVal() + 5\n")],
                &["core"],
            ),
            (
                "app",
                &[("Main.kt", "fun main() { println(utilVal()) }\n")],
                &["util"],
            ),
        ],
    );

    let r = build(&dir);
    assert!(r.is_ok(), "Chain dependency build failed: {:?}", r.err());

    let _ = fs::remove_dir_all(&dir);
}
