//! Test execution via JUnit Platform Launcher.
//!
//! Compiles test sources, resolves JUnit dependencies, and runs tests
//! by launching a JVM with the JUnit Platform Console Launcher — the
//! same launcher that Gradle's `useJUnitPlatform()` delegates to.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

use crate::discover::discover_sources;

/// Options for `skotch test`.
#[derive(Clone, Debug)]
pub struct TestOptions {
    pub project_dir: PathBuf,
}

/// Result of running tests.
#[derive(Clone, Debug)]
pub struct TestResult {
    pub tests_found: u32,
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub tests_skipped: u32,
    pub success: bool,
    /// JUnit XML report path (if generated).
    pub xml_report_path: Option<PathBuf>,
}

/// JUnit dependency coordinates needed for test execution.
const JUNIT_PLATFORM_LAUNCHER: &str = "org.junit.platform:junit-platform-launcher:1.11.4";
const JUNIT_PLATFORM_CONSOLE: &str = "org.junit.platform:junit-platform-console-standalone:1.11.4";
const JUNIT_JUPITER: &str = "org.junit.jupiter:junit-jupiter:5.11.4";

/// Run tests for a project.
pub fn run_tests(opts: &TestOptions) -> Result<TestResult> {
    let project_dir = &opts.project_dir;

    // ── 1. Parse build file to get test config ──────────────────────────
    let buildfile = project_dir.join("build.gradle.kts");
    if !buildfile.exists() {
        anyhow::bail!("no build.gradle.kts found in {:?}", project_dir);
    }
    let mut interner = skotch_intern::Interner::new();
    let build_text = std::fs::read_to_string(&buildfile)?;
    let mut sm = skotch_span::SourceMap::new();
    let fid = sm.add(buildfile.clone(), build_text.clone());
    let parsed = skotch_buildscript::parse_buildfile(&build_text, fid, &mut interner);
    let project = parsed.project;

    // ── 2. Build main sources first ─────────────────────────────────────
    eprintln!("  compiling main sources...");
    let _build_outcome = crate::build_project(&crate::BuildOptions {
        project_dir: project_dir.clone(),
        target_override: Some(skotch_buildscript::BuildTarget::Jvm),
    })?;

    let main_classes_dir = project_dir.join("build/classes/kotlin/main");

    // ── 3. Discover and compile test sources ────────────────────────────
    let test_src_dirs: Vec<PathBuf> = if project.test_source_dirs.is_empty() {
        vec![project_dir.join("src/test/kotlin")]
    } else {
        project
            .test_source_dirs
            .iter()
            .map(|d| project_dir.join(d))
            .collect()
    };

    let mut test_files: Vec<PathBuf> = Vec::new();
    for dir in &test_src_dirs {
        if dir.exists() {
            test_files.extend(discover_sources(dir).unwrap_or_default());
        }
    }

    if test_files.is_empty() {
        eprintln!("  no test sources found");
        return Ok(TestResult {
            tests_found: 0,
            tests_passed: 0,
            tests_failed: 0,
            tests_skipped: 0,
            success: true,
            xml_report_path: None,
        });
    }

    eprintln!("  compiling {} test files...", test_files.len());

    let test_classes_dir = project_dir.join("build/classes/kotlin/test");
    std::fs::create_dir_all(&test_classes_dir)?;

    // Resolve test dependencies (JUnit jars).
    let mut test_dep_coords: Vec<String> = project.test_deps.clone();
    // Always ensure JUnit Platform Console Standalone is available for execution.
    if !test_dep_coords
        .iter()
        .any(|d| d.contains("junit-platform-console"))
    {
        test_dep_coords.push(JUNIT_PLATFORM_CONSOLE.to_string());
    }
    if !test_dep_coords.iter().any(|d| d.contains("junit-jupiter")) {
        test_dep_coords.push(JUNIT_JUPITER.to_string());
    }
    if !test_dep_coords
        .iter()
        .any(|d| d.contains("junit-platform-launcher"))
    {
        test_dep_coords.push(JUNIT_PLATFORM_LAUNCHER.to_string());
    }

    let test_coords: Vec<skotch_tape::MavenCoord> = test_dep_coords
        .iter()
        .filter_map(|s| skotch_tape::MavenCoord::parse(s))
        .collect();

    let repos = vec!["https://repo1.maven.org/maven2".to_string()];
    let test_resolved = skotch_tape::resolve(&test_coords, &repos, false)
        .with_context(|| "resolving test dependencies")?;

    eprintln!("  {} test dependencies resolved", test_resolved.jars.len());

    // Also resolve main dependencies for classpath.
    let main_dep_coords: Vec<skotch_tape::MavenCoord> = project
        .external_deps
        .iter()
        .filter_map(|s| skotch_tape::MavenCoord::parse(s))
        .collect();
    let main_resolved = if main_dep_coords.is_empty() {
        skotch_tape::ResolvedDeps::default()
    } else {
        skotch_tape::resolve(&main_dep_coords, &repos, false).unwrap_or_default()
    };

    // Set CLASSPATH for test compilation: main classes + main deps + test deps.
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut compile_cp = main_classes_dir.to_string_lossy().to_string();
    for jar in &main_resolved.jars {
        compile_cp.push_str(sep);
        compile_cp.push_str(&jar.to_string_lossy());
    }
    for jar in &test_resolved.jars {
        compile_cp.push_str(sep);
        compile_cp.push_str(&jar.to_string_lossy());
    }
    std::env::set_var("CLASSPATH", &compile_cp);

    // Pre-load classes from dependency JARs into the class registry
    // so the MIR lowerer can resolve external method signatures.
    let all_jars: Vec<std::path::PathBuf> = main_resolved
        .jars
        .iter()
        .chain(test_resolved.jars.iter())
        .cloned()
        .collect();
    skotch_mir_lower::preload_registry_jars(&all_jars);

    // Build PackageSymbolTable from main sources so test files can call
    // functions/classes from the main module.
    let main_src_dir = project_dir.join("src/main/kotlin");
    let main_files = discover_sources(&main_src_dir).unwrap_or_default();
    let mut test_interner = skotch_intern::Interner::new();
    let mut test_diags = skotch_diagnostics::Diagnostics::new();
    let mut test_sm = skotch_span::SourceMap::new();

    // Parse main sources to gather their declarations.
    let mut main_parsed: Vec<(skotch_span::FileId, skotch_syntax::KtFile, String)> = Vec::new();
    for path in &main_files {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let fid = test_sm.add(path.clone(), text.clone());
        let lexed = skotch_lexer::lex(fid, &text, &mut test_diags);
        let ast = skotch_parser::parse_file(&lexed, &mut test_interner, &mut test_diags);
        let wrapper = crate::pipeline_wrapper_class_for(path);
        main_parsed.push((fid, ast, wrapper));
    }
    let main_refs: Vec<(skotch_span::FileId, &skotch_syntax::KtFile, &str)> = main_parsed
        .iter()
        .map(|(fid, ast, wc)| (*fid, ast, wc.as_str()))
        .collect();
    let main_symbols = skotch_resolve::gather_declarations(&main_refs, &test_interner);

    // Now parse test files and gather combined symbol table.
    let mut test_parsed: Vec<(skotch_span::FileId, skotch_syntax::KtFile, String)> = Vec::new();
    for path in &test_files {
        let text = std::fs::read_to_string(path)?;
        let fid = test_sm.add(path.clone(), text.clone());
        let lexed = skotch_lexer::lex(fid, &text, &mut test_diags);
        let ast = skotch_parser::parse_file(&lexed, &mut test_interner, &mut test_diags);
        let wrapper = crate::pipeline_wrapper_class_for(path);
        test_parsed.push((fid, ast, wrapper));
    }
    let test_refs: Vec<(skotch_span::FileId, &skotch_syntax::KtFile, &str)> = test_parsed
        .iter()
        .map(|(fid, ast, wc)| (*fid, ast, wc.as_str()))
        .collect();
    let test_symbols = skotch_resolve::gather_declarations(&test_refs, &test_interner);

    // Merge main + test symbols into combined table.
    let mut combined_symbols = main_symbols;
    for (k, v) in test_symbols.functions {
        combined_symbols.functions.entry(k).or_default().extend(v);
    }
    for (k, v) in test_symbols.vals {
        combined_symbols.vals.entry(k).or_insert(v);
    }
    for (k, v) in test_symbols.classes {
        combined_symbols.classes.entry(k).or_insert(v);
    }

    // Compile test files with the combined symbol table.
    for (_fid, ast, wrapper) in &test_parsed {
        let mir = skotch_driver::compile_ast(
            ast,
            wrapper,
            &mut test_interner,
            &mut test_diags,
            Some(&combined_symbols),
        );
        let classes = skotch_backend_jvm::compile_module(&mir, &test_interner);
        for (name, bytes) in &classes {
            let class_path = test_classes_dir.join(format!("{name}.class"));
            if let Some(parent) = class_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&class_path, bytes)?;
        }
    }

    if test_diags.has_errors() {
        let rendered = skotch_diagnostics::render(&test_diags, &test_sm);
        eprint!("{rendered}");
        anyhow::bail!("test compilation failed");
    }

    // ── 4. Run tests via JUnit Platform Console Launcher ────────────────
    let java = which::which("java").with_context(|| "java not found on PATH")?;

    // Find the console-standalone JAR.
    let console_jar = test_resolved
        .jars
        .iter()
        .find(|j| {
            j.to_string_lossy()
                .contains("junit-platform-console-standalone")
        })
        .with_context(|| "junit-platform-console-standalone JAR not found")?;

    // Build runtime classpath: test classes + main classes + all deps.
    let mut runtime_cp = test_classes_dir.to_string_lossy().to_string();
    runtime_cp.push_str(sep);
    runtime_cp.push_str(&main_classes_dir.to_string_lossy());
    for jar in &main_resolved.jars {
        runtime_cp.push_str(sep);
        runtime_cp.push_str(&jar.to_string_lossy());
    }
    for jar in &test_resolved.jars {
        runtime_cp.push_str(sep);
        runtime_cp.push_str(&jar.to_string_lossy());
    }

    // Create JUnit XML report directory.
    let xml_dir = project_dir.join("build/test-results/test");
    std::fs::create_dir_all(&xml_dir)?;

    eprintln!("  running tests...\n");

    // Launch JUnit Console Launcher.
    let output = Command::new(&java)
        .arg("-jar")
        .arg(console_jar.to_str().unwrap())
        .arg("execute")
        .arg("--classpath")
        .arg(&runtime_cp)
        .arg("--scan-classpath")
        .arg(test_classes_dir.to_str().unwrap())
        .arg("--reports-dir")
        .arg(xml_dir.to_str().unwrap())
        .output()
        .with_context(|| "running JUnit Console Launcher")?;

    // Print test output.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.is_empty() {
        eprint!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }

    // Parse results from exit code.
    let success = output.status.success();
    let xml_report = xml_dir.join("TEST-junit-jupiter.xml");

    // Try to extract counts from the summary line in stdout.
    let (found, passed, failed, skipped) = parse_junit_summary(&stdout);

    Ok(TestResult {
        tests_found: found,
        tests_passed: passed,
        tests_failed: failed,
        tests_skipped: skipped,
        success,
        xml_report_path: if xml_report.exists() {
            Some(xml_report)
        } else {
            None
        },
    })
}

/// Parse JUnit Console Launcher summary output.
/// Handles both plain (`3 tests found`) and bracket-delimited
/// (`[         3 tests found           ]`) formats.
fn parse_junit_summary(output: &str) -> (u32, u32, u32, u32) {
    let mut found = 0u32;
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;

    for line in output.lines() {
        let line = line.trim();
        // Extract the first numeric token from the line.
        let num = line.split_whitespace().find_map(|s| s.parse::<u32>().ok());
        if line.contains("tests found") || line.contains("test found") {
            if let Some(n) = num {
                found = n;
            }
        } else if line.contains("tests successful") || line.contains("test successful") {
            if let Some(n) = num {
                passed = n;
            }
        } else if line.contains("tests failed") || line.contains("test failed") {
            if let Some(n) = num {
                failed = n;
            }
        } else if line.contains("tests skipped")
            || line.contains("test skipped")
            || line.contains("tests aborted")
        {
            if let Some(n) = num {
                skipped = n;
            }
        }
    }

    (found, passed, failed, skipped)
}
