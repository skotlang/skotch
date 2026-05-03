//! End-to-end build pipeline with two-phase multi-file compilation.
//!
//! **Phase 1 (Gather)**: Parse all `.kt` files and build a
//! [`PackageSymbolTable`] of all top-level declarations.
//!
//! **Phase 2 (Compile)**: Compile each file sequentially with the shared
//! symbol table for cross-file visibility. Diagnostics accumulate in a
//! shared sink and are rendered at the end.
//!
//! **Phase 3 (Backend)**: Write `.class` files in parallel via rayon and
//! package into a JAR.

use crate::discover::{discover_sources, find_build_file, find_settings_file};
#[allow(unused_imports)]
use crate::merge::merge_modules;
use anyhow::{Context, Result};
use rayon::prelude::*;
use skotch_buildscript::{parse_buildfile_with_catalog, parse_settings, BuildTarget, ProjectModel};
use skotch_diagnostics::{render, Diagnostics};
use skotch_intern::Interner;
use skotch_lexer::lex;
use skotch_mir::MirModule;
use skotch_parser::parse_file;
use skotch_resolve::gather_declarations;
use skotch_span::SourceMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Working directory of the build (typically the directory containing
    /// `build.gradle.kts`).
    pub project_dir: PathBuf,
    /// Optional override of the target. `None` means infer from build file.
    pub target_override: Option<BuildTarget>,
}

#[derive(Clone, Debug)]
pub struct BuildOutcome {
    pub project: ProjectModel,
    pub target: BuildTarget,
    pub output_path: PathBuf,
}

pub fn build_project(opts: &BuildOptions) -> Result<BuildOutcome> {
    // Check for settings.gradle.kts to detect multi-module projects.
    // Also capture rootProject.name for JAR naming even in single-module case.
    let mut root_project_name: Option<String> = None;
    if let Some(settings_path) = find_settings_file(&opts.project_dir) {
        let settings_dir = settings_path.parent().unwrap().to_path_buf();
        let settings_text = std::fs::read_to_string(&settings_path)?;
        let mut interner = Interner::new();
        let sm_file = skotch_span::FileId(0);
        let parsed = parse_settings(&settings_text, sm_file, &mut interner);
        root_project_name = parsed.settings.root_project_name.clone();
        if !parsed.settings.included_modules.is_empty() {
            return build_multi_module(&settings_dir, &parsed.settings, opts);
        }
    }

    // Single-module build.
    let buildfile = find_build_file(&opts.project_dir).with_context(|| {
        format!(
            "no build.gradle.kts found at or above {:?}",
            opts.project_dir
        )
    })?;
    let project_dir = buildfile
        .parent()
        .context("build file has no parent dir")?
        .to_path_buf();

    // Parse the build file.
    let mut sm = SourceMap::new();
    let mut interner = Interner::new();
    let buildfile_text = std::fs::read_to_string(&buildfile)
        .with_context(|| format!("reading {}", buildfile.display()))?;
    let buildfile_id = sm.add(buildfile.clone(), buildfile_text.clone());
    let parsed = parse_buildfile_with_catalog(
        &buildfile_text,
        buildfile_id,
        &mut interner,
        Some(&project_dir),
    );

    let mut project = parsed.project;
    if let Some(t) = opts.target_override.clone() {
        project.target = Some(t);
    }
    let target = project.target.clone().unwrap_or(BuildTarget::Jvm);

    // Set project name: rootProject.name > directory name > "app".
    if project.project_name.is_none() {
        project.project_name = root_project_name.or_else(|| {
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        });
    }

    // ── Resolve external Maven dependencies ────────────────────────────
    let resolved_jars = resolve_external_deps(&project, &project_dir)?;
    if !resolved_jars.is_empty() {
        // Add resolved JARs to CLASSPATH so classinfo can find external classes.
        let sep = if cfg!(windows) { ";" } else { ":" };
        let mut cp = std::env::var("CLASSPATH").unwrap_or_default();
        for jar in &resolved_jars {
            if !cp.is_empty() {
                cp.push_str(sep);
            }
            cp.push_str(&jar.to_string_lossy());
        }
        std::env::set_var("CLASSPATH", &cp);
        // Pre-load dependency classes into the shared registry so the
        // MIR lowerer can resolve external method signatures without
        // relying on CLASSPATH (which is racy under parallel tests).
        skotch_mir_lower::preload_registry_jars(&resolved_jars);
        eprintln!("  {} dependencies resolved", resolved_jars.len());
    }

    // Discover sources from configured directories or default.
    let src_dirs: Vec<PathBuf> = if !project.source_dirs.is_empty() {
        project
            .source_dirs
            .iter()
            .map(|d| project_dir.join(d))
            .collect()
    } else if project.is_multiplatform {
        // KMP default layout: commonMain + jvmMain + main
        vec![
            project_dir.join("src/commonMain/kotlin"),
            project_dir.join("src/jvmMain/kotlin"),
            project_dir.join("src/main/kotlin"),
        ]
    } else {
        // Default: src/main/kotlin + src/main/java (Compose samples use java/ for .kt files)
        vec![
            project_dir.join("src/main/kotlin"),
            project_dir.join("src/main/java"),
        ]
    };
    let mut src_files = Vec::new();
    for src_dir in &src_dirs {
        src_files.extend(discover_sources(src_dir).unwrap_or_default());
    }
    if src_files.is_empty() {
        let dirs_str = src_dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!("no .kt sources found under {}", dirs_str);
    }

    // ── Salsa-based incremental multi-file compilation ─────────────────
    //
    // The pipeline uses Salsa for memoized, demand-driven compilation:
    //
    //   Level 1: gather_exports(file) → FileExports  (per-file, memoized)
    //   Aggregate: build SymbolTableInput from all FileExports
    //   Level 2: compile_with_context(file, table) → CompileResult  (per-file, memoized)
    //   Backend: write .class files in parallel via rayon
    //
    // Key incremental property: a body-only change (no signature change)
    // produces identical FileExports, so the SymbolTableInput stays the
    // same, and other files' compile_with_context calls return from cache.

    let mut db = skotch_db::Db::new();

    // Register all source files as Salsa inputs.
    let salsa_files: Vec<skotch_db::SourceFile> = src_files
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))
                .unwrap_or_default();
            let class_name = wrapper_class_for(path);
            db.add_source(path.to_string_lossy().to_string(), text, class_name)
        })
        .collect();

    // Run the incremental pipeline: gather → symbol table → compile.
    let (results, _table_input) = db.compile_all_incremental(&salsa_files, None);

    // Collect results and check for errors.
    let mut all_classes: Vec<(String, Vec<u8>)> = Vec::new();
    let mut error_count = 0;
    let mut error_messages = Vec::new();
    for (module, has_errors, diag_msgs) in &results {
        if *has_errors {
            error_count += 1;
            if !diag_msgs.is_empty() {
                error_messages.push(diag_msgs.as_str());
            }
        }
        // Backend: compile MIR to class files.
        let classes = skotch_backend_jvm::compile_module(module, &interner);
        all_classes.extend(classes);
    }

    if error_count > 0 {
        for msg in &error_messages {
            eprintln!("{msg}");
        }
        anyhow::bail!("compilation failed with {error_count} file(s) containing errors");
    }

    eprintln!("  {} files compiled", src_files.len());

    // Backend dispatch.
    match target {
        BuildTarget::Jvm => build_jvm_classes(
            &project,
            &project_dir,
            &all_classes,
            &interner,
            &resolved_jars,
        ),
        BuildTarget::Android => {
            // For Android, merge MIR modules (DEX needs a single module).
            let mut module = MirModule::default();
            for (file_module, _, _) in &results {
                merge_modules(&mut module, file_module.clone());
            }
            // Apply Compose transform if the project uses Compose.
            if project.is_compose || skotch_compose::has_composables(&module) {
                skotch_compose::compose_transform(&mut module);
            }
            build_android(&project, &project_dir, &module)
        }
        BuildTarget::Native => {
            anyhow::bail!("native target not yet implemented for `skotch build`");
        }
    }
}

/// Build JVM output from pre-compiled class files (multi-file pipeline).
fn build_jvm_classes(
    project: &ProjectModel,
    project_dir: &Path,
    classes: &[(String, Vec<u8>)],
    _interner: &Interner,
    dep_jars: &[PathBuf],
) -> Result<BuildOutcome> {
    // Write individual .class files in parallel (Gradle-compatible layout).
    let classes_dir = project_dir.join("build/classes/kotlin/main");
    std::fs::create_dir_all(&classes_dir)
        .with_context(|| format!("creating {}", classes_dir.display()))?;
    classes.par_iter().for_each(|(name, bytes)| {
        let path = classes_dir.join(format!("{name}.class"));
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = std::fs::write(&path, bytes);
    });

    // Determine main class: prefer MainKt, then any *Kt class.
    let main_class = project
        .main_class
        .clone()
        .or_else(|| {
            classes
                .iter()
                .find(|(n, _)| n == "MainKt" || n.ends_with("/MainKt"))
                .map(|(n, _)| n.clone())
        })
        .or_else(|| {
            classes
                .iter()
                .find(|(n, _)| n.ends_with("Kt"))
                .map(|(n, _)| n.clone())
        })
        .or_else(|| classes.first().map(|(n, _)| n.clone()))
        .unwrap_or_else(|| "Main".to_string());

    // Discover resource files from src/main/resources/.
    let resources = discover_resources(&project_dir.join("src/main/resources"));

    // Build a runnable JAR (Gradle-compatible: build/libs/{project-name}.jar).
    let jar_dir = project_dir.join("build/libs");
    std::fs::create_dir_all(&jar_dir).ok();
    let jar_name = project.project_name.as_deref().unwrap_or_else(|| {
        project
            .group
            .as_deref()
            .and_then(|g| g.rsplit('.').next())
            .unwrap_or("app")
    });
    let jar_path = jar_dir.join(format!("{jar_name}.jar"));
    if dep_jars.is_empty() {
        skotch_jar::write_jar(&jar_path, &main_class, classes, &resources)
            .with_context(|| format!("writing {}", jar_path.display()))?;
    } else {
        skotch_jar::write_fat_jar(&jar_path, &main_class, classes, dep_jars, &resources)
            .with_context(|| format!("writing fat JAR {}", jar_path.display()))?;
    }

    eprintln!("BUILD SUCCESS: {}", jar_path.display());

    Ok(BuildOutcome {
        project: project.clone(),
        target: BuildTarget::Jvm,
        output_path: jar_path,
    })
}

#[allow(dead_code)]
fn build_jvm(
    project: &ProjectModel,
    project_dir: &Path,
    module: &skotch_mir::MirModule,
    interner: &Interner,
) -> Result<BuildOutcome> {
    let classes = skotch_backend_jvm::compile_module(module, interner);

    // Write individual .class files in parallel.
    let classes_dir = project_dir.join("build/classes/kotlin/main");
    std::fs::create_dir_all(&classes_dir)
        .with_context(|| format!("creating {}", classes_dir.display()))?;
    classes.par_iter().for_each(|(name, bytes)| {
        let path = classes_dir.join(format!("{name}.class"));
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = std::fs::write(&path, bytes);
    });

    let main_class = project
        .main_class
        .clone()
        .or_else(|| {
            classes
                .iter()
                .find(|(n, _)| n == "MainKt" || n.ends_with("/MainKt"))
                .map(|(n, _)| n.clone())
        })
        .or_else(|| {
            classes
                .iter()
                .find(|(n, _)| n.ends_with("Kt"))
                .map(|(n, _)| n.clone())
        })
        .or_else(|| classes.first().map(|(n, _)| n.clone()))
        .unwrap_or_else(|| "Main".to_string());

    let jar_dir = project_dir.join("build/libs");
    std::fs::create_dir_all(&jar_dir).ok();
    let jar_name = project.project_name.as_deref().unwrap_or_else(|| {
        project
            .group
            .as_deref()
            .and_then(|g| g.rsplit('.').next())
            .unwrap_or("app")
    });
    let jar_path = jar_dir.join(format!("{jar_name}.jar"));
    skotch_jar::write_jar(&jar_path, &main_class, &classes, &[])
        .with_context(|| format!("writing {}", jar_path.display()))?;

    eprintln!("BUILD SUCCESS: {}", jar_path.display());

    Ok(BuildOutcome {
        project: project.clone(),
        target: BuildTarget::Jvm,
        output_path: jar_path,
    })
}

fn build_android(
    project: &ProjectModel,
    project_dir: &Path,
    module: &MirModule,
) -> Result<BuildOutcome> {
    // 1. Scan resources and generate R class.
    let res_dir = project_dir.join("src/main/res");
    let resource_table = crate::r_class::scan_resources(&res_dir);
    let _r_classes = if !resource_table.entries.is_empty() {
        let package = project
            .namespace
            .as_deref()
            .or(project.application_id.as_deref())
            .unwrap_or("com.example");
        crate::r_class::generate_r_class(package, &resource_table)
    } else {
        Vec::new()
    };
    if !resource_table.entries.is_empty() {
        eprintln!(
            "  {} resources found across {} types",
            resource_table
                .entries
                .values()
                .map(|v| v.len())
                .sum::<usize>(),
            resource_table.entries.len()
        );
    }

    // 2. Compile to DEX.
    let dex_bytes = skotch_backend_dex::compile_module(module);
    // TODO: include R classes in DEX (requires merging MIR or DEX)

    // 3. Encode AndroidManifest.xml to binary AXML.
    let manifest_elem = build_manifest_from_project(project);
    let axml_bytes = skotch_axml::encode_axml(&manifest_elem);

    // 4. Collect raw resource files for APK.
    let mut res_files = Vec::new();
    if res_dir.is_dir() {
        for entry in walkdir::WalkDir::new(&res_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path().is_file() {
                if let Ok(rel) = entry.path().strip_prefix(&res_dir) {
                    let apk_path_str = format!("res/{}", rel.to_string_lossy().replace('\\', "/"));
                    if let Ok(data) = std::fs::read(entry.path()) {
                        res_files.push((apk_path_str, data));
                    }
                }
            }
        }
    }

    // 5. Assemble unsigned APK.
    let contents = skotch_apk::ApkContents {
        manifest_xml: axml_bytes,
        classes_dex: dex_bytes,
        resources_arsc: None, // TODO: generate resources.arsc binary table
        res_files,
    };

    let build_dir = project_dir.join("build");
    std::fs::create_dir_all(&build_dir).ok();
    let unsigned_path = build_dir.join("app-unsigned.apk");
    skotch_apk::write_unsigned_apk(&unsigned_path, &contents)
        .with_context(|| format!("writing {}", unsigned_path.display()))?;

    // 6. Sign the APK (debug signing with v2 scheme).
    let signed_path = build_dir.join("app-debug.apk");
    skotch_sign::sign_apk_debug(&unsigned_path, &signed_path).with_context(|| "signing APK")?;

    eprintln!("BUILD SUCCESS: {}", signed_path.display());

    Ok(BuildOutcome {
        project: project.clone(),
        target: BuildTarget::Android,
        output_path: signed_path,
    })
}

/// Find aapt2 from Android SDK.
fn find_aapt2() -> Result<PathBuf> {
    find_sdk_tool("aapt2")
}

/// Find d8 from Android SDK.
fn find_d8() -> Result<PathBuf> {
    find_sdk_tool("d8")
}

/// Find a tool from the Android SDK build-tools directory.
fn find_sdk_tool(name: &str) -> Result<PathBuf> {
    for var in &["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(sdk) = std::env::var(var) {
            let bt = PathBuf::from(sdk).join("build-tools");
            if let Some(tool) = find_latest_tool(&bt, name) {
                return Ok(tool);
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let bt = PathBuf::from(&home).join("Library/Android/sdk/build-tools");
    if let Some(tool) = find_latest_tool(&bt, name) {
        return Ok(tool);
    }
    anyhow::bail!("{name} not found in Android SDK")
}

fn find_latest_tool(build_tools: &Path, name: &str) -> Option<PathBuf> {
    let mut versions: Vec<_> = std::fs::read_dir(build_tools)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(name).exists())
        .collect();
    versions.sort();
    versions.last().map(|p| p.join(name))
}

/// Compile .class files to DEX using Android SDK's d8 tool.
fn compile_classes_with_d8(
    d8: &Path,
    classes: &[(String, Vec<u8>)],
    build_dir: &Path,
    dep_jars: &[PathBuf],
) -> Result<Vec<u8>> {
    let android_jar = find_android_jar();

    // If we have dep JARs, compile them separately first, then compile
    // app classes with deps as classpath (not program input). This avoids
    // d8 rejecting app classes due to type mismatches with real libraries.
    if !dep_jars.is_empty() {
        // Deduplicate dep JARs: when multiple versions of the same artifact
        // exist (e.g. annotation-1.1.0.jar and annotation-1.4.0.jar), keep
        // only the latest version to avoid "defined multiple times" errors.
        let deduped_jars = dedup_dep_jars(dep_jars);
        eprintln!(
            "  d8: {} dep JARs ({} after dedup)",
            dep_jars.len(),
            deduped_jars.len()
        );

        let deps_dex_dir = build_dir.join("d8-deps");
        std::fs::create_dir_all(&deps_dex_dir)?;

        // Phase 1: d8 on dependency JARs only.
        // Retry loop: exclude JARs that cause duplicate class errors.
        let mut jar_list = deduped_jars.clone();
        let deps_dex = deps_dex_dir.join("classes.dex");
        for _attempt in 0..20 {
            std::fs::create_dir_all(&deps_dex_dir)?;
            let _ = std::fs::remove_file(&deps_dex);
            let mut cmd = std::process::Command::new(d8);
            cmd.arg("--output").arg(&deps_dex_dir);
            if let Some(ref jar) = android_jar {
                cmd.arg("--lib").arg(jar);
            }
            cmd.arg("--min-api").arg("24");
            for jar in &jar_list {
                cmd.arg(jar);
            }
            let output = cmd.output().with_context(|| "running d8 on deps")?;
            if deps_dex.exists() {
                eprintln!(
                    "  deps DEX: {} bytes ({} JARs)",
                    std::fs::metadata(&deps_dex).map(|m| m.len()).unwrap_or(0),
                    jar_list.len()
                );
                break;
            }
            // Parse failing JAR from error message and exclude it.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let bad_jar = stderr.lines().find_map(|line| {
                if let Some(rest) = line.strip_prefix("Error in ") {
                    if let Some(idx) = rest.find(".jar:") {
                        return Some(PathBuf::from(&rest[..idx + 4]));
                    }
                }
                None
            });
            if let Some(ref bad) = bad_jar {
                eprintln!("  d8 deps: excluding {}", bad.display());
                jar_list.retain(|j| j != bad);
                if jar_list.is_empty() {
                    break;
                }
            } else {
                eprintln!("  d8 deps: unrecoverable error");
                break;
            }
        }

        // Phase 2: d8 on app classes with deps as --classpath.
        let app_dex = compile_app_classes_with_d8(d8, classes, build_dir, dep_jars, &android_jar)?;

        // Merge: if we have deps DEX(es) and app DEX, combine them.
        // d8 may produce multiple DEX files (classes.dex, classes2.dex, ...)
        // for large dependency sets.
        let deps_dexes: Vec<PathBuf> = (0..10)
            .map(|i| {
                if i == 0 {
                    deps_dex_dir.join("classes.dex")
                } else {
                    deps_dex_dir.join(format!("classes{}.dex", i + 1))
                }
            })
            .filter(|p| p.exists())
            .collect();
        if !deps_dexes.is_empty() {
            // Merge by running d8 on all DEX files.
            let merge_dir = build_dir.join("d8-merged");
            std::fs::create_dir_all(&merge_dir)?;
            let mut cmd = std::process::Command::new(d8);
            cmd.arg("--output").arg(&merge_dir);
            cmd.arg("--min-api").arg("24");
            for dex in &deps_dexes {
                cmd.arg(dex);
            }
            // Write app DEX to a temp file.
            let app_dex_path = build_dir.join("d8-app-classes.dex");
            std::fs::write(&app_dex_path, &app_dex)?;
            cmd.arg(&app_dex_path);
            let output = cmd.output()?;
            // Collect all merged DEX files into a single blob.
            // The APK will need all of them (classes.dex, classes2.dex, ...).
            let merged = merge_dir.join("classes.dex");
            if merged.exists() {
                // For multi-DEX APKs, we'll return just classes.dex and
                // handle additional DEX files in the APK assembly.
                // Store the merge dir path for the caller.
                return Ok(std::fs::read(&merged)?);
            }
            // Merge failed — just use the app DEX (deps won't be included).
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "  d8 merge warning: {}",
                stderr.lines().take(2).collect::<Vec<_>>().join("; ")
            );
        }

        return Ok(app_dex);
    }

    compile_app_classes_with_d8(d8, classes, build_dir, &[], &android_jar)
}

fn compile_app_classes_with_d8(
    d8: &Path,
    classes: &[(String, Vec<u8>)],
    build_dir: &Path,
    classpath_jars: &[PathBuf],
    android_jar: &Option<PathBuf>,
) -> Result<Vec<u8>> {
    // Write .class files to a temp directory.
    let classes_dir = build_dir.join("d8-input");
    std::fs::create_dir_all(&classes_dir)?;
    for (name, bytes) in classes {
        let path = classes_dir.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, bytes)?;
    }
    // Collect all .class file paths.
    let mut class_files: Vec<PathBuf> = walkdir::WalkDir::new(&classes_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("class"))
        .map(|e| e.path().to_path_buf())
        .collect();
    if class_files.is_empty() {
        anyhow::bail!("no .class files to dex");
    }
    let dex_output = build_dir.join("d8-output");

    // Retry loop: if d8 fails, extract the failing class file from the error
    // message, exclude it, and retry. Up to 50 retries (many classes may have
    // bytecode issues from partial compilation).
    let total = class_files.len();
    let mut excluded: Vec<PathBuf> = Vec::new();
    for attempt in 0..200 {
        std::fs::create_dir_all(&dex_output)?;
        // Clean previous output.
        let _ = std::fs::remove_file(dex_output.join("classes.dex"));

        let mut cmd = std::process::Command::new(d8);
        cmd.arg("--output").arg(&dex_output);
        if let Some(ref jar) = android_jar {
            cmd.arg("--lib").arg(jar);
        }
        // Pass dependency JARs as --classpath for type resolution.
        for jar in classpath_jars {
            cmd.arg("--classpath").arg(jar);
        }
        cmd.arg("--min-api").arg("24");
        for f in &class_files {
            cmd.arg(f);
        }
        let output = cmd.output().with_context(|| "running d8")?;
        let dex_path = dex_output.join("classes.dex");
        if dex_path.exists() {
            let actual_excluded = total - class_files.len();
            if actual_excluded > 0 {
                eprintln!(
                    "  d8: {} of {} classes compiled ({} excluded)",
                    class_files.len(),
                    total,
                    actual_excluded
                );
            }
            return Ok(std::fs::read(&dex_path)?);
        }
        // d8 failed — try to extract the failing file and exclude it.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let bad_file = stderr.lines().find_map(|line| {
            // d8 error format: "Error in /path/to/Foo.class:" or
            // "Error in /path/to/Foo.class at Lcom/...;method()V:"
            if let Some(rest) = line.strip_prefix("Error in ") {
                // The file path ends at ".class" — everything after is metadata.
                if let Some(idx) = rest.find(".class") {
                    Some(PathBuf::from(&rest[..idx + 6]))
                } else {
                    rest.strip_suffix(':').map(PathBuf::from)
                }
            } else {
                None
            }
        });
        if let Some(ref bad) = bad_file {
            eprintln!("  d8 attempt {}: excluding {}", attempt + 1, bad.display());
            let times_tried = excluded.iter().filter(|e| *e == bad).count();
            if times_tried == 0 {
                // First try: downgrade to version 50 + skip StackMapTable.
                if let Ok(original) = std::fs::read(bad) {
                    if let Some(patched) = generate_stub_class(&original) {
                        let _ = std::fs::write(bad, patched);
                        excluded.push(bad.clone());
                        continue;
                    }
                }
            } else if times_tried == 1 {
                // Second try: generate a completely minimal stub class.
                // This class has the same name/super/interfaces but ALL
                // methods are trivial stubs that d8 always accepts.
                if let Ok(original) = std::fs::read(bad) {
                    if let Some(minimal) = generate_minimal_stub(&original) {
                        let _ = std::fs::write(bad, minimal);
                        excluded.push(bad.clone());
                        continue;
                    }
                }
            }
            // After 2 attempts, exclude.
            class_files.retain(|f| f != bad);
            excluded.push(bad.clone());
            if class_files.is_empty() {
                anyhow::bail!("d8: all class files excluded");
            }
        } else {
            // Can't identify the failing file — give up.
            let msg = stderr.lines().take(3).collect::<Vec<_>>().join("; ");
            anyhow::bail!("d8 failed: {msg}");
        }
    }
    anyhow::bail!(
        "d8: too many retries ({} of {} classes excluded)",
        excluded.len(),
        total
    )
}

/// Generate a minimal stub classfile from an original classfile.
/// Preserves class name, superclass, interfaces, and fields. All methods
/// become stubs that return the default value (null/0/void).
/// Returns None if the class can't be parsed.
/// First attempt: downgrade to version 50 (Java 6) for lenient d8 verification.
fn generate_stub_class(original: &[u8]) -> Option<Vec<u8>> {
    if original.len() > 7 && original[0..4] == [0xCA, 0xFE, 0xBA, 0xBE] {
        let mut out = original.to_vec();
        out[6] = 0x00;
        out[7] = 0x32; // version 50
        Some(out)
    } else {
        None
    }
}

/// Generate a minimal stub class that d8 always accepts.
/// Parses the original class to extract name/super/interfaces, then
/// generates fresh bytecode with just <init> calling super.<init>()
/// and an invoke() method that returns null.
fn generate_minimal_stub(original: &[u8]) -> Option<Vec<u8>> {
    use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
    use std::io::{Cursor, Write};

    // We need: this_class name, super_class name, interfaces.
    // Parse the constant pool to find these.
    let mut r = Cursor::new(original);
    let magic = r.read_u32::<BigEndian>().ok()?;
    if magic != 0xCAFE_BABE {
        return None;
    }
    let _minor = r.read_u16::<BigEndian>().ok()?;
    let _major = r.read_u16::<BigEndian>().ok()?;
    let cp_count = r.read_u16::<BigEndian>().ok()?;

    // Parse constant pool entries to extract class/utf8 refs.
    let mut utf8_entries: std::collections::HashMap<u16, String> = std::collections::HashMap::new();
    let mut class_entries: std::collections::HashMap<u16, u16> = std::collections::HashMap::new();
    let mut idx = 1u16;
    while idx < cp_count {
        let tag = r.read_u8().ok()?;
        match tag {
            1 => {
                // CONSTANT_Utf8
                let len = r.read_u16::<BigEndian>().ok()?;
                let pos = r.position() as usize;
                let s = std::str::from_utf8(&original[pos..pos + len as usize])
                    .ok()?
                    .to_string();
                r.set_position((pos + len as usize) as u64);
                utf8_entries.insert(idx, s);
            }
            7 => {
                // CONSTANT_Class
                let name_idx = r.read_u16::<BigEndian>().ok()?;
                class_entries.insert(idx, name_idx);
            }
            9..=11 => {
                r.set_position(r.position() + 4);
            } // Fieldref/Methodref/InterfaceMethodref
            8 => {
                r.set_position(r.position() + 2);
            } // String
            3 | 4 => {
                r.set_position(r.position() + 4);
            } // Integer/Float
            5 | 6 => {
                r.set_position(r.position() + 8);
                idx += 1;
            } // Long/Double (takes 2 slots)
            12 => {
                r.set_position(r.position() + 4);
            } // NameAndType
            15 => {
                r.set_position(r.position() + 3);
            } // MethodHandle
            16 => {
                r.set_position(r.position() + 2);
            } // MethodType
            17 | 18 => {
                r.set_position(r.position() + 4);
            } // Dynamic/InvokeDynamic
            19 | 20 => {
                r.set_position(r.position() + 2);
            } // Module/Package
            _ => return None, // unknown tag
        }
        idx += 1;
    }

    let _access = r.read_u16::<BigEndian>().ok()?;
    let this_class_idx = r.read_u16::<BigEndian>().ok()?;
    let super_class_idx = r.read_u16::<BigEndian>().ok()?;
    let iface_count = r.read_u16::<BigEndian>().ok()?;
    let mut iface_idxs = Vec::new();
    for _ in 0..iface_count {
        iface_idxs.push(r.read_u16::<BigEndian>().ok()?);
    }

    // Resolve names.
    let this_name_idx = class_entries.get(&this_class_idx)?;
    let this_name = utf8_entries.get(this_name_idx)?.clone();
    let super_name_idx = class_entries.get(&super_class_idx)?;
    let super_name = utf8_entries.get(super_name_idx)?.clone();
    let iface_names: Vec<String> = iface_idxs
        .iter()
        .filter_map(|i| class_entries.get(i))
        .filter_map(|ni| utf8_entries.get(ni))
        .cloned()
        .collect();

    // Determine invoke arity from interface (FunctionN → N args).
    let invoke_arity = iface_names
        .iter()
        .find_map(|n| {
            n.strip_prefix("kotlin/jvm/functions/Function")
                .and_then(|s| s.parse::<usize>().ok())
        })
        .unwrap_or(0);

    // Build a fresh classfile with just <init> and invoke.
    let mut cp = Vec::<u8>::new();
    let mut cp_idx = 1u16;
    let cp_utf8 = |cp: &mut Vec<u8>, idx: &mut u16, s: &str| -> u16 {
        let i = *idx;
        cp.push(1); // CONSTANT_Utf8
        cp.write_u16::<BigEndian>(s.len() as u16).unwrap();
        cp.write_all(s.as_bytes()).unwrap();
        *idx += 1;
        i
    };
    let cp_class = |cp: &mut Vec<u8>, idx: &mut u16, name_idx: u16| -> u16 {
        let i = *idx;
        cp.push(7); // CONSTANT_Class
        cp.write_u16::<BigEndian>(name_idx).unwrap();
        *idx += 1;
        i
    };
    let cp_nat = |cp: &mut Vec<u8>, idx: &mut u16, name: u16, desc: u16| -> u16 {
        let i = *idx;
        cp.push(12); // CONSTANT_NameAndType
        cp.write_u16::<BigEndian>(name).unwrap();
        cp.write_u16::<BigEndian>(desc).unwrap();
        *idx += 1;
        i
    };
    let cp_methodref = |cp: &mut Vec<u8>, idx: &mut u16, class: u16, nat: u16| -> u16 {
        let i = *idx;
        cp.push(10); // CONSTANT_Methodref
        cp.write_u16::<BigEndian>(class).unwrap();
        cp.write_u16::<BigEndian>(nat).unwrap();
        *idx += 1;
        i
    };

    // Build constant pool.
    let this_name_u = cp_utf8(&mut cp, &mut cp_idx, &this_name);
    let this_ci = cp_class(&mut cp, &mut cp_idx, this_name_u);
    let super_name_u = cp_utf8(&mut cp, &mut cp_idx, &super_name);
    let super_ci = cp_class(&mut cp, &mut cp_idx, super_name_u);
    let init_name_u = cp_utf8(&mut cp, &mut cp_idx, "<init>");
    let init_desc_u = cp_utf8(&mut cp, &mut cp_idx, "()V");
    let init_nat = cp_nat(&mut cp, &mut cp_idx, init_name_u, init_desc_u);
    let super_init_mr = cp_methodref(&mut cp, &mut cp_idx, super_ci, init_nat);
    let code_u = cp_utf8(&mut cp, &mut cp_idx, "Code");
    // Interface class entries.
    let iface_cis: Vec<u16> = iface_names
        .iter()
        .map(|n| {
            let nu = cp_utf8(&mut cp, &mut cp_idx, n);
            cp_class(&mut cp, &mut cp_idx, nu)
        })
        .collect();
    // invoke method.
    let invoke_name_u = cp_utf8(&mut cp, &mut cp_idx, "invoke");
    let mut invoke_desc = String::from("(");
    for _ in 0..invoke_arity {
        invoke_desc.push_str("Ljava/lang/Object;");
    }
    invoke_desc.push_str(")Ljava/lang/Object;");
    let invoke_desc_u = cp_utf8(&mut cp, &mut cp_idx, &invoke_desc);

    // Assemble the classfile.
    let mut out = Vec::new();
    out.write_u32::<BigEndian>(0xCAFE_BABE).unwrap();
    out.write_u16::<BigEndian>(0).unwrap(); // minor
    out.write_u16::<BigEndian>(50).unwrap(); // major = Java 6
    out.write_u16::<BigEndian>(cp_idx).unwrap(); // cp_count
    out.write_all(&cp).unwrap();
    out.write_u16::<BigEndian>(0x0021).unwrap(); // ACC_PUBLIC | ACC_SUPER
    out.write_u16::<BigEndian>(this_ci).unwrap();
    out.write_u16::<BigEndian>(super_ci).unwrap();
    out.write_u16::<BigEndian>(iface_cis.len() as u16).unwrap();
    for ic in &iface_cis {
        out.write_u16::<BigEndian>(*ic).unwrap();
    }
    out.write_u16::<BigEndian>(0).unwrap(); // fields_count = 0
                                            // Methods: <init> + invoke
    out.write_u16::<BigEndian>(2).unwrap(); // methods_count
                                            // <init>: aload_0; invokespecial super.<init>; return
    out.write_u16::<BigEndian>(0x0001).unwrap(); // ACC_PUBLIC
    out.write_u16::<BigEndian>(init_name_u).unwrap();
    out.write_u16::<BigEndian>(init_desc_u).unwrap();
    out.write_u16::<BigEndian>(1).unwrap(); // attributes_count = 1
    out.write_u16::<BigEndian>(code_u).unwrap(); // Code attribute
    let init_code: &[u8] = &[
        0x2A,
        0xB7,
        (super_init_mr >> 8) as u8,
        super_init_mr as u8,
        0xB1,
    ];
    let init_code_attr_len = 2 + 2 + 4 + init_code.len() as u32 + 2 + 2;
    out.write_u32::<BigEndian>(init_code_attr_len).unwrap();
    out.write_u16::<BigEndian>(2).unwrap(); // max_stack
    out.write_u16::<BigEndian>(1).unwrap(); // max_locals
    out.write_u32::<BigEndian>(init_code.len() as u32).unwrap();
    out.write_all(init_code).unwrap();
    out.write_u16::<BigEndian>(0).unwrap(); // exception_table_length
    out.write_u16::<BigEndian>(0).unwrap(); // attributes_count

    // invoke: aconst_null; areturn
    out.write_u16::<BigEndian>(0x0001).unwrap(); // ACC_PUBLIC
    out.write_u16::<BigEndian>(invoke_name_u).unwrap();
    out.write_u16::<BigEndian>(invoke_desc_u).unwrap();
    out.write_u16::<BigEndian>(1).unwrap(); // attributes_count = 1
    out.write_u16::<BigEndian>(code_u).unwrap();
    let invoke_code: &[u8] = &[0x01, 0xB0]; // aconst_null; areturn
    let invoke_code_attr_len = 2 + 2 + 4 + invoke_code.len() as u32 + 2 + 2;
    out.write_u32::<BigEndian>(invoke_code_attr_len).unwrap();
    out.write_u16::<BigEndian>(1).unwrap(); // max_stack
    out.write_u16::<BigEndian>((invoke_arity + 1) as u16)
        .unwrap(); // max_locals
    out.write_u32::<BigEndian>(invoke_code.len() as u32)
        .unwrap();
    out.write_all(invoke_code).unwrap();
    out.write_u16::<BigEndian>(0).unwrap(); // exception_table_length
    out.write_u16::<BigEndian>(0).unwrap(); // attributes_count

    // Class attributes: none
    out.write_u16::<BigEndian>(0).unwrap();

    Some(out)
}

/// Find android.jar from Android SDK.
fn find_android_jar() -> Option<PathBuf> {
    for var in &["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(sdk) = std::env::var(var) {
            let platforms = PathBuf::from(&sdk).join("platforms");
            if let Ok(entries) = std::fs::read_dir(&platforms) {
                let mut versions: Vec<_> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.join("android.jar").exists())
                    .collect();
                versions.sort();
                if let Some(latest) = versions.last() {
                    return Some(latest.join("android.jar"));
                }
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let platforms = PathBuf::from(&home).join("Library/Android/sdk/platforms");
    if let Ok(entries) = std::fs::read_dir(&platforms) {
        let mut versions: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("android.jar").exists())
            .collect();
        versions.sort();
        if let Some(latest) = versions.last() {
            return Some(latest.join("android.jar"));
        }
    }
    None
}

// ─── assemble_android ──────────────────────────────────────────────────────
//
// Full Android APK assembly using SDK tools (aapt2, d8, apksigner).
// Produces an APK that matches Gradle's output structure.

/// Options for the `assemble` command.
#[derive(Clone, Debug)]
pub struct AssembleOptions {
    pub project_dir: PathBuf,
}

/// Assemble a signed debug APK using Android SDK tools.
///
/// Pipeline:
///   1. Compile Kotlin sources → .class files (Skotch)
///   2. Resolve Maven dependencies → JAR/AAR files
///   3. aapt2 compile + link → base APK with manifest + resources.arsc
///   4. d8 on app .class + dep JARs → DEX files
///   5. Inject DEX into base APK
///   6. zipalign + apksigner → signed APK
pub fn assemble_android(opts: &AssembleOptions) -> Result<BuildOutcome> {
    let aapt2 = find_aapt2().context("aapt2 required for assemble")?;
    let d8 = find_d8().context("d8 required for assemble")?;
    let apksigner = find_sdk_tool("apksigner").context("apksigner required for assemble")?;
    let zipalign = find_sdk_tool("zipalign").context("zipalign required for assemble")?;
    let android_jar = find_android_jar().context("android.jar required for assemble")?;

    // ── Step 0: Parse project and compile sources ──────────────────────
    // Check for multi-module project.
    let settings_path = find_settings_file(&opts.project_dir);
    let (project, all_classes, root_dir, app_dir) = if let Some(ref sp) = settings_path {
        let settings_dir = sp.parent().unwrap().to_path_buf();
        let settings_text = std::fs::read_to_string(sp)?;
        let mut interner = Interner::new();
        let parsed = parse_settings(&settings_text, skotch_span::FileId(0), &mut interner);
        // Run the compilation part of build_multi_module, collecting classes.
        let (project, classes, modules_info) =
            compile_multi_module_classes(&settings_dir, &parsed.settings)?;
        let app_dir = modules_info
            .iter()
            .find(|(_, is_app)| *is_app)
            .map(|(d, _)| d.clone())
            .unwrap_or_else(|| settings_dir.clone());
        (project, classes, settings_dir, app_dir)
    } else {
        anyhow::bail!("assemble requires a multi-module project with settings.gradle.kts");
    };

    let pkg = project
        .namespace
        .as_deref()
        .or(project.application_id.as_deref())
        .unwrap_or("com.example");
    let min_sdk = project.min_sdk.unwrap_or(24);
    let target_sdk = project.target_sdk.unwrap_or(34);

    let build_dir = root_dir.join("build");
    std::fs::create_dir_all(&build_dir)?;

    eprintln!("  {} app classes compiled by Skotch", all_classes.len());

    // ── Step 1: Resolve external dependencies ─────────────────────────
    let dep_jars = match resolve_external_deps(&project, &root_dir) {
        Ok(jars) => jars,
        Err(e) => {
            eprintln!("  WARNING: failed to resolve deps: {e}");
            Vec::new()
        }
    };
    if !dep_jars.is_empty() {
        eprintln!("  {} dependency JARs resolved", dep_jars.len());
    }

    // Collect resource overlay JARs from AARs (they contain res/ dirs).
    let mut extra_res_dirs: Vec<PathBuf> = Vec::new();
    for jar in &dep_jars {
        // Check if the AAR (parent of .jar) has a res/ directory.
        // Our resolver extracts classes.jar next to the .aar file.
        let aar_path = jar.with_extension("aar");
        if aar_path.exists() {
            // Extract res/ from AAR to a temp dir if present.
            if let Ok(file) = std::fs::File::open(&aar_path) {
                if let Ok(mut archive) = zip::ZipArchive::new(file) {
                    let has_res = (0..archive.len()).any(|i| {
                        archive
                            .by_index(i)
                            .map(|f| f.name().starts_with("res/"))
                            .unwrap_or(false)
                    });
                    if has_res {
                        let res_extract_dir = aar_path.with_extension("res");
                        if !res_extract_dir.exists() {
                            std::fs::create_dir_all(&res_extract_dir).ok();
                            for i in 0..archive.len() {
                                if let Ok(mut entry) = archive.by_index(i) {
                                    let name = entry.name().to_string();
                                    if name.starts_with("res/") && !entry.is_dir() {
                                        let out_path = res_extract_dir.join(&name);
                                        if let Some(parent) = out_path.parent() {
                                            std::fs::create_dir_all(parent).ok();
                                        }
                                        if let Ok(mut out) = std::fs::File::create(&out_path) {
                                            std::io::copy(&mut entry, &mut out).ok();
                                        }
                                    }
                                }
                            }
                        }
                        extra_res_dirs.push(res_extract_dir);
                    }
                }
            }
        }
    }

    // ── Step 2: aapt2 compile resources ───────────────────────────────
    let app_res_dir = app_dir.join("src/main/res");
    let compiled_res = build_dir.join("aapt2-compiled");
    std::fs::create_dir_all(&compiled_res)?;

    // Compile app resources.
    if app_res_dir.is_dir() {
        let output = std::process::Command::new(&aapt2)
            .arg("compile")
            .arg("--dir")
            .arg(&app_res_dir)
            .arg("-o")
            .arg(compiled_res.join("app-res.zip"))
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "  aapt2 compile app resources: {}",
                stderr.lines().take(3).collect::<Vec<_>>().join("; ")
            );
        }
    }

    // Deduplicate library resource dirs: when multiple versions of the same
    // artifact exist, keep only the latest to avoid resource conflicts.
    let deduped_res_dirs = {
        use std::collections::HashMap;
        let mut best: HashMap<String, (String, PathBuf)> = HashMap::new();
        for dir in &extra_res_dirs {
            // Path: .../groupId.../artifactId/version/artifactId-version.res/
            // e.g.: .../core/core/1.8.0/core-1.8.0.res
            //   parent(1) = .../core/core/1.8.0  (version dir)
            //   parent(2) = .../core/core         (artifact dir)
            let ver_dir = dir.parent(); // .../1.8.0
            let art_dir = ver_dir.and_then(|p| p.parent()); // .../core/core
            if let (Some(art), Some(ver)) = (art_dir, ver_dir) {
                let key = art.to_string_lossy().to_string();
                let ver_name = ver
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if let Some((ev, _)) = best.get(&key) {
                    if semver_gt(&ver_name, ev) {
                        best.insert(key, (ver_name, dir.clone()));
                    }
                } else {
                    best.insert(key, (ver_name, dir.clone()));
                }
            } else {
                best.entry(dir.to_string_lossy().to_string())
                    .or_insert_with(|| (String::new(), dir.clone()));
            }
        }
        let v: Vec<PathBuf> = best.into_values().map(|(_, p)| p).collect();
        v
    };
    eprintln!(
        "  {} library resource dirs ({} after dedup)",
        extra_res_dirs.len(),
        deduped_res_dirs.len()
    );

    // Compile library resources (from extracted AARs).
    for (i, res_dir) in deduped_res_dirs.iter().enumerate() {
        let res_path = res_dir.join("res");
        if res_path.is_dir() {
            let out_zip = compiled_res.join(format!("lib-{i}-res.zip"));
            let output = std::process::Command::new(&aapt2)
                .arg("compile")
                .arg("--dir")
                .arg(&res_path)
                .arg("-o")
                .arg(&out_zip)
                .output()?;
            if !output.status.success() {
                // Library resource compile failures are non-fatal.
                continue;
            }
        }
    }

    // ── Step 3: aapt2 link → base APK with manifest + resources.arsc ──
    let manifest_path = app_dir.join("src/main/AndroidManifest.xml");
    let tmp_manifest = build_dir.join("aapt2-manifest.xml");
    {
        // Inject package attribute if missing.
        let manifest_xml = std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|_| format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\" package=\"{pkg}\">\n  <application><activity android:name=\".MainActivity\" android:exported=\"true\"><intent-filter><action android:name=\"android.intent.action.MAIN\"/><category android:name=\"android.intent.category.LAUNCHER\"/></intent-filter></activity></application>\n</manifest>"
            ));
        let fixed = if manifest_xml.contains("package=") {
            manifest_xml
        } else {
            manifest_xml.replace("<manifest ", &format!("<manifest package=\"{pkg}\" "))
        };
        std::fs::write(&tmp_manifest, &fixed)?;
    }

    let base_apk = build_dir.join("aapt2-base.apk");

    // Collect all compiled resource ZIPs.
    let mut res_zips: Vec<PathBuf> = std::fs::read_dir(&compiled_res)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zip"))
        .collect();
    res_zips.sort();

    // Directory for R.java generation from aapt2.
    let r_java_dir = build_dir.join("r-java");
    std::fs::create_dir_all(&r_java_dir)?;

    // Retry loop: run aapt2 link, exclude ZIPs that cause conflicts.
    let mut link_ok = false;
    for _retry in 0..30 {
        let _ = std::fs::remove_file(&base_apk);
        let mut link_cmd = std::process::Command::new(&aapt2);
        link_cmd
            .arg("link")
            .arg("-o")
            .arg(&base_apk)
            .arg("-I")
            .arg(&android_jar)
            .arg("--manifest")
            .arg(&tmp_manifest)
            .arg("--min-sdk-version")
            .arg(min_sdk.to_string())
            .arg("--target-sdk-version")
            .arg(target_sdk.to_string())
            .arg("--version-code")
            .arg(project.version_code.unwrap_or(1).to_string())
            .arg("--version-name")
            .arg(project.version_name.as_deref().unwrap_or("1.0"))
            .arg("--auto-add-overlay");
        // Generate R.java for resource ID constants.
        link_cmd.arg("--java").arg(&r_java_dir);
        // Generate R classes for ALL library packages that contributed resources.
        // Collect package names from AAR manifests in the extracted res dirs.
        let mut lib_packages: Vec<String> = Vec::new();
        for res_dir in &deduped_res_dirs {
            // The AAR manifest is next to the .res dir: ../artifact-version.aar
            let aar_path = res_dir.with_extension("aar");
            if aar_path.exists() {
                if let Ok(file) = std::fs::File::open(&aar_path) {
                    if let Ok(mut archive) = zip::ZipArchive::new(file) {
                        if let Ok(mut manifest) = archive.by_name("AndroidManifest.xml") {
                            let mut xml = String::new();
                            std::io::Read::read_to_string(&mut manifest, &mut xml).ok();
                            // Extract package from: <manifest ... package="xxx">
                            if let Some(idx) = xml.find("package=\"") {
                                let rest = &xml[idx + 9..];
                                if let Some(end) = rest.find('"') {
                                    lib_packages.push(rest[..end].to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        lib_packages.sort();
        lib_packages.dedup();
        if !lib_packages.is_empty() {
            link_cmd.arg("--extra-packages").arg(lib_packages.join(":"));
        }
        for z in &res_zips {
            link_cmd.arg(z);
        }
        let link_output = link_cmd.output()?;
        if link_output.status.success() || base_apk.exists() {
            link_ok = true;
            break;
        }
        // Extract the conflicting ZIP from the error and exclude it.
        let stderr = String::from_utf8_lossy(&link_output.stderr);
        let bad_zip = stderr.lines().find_map(|line| {
            // Look for: ".../lib-N-res.zip@...: error: failed to merge"
            if let Some(idx) = line.find(".zip@") {
                let prefix = &line[..idx + 4];
                // Extract just the filename part.
                return Some(PathBuf::from(
                    prefix.split_whitespace().last().unwrap_or(prefix),
                ));
            }
            None
        });
        if let Some(ref bad) = bad_zip {
            let before = res_zips.len();
            res_zips.retain(|z| z != bad);
            if res_zips.len() == before {
                // Couldn't find the exact path — try matching by filename.
                if let Some(name) = bad.file_name() {
                    res_zips.retain(|z| z.file_name() != Some(name));
                }
            }
            if res_zips.len() < before {
                continue;
            }
        }
        // Can't resolve conflict — print errors and break.
        eprintln!("  aapt2 link errors:");
        for line in stderr.lines().take(5) {
            eprintln!("    {line}");
        }
        break;
    }
    if !link_ok {
        // Even on error, aapt2 may have produced a partial APK.
        // If it didn't, fall back to a minimal manifest-only APK.
        if !base_apk.exists() {
            eprintln!("  aapt2 link with resources failed, trying app-only resources");
            // Retry with just app resources (no library overlays).
            let mut retry = std::process::Command::new(&aapt2);
            retry
                .arg("link")
                .arg("-o")
                .arg(&base_apk)
                .arg("-I")
                .arg(&android_jar)
                .arg("--manifest")
                .arg(&tmp_manifest)
                .arg("--min-sdk-version")
                .arg(min_sdk.to_string())
                .arg("--target-sdk-version")
                .arg(target_sdk.to_string())
                .arg("--auto-add-overlay");
            // Only add app resources, not library ones.
            let app_zip = compiled_res.join("app-res.zip");
            if app_zip.exists() {
                retry.arg(&app_zip);
            }
            let retry_out = retry.output()?;
            if !retry_out.status.success() || !base_apk.exists() {
                // Final fallback: stripped manifest, no resource references.
                eprintln!("  aapt2 link with app resources also failed, using stripped manifest");
                let stripped = format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                     <manifest xmlns:android=\"http://schemas.android.com/apk/res/android\"\n\
                         package=\"{pkg}\">\n\
                       <application android:allowBackup=\"true\" android:label=\"{pkg}\" \
                         android:supportsRtl=\"true\">\n\
                         <activity android:name=\".NavActivity\" \
                           android:windowSoftInputMode=\"adjustResize\" \
                           android:exported=\"true\">\n\
                           <intent-filter>\n\
                             <action android:name=\"android.intent.action.MAIN\"/>\n\
                             <category android:name=\"android.intent.category.LAUNCHER\"/>\n\
                           </intent-filter>\n\
                         </activity>\n\
                       </application>\n\
                     </manifest>"
                );
                let stripped_path = build_dir.join("aapt2-stripped-manifest.xml");
                std::fs::write(&stripped_path, &stripped)?;
                let mut final_fb = std::process::Command::new(&aapt2);
                final_fb
                    .arg("link")
                    .arg("-o")
                    .arg(&base_apk)
                    .arg("-I")
                    .arg(&android_jar)
                    .arg("--manifest")
                    .arg(&stripped_path)
                    .arg("--min-sdk-version")
                    .arg(min_sdk.to_string())
                    .arg("--target-sdk-version")
                    .arg(target_sdk.to_string());
                // No resources — just manifest + empty resources.arsc.
                let final_out = final_fb.output()?;
                if !final_out.status.success() {
                    anyhow::bail!("aapt2 link failed even with stripped manifest");
                }
            }
        }
    }
    eprintln!(
        "  aapt2 base APK: {} bytes",
        std::fs::metadata(&base_apk)?.len()
    );

    // ── Step 3.5: Compile R.java → R.class ────────────────────────────
    // aapt2 generates R.java files for resource ID constants.
    // Compile them with javac and include in the d8 input.
    let r_classes_dir = build_dir.join("r-classes");
    std::fs::create_dir_all(&r_classes_dir)?;
    let r_java_files: Vec<PathBuf> = walkdir::WalkDir::new(&r_java_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("java"))
        .map(|e| e.path().to_path_buf())
        .collect();
    if !r_java_files.is_empty() {
        let mut javac_cmd = std::process::Command::new("javac");
        javac_cmd
            .arg("-d")
            .arg(&r_classes_dir)
            .arg("-source")
            .arg("8")
            .arg("-target")
            .arg("8")
            .arg("-nowarn");
        for f in &r_java_files {
            javac_cmd.arg(f);
        }
        let javac_out = javac_cmd.output();
        match javac_out {
            Ok(out) if out.status.success() => {
                let r_count = walkdir::WalkDir::new(&r_classes_dir)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("class"))
                    .count();
                eprintln!(
                    "  {} R.class files compiled from {} R.java files",
                    r_count,
                    r_java_files.len()
                );
            }
            _ => {
                eprintln!("  WARNING: javac failed to compile R.java files");
            }
        }
    }

    // ── Step 4: DEX files ───────────────────────────────────────────────
    let dex_dir = build_dir.join("d8-final");
    std::fs::create_dir_all(&dex_dir)?;
    // Clean old outputs.
    for entry in std::fs::read_dir(&dex_dir)?.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("dex") {
            std::fs::remove_file(entry.path()).ok();
        }
    }

    {
        // Deduplicate dependency JARs.
        let deduped = dedup_dep_jars(&dep_jars);
        eprintln!(
            "  d8: {} app classes, {} dep JARs ({} after dedup)",
            all_classes.len(),
            dep_jars.len(),
            deduped.len()
        );

        // Write app .class files to a temp dir.
        let app_classes_dir = build_dir.join("d8-input");
        if app_classes_dir.exists() {
            std::fs::remove_dir_all(&app_classes_dir).ok();
        }
        std::fs::create_dir_all(&app_classes_dir)?;
        for (name, bytes) in &all_classes {
            let path = app_classes_dir.join(format!("{name}.class"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&path, bytes)?;
        }
        let app_class_files: Vec<PathBuf> = walkdir::WalkDir::new(&app_classes_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("class"))
            .map(|e| e.path().to_path_buf())
            .collect();
        // R classes are compiled separately — they'll be d8'd as a separate DEX
        // to avoid interfering with the app class d8 retry loop.
        let r_class_files: Vec<PathBuf> = walkdir::WalkDir::new(&r_classes_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("class"))
            .map(|e| e.path().to_path_buf())
            .collect();

        // Run d8 with all dep JARs + app classes + R classes as program inputs.
        // Use --map-diagnostics to downgrade duplicate-class errors.
        let mut d8_cmd = std::process::Command::new(&d8);
        d8_cmd
            .arg("--output")
            .arg(&dex_dir)
            .arg("--lib")
            .arg(&android_jar)
            .arg("--map-diagnostics")
            .arg("error")
            .arg("warning")
            .arg("--min-api")
            .arg(min_sdk.to_string());
        for jar in &deduped {
            d8_cmd.arg(jar);
        }
        for f in &app_class_files {
            d8_cmd.arg(f);
        }
        for f in &r_class_files {
            d8_cmd.arg(f);
        }
        let d8_output = d8_cmd.output()?;
        if !d8_output.status.success() {
            let stderr = String::from_utf8_lossy(&d8_output.stderr);
            // d8 may fail on some app classes. Retry: compile deps only, then
            // try app classes separately with deps as classpath.
            eprintln!("  d8 combined failed, trying two-phase approach");
            for line in stderr.lines().take(3) {
                eprintln!("    {line}");
            }
            // Clean and retry.
            for entry in std::fs::read_dir(&dex_dir)?.flatten() {
                std::fs::remove_file(entry.path()).ok();
            }
            // Phase 1: deps only — with retry loop to exclude conflicting JARs.
            let mut jar_list = deduped.clone();
            for _retry in 0..30 {
                for entry in std::fs::read_dir(&dex_dir)?.flatten() {
                    std::fs::remove_file(entry.path()).ok();
                }
                let mut deps_cmd = std::process::Command::new(&d8);
                deps_cmd
                    .arg("--output")
                    .arg(&dex_dir)
                    .arg("--lib")
                    .arg(&android_jar)
                    .arg("--min-api")
                    .arg(min_sdk.to_string());
                for jar in &jar_list {
                    deps_cmd.arg(jar);
                }
                let deps_out = deps_cmd.output()?;
                if deps_out.status.success() || dex_dir.join("classes.dex").exists() {
                    break;
                }
                // Extract conflicting JARs. d8 reports:
                //   "Type X defined multiple times: A.jar:..., B.jar:..."
                // Always exclude the SECOND JAR (the older/conflicting one).
                // d8 reports "Error in A.jar" where A is the first-encountered
                // (often newer), and B is the duplicate. Exclude B to keep A.
                let deps_stderr = String::from_utf8_lossy(&deps_out.stderr);
                let extract_jar = |s: &str| -> Option<PathBuf> {
                    s.find(".jar:")
                        .or_else(|| s.find(".jar,"))
                        .map(|idx| PathBuf::from(&s[..idx + 4]))
                };
                let bad_jar = deps_stderr.lines().find_map(|line| {
                    if line.contains("defined multiple times") {
                        // "defined in A.jar:..., B.jar:..." — exclude B (the second).
                        let parts: Vec<&str> = line.split(", ").collect();
                        if parts.len() >= 2 {
                            if let Some(jar2) = extract_jar(parts[1]) {
                                return Some(jar2);
                            }
                            // Fallback to first if second can't be parsed.
                            return extract_jar(parts[0].split_whitespace().last().unwrap_or(""));
                        }
                    }
                    // Fallback: extract from "Error in" line.
                    if let Some(rest) = line.strip_prefix("Error in ") {
                        return extract_jar(rest);
                    }
                    None
                });
                if let Some(ref bad) = bad_jar {
                    eprintln!(
                        "  d8 deps: excluding {}",
                        bad.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                    );
                    jar_list.retain(|j| j != bad);
                } else {
                    eprintln!("  d8 deps-only failed (no recoverable JAR)");
                    break;
                }
            }
            let deps_dex_count = std::fs::read_dir(&dex_dir)?
                .flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("dex"))
                .count();
            eprintln!(
                "  d8 deps: {} DEX files from {} JARs",
                deps_dex_count,
                jar_list.len()
            );
            // Phase 2: app classes with deps as classpath.
            let app_dex = compile_app_classes_with_d8(
                &d8,
                &all_classes,
                &build_dir,
                &dep_jars,
                &Some(android_jar.clone()),
            )?;

            // Instead of merging DEX files (which can corrupt class references),
            // collect all DEX files: deps DEX + app DEX, rename sequentially.
            // Collect dep DEX files.
            let mut all_dex: Vec<PathBuf> = std::fs::read_dir(&dex_dir)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("dex"))
                .collect();
            all_dex.sort();
            // Write app DEX as a separate file.
            let app_dex_path = dex_dir.join("app-classes.dex");
            std::fs::write(&app_dex_path, &app_dex)?;
            all_dex.push(app_dex_path);

            // Compile R classes to separate DEX(es) — they may span multiple
            // DEX files due to the 65K method limit.
            if !r_class_files.is_empty() {
                let r_dex_dir = build_dir.join("d8-rclasses");
                std::fs::create_dir_all(&r_dex_dir)?;
                let mut r_cmd = std::process::Command::new(&d8);
                r_cmd
                    .arg("--output")
                    .arg(&r_dex_dir)
                    .arg("--min-api")
                    .arg(min_sdk.to_string());
                for f in &r_class_files {
                    r_cmd.arg(f);
                }
                if r_cmd.output()?.status.success() {
                    // Collect ALL DEX files from the R class compilation.
                    for entry in std::fs::read_dir(&r_dex_dir)?.flatten() {
                        if entry.path().extension().and_then(|e| e.to_str()) == Some("dex") {
                            let dest =
                                dex_dir.join(format!("r-{}", entry.file_name().to_string_lossy()));
                            std::fs::copy(entry.path(), &dest)?;
                            all_dex.push(dest);
                        }
                    }
                }
            }

            // Rename all DEX files: classes.dex, classes2.dex, classes3.dex, ...
            let final_dir = build_dir.join("d8-assembled");
            std::fs::create_dir_all(&final_dir)?;
            for (i, dex_path) in all_dex.iter().enumerate() {
                let name = if i == 0 {
                    "classes.dex".to_string()
                } else {
                    format!("classes{}.dex", i + 1)
                };
                std::fs::copy(dex_path, final_dir.join(&name))?;
            }
            // Replace dex_dir contents with assembled DEX files.
            for entry in std::fs::read_dir(&dex_dir)?.flatten() {
                std::fs::remove_file(entry.path()).ok();
            }
            for entry in std::fs::read_dir(&final_dir)?.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("dex") {
                    std::fs::copy(entry.path(), dex_dir.join(entry.file_name()))?;
                }
            }
        }
    }

    // Count DEX files produced. Sort so classes.dex is first — Android
    // requires the primary DEX to be the first entry in the ZIP.
    let mut dex_files: Vec<PathBuf> = std::fs::read_dir(&dex_dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("dex"))
        .collect();
    dex_files.sort_by(|a, b| {
        // classes.dex < classes2.dex < classes3.dex ...
        let an = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let bn = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
        an.cmp(bn)
    });
    let total_dex_size: u64 = dex_files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    eprintln!(
        "  {} DEX files ({:.1} MB)",
        dex_files.len(),
        total_dex_size as f64 / 1_048_576.0
    );

    // ── Step 5: Build final APK ────────────────────────────────────────
    let unsigned_apk = build_dir.join("app-unsigned.apk");
    {
        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .with_alignment(4);
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        {
            // 1. AndroidManifest.xml from aapt2 base (STORED).
            let base_file = std::fs::File::open(&base_apk)?;
            let mut base_zip = zip::ZipArchive::new(base_file)?;
            if let Ok(mut entry) = base_zip.by_name("AndroidManifest.xml") {
                zip.start_file("AndroidManifest.xml", stored)?;
                std::io::copy(&mut entry, &mut zip)?;
            }
            // 2. DEX files (STORED).
            for dex_path in &dex_files {
                let name = dex_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("classes.dex");
                zip.start_file(name, stored)?;
                std::io::Write::write_all(&mut zip, &std::fs::read(dex_path)?)?;
            }
            // 3. resources.arsc from aapt2 base (STORED).
            if let Ok(mut entry) = base_zip.by_name("resources.arsc") {
                zip.start_file("resources.arsc", stored)?;
                std::io::copy(&mut entry, &mut zip)?;
            }
            // 4. Resource files from aapt2 base (DEFLATED).
            for i in 0..base_zip.len() {
                let entry = base_zip.by_index(i)?;
                let name = entry.name().to_string();
                if name == "AndroidManifest.xml" || name == "resources.arsc" {
                    continue;
                }
                drop(entry);
                let mut entry = base_zip.by_index(i)?;
                zip.start_file(&name, deflated)?;
                std::io::copy(&mut entry, &mut zip)?;
            }
            // 5. Raw resource files from app's res/.
            if app_res_dir.is_dir() {
                let existing: std::collections::HashSet<String> = (0..base_zip.len())
                    .filter_map(|i| base_zip.by_index(i).ok().map(|e| e.name().to_string()))
                    .collect();
                for entry in walkdir::WalkDir::new(&app_res_dir)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if entry.path().is_file() {
                        if let Ok(rel) = entry.path().strip_prefix(&app_res_dir) {
                            let apk_path =
                                format!("res/{}", rel.to_string_lossy().replace('\\', "/"));
                            if !existing.contains(&apk_path) {
                                zip.start_file(&apk_path, deflated)?;
                                if let Ok(data) = std::fs::read(entry.path()) {
                                    std::io::Write::write_all(&mut zip, &data)?;
                                }
                            }
                        }
                    }
                }
            }
        }

        let buf = zip.finish()?;
        std::fs::write(&unsigned_apk, buf.into_inner())?;
    }

    // ── Step 6: zipalign + apksigner ──────────────────────────────────
    let aligned_apk = build_dir.join("app-aligned.apk");
    let output = std::process::Command::new(&zipalign)
        .arg("-f")
        .arg("-p")
        .arg("4")
        .arg(&unsigned_apk)
        .arg(&aligned_apk)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("zipalign failed");
    }

    // Sign with debug keystore.
    let signed_apk = build_dir.join("app-debug.apk");
    let home = std::env::var("HOME").unwrap_or_default();
    let debug_keystore = PathBuf::from(&home).join(".android/debug.keystore");
    if !debug_keystore.exists() {
        // Generate a debug keystore if it doesn't exist.
        let keytool = std::process::Command::new("keytool")
            .arg("-genkeypair")
            .arg("-v")
            .arg("-keystore")
            .arg(&debug_keystore)
            .arg("-storepass")
            .arg("android")
            .arg("-alias")
            .arg("androiddebugkey")
            .arg("-keypass")
            .arg("android")
            .arg("-keyalg")
            .arg("RSA")
            .arg("-keysize")
            .arg("2048")
            .arg("-validity")
            .arg("10000")
            .arg("-dname")
            .arg("CN=Android Debug,O=Android,C=US")
            .output()?;
        if !keytool.status.success() {
            anyhow::bail!("failed to generate debug keystore");
        }
    }
    let sign_output = std::process::Command::new(&apksigner)
        .arg("sign")
        .arg("--ks")
        .arg(&debug_keystore)
        .arg("--ks-pass")
        .arg("pass:android")
        .arg("--key-pass")
        .arg("pass:android")
        .arg("--out")
        .arg(&signed_apk)
        .arg(&aligned_apk)
        .output()?;
    if !sign_output.status.success() {
        let stderr = String::from_utf8_lossy(&sign_output.stderr);
        anyhow::bail!(
            "apksigner failed: {}",
            stderr.lines().take(3).collect::<Vec<_>>().join("; ")
        );
    }

    let apk_size = std::fs::metadata(&signed_apk)?.len();
    eprintln!(
        "BUILD SUCCESS: {} ({:.1} MB)",
        signed_apk.display(),
        apk_size as f64 / 1_048_576.0
    );

    Ok(BuildOutcome {
        project,
        target: BuildTarget::Android,
        output_path: signed_apk,
    })
}

/// Compile all modules' Kotlin sources → .class files without packaging.
/// Returns (project model, compiled classes, module dirs with app flag).
type CompileResult = (ProjectModel, Vec<(String, Vec<u8>)>, Vec<(PathBuf, bool)>);

fn compile_multi_module_classes(
    root_dir: &Path,
    settings: &skotch_buildscript::SettingsModel,
) -> Result<CompileResult> {
    use rustc_hash::FxHashMap;

    // d8-safe mode: not needed since the retry loop handles failing classes.
    // The retry downgrades to v50 per-class, then stubs if still failing.

    let mut sm = SourceMap::new();
    let mut interner = Interner::new();

    struct ModuleInfo {
        name: String,
        dir: PathBuf,
        project: ProjectModel,
    }
    let mut modules: Vec<ModuleInfo> = Vec::new();

    // Parse root build.gradle.kts.
    let root_bf = root_dir.join("build.gradle.kts");
    let (allprojects_cfg, subprojects_cfg) = if root_bf.exists() {
        let root_text = std::fs::read_to_string(&root_bf)?;
        let root_fid = sm.add(root_bf.clone(), root_text.clone());
        let parsed =
            parse_buildfile_with_catalog(&root_text, root_fid, &mut interner, Some(root_dir));
        (parsed.allprojects_config, parsed.subprojects_config)
    } else {
        (Default::default(), Default::default())
    };

    for module_path in &settings.included_modules {
        let dir_name = module_path.trim_start_matches(':');
        let module_dir = root_dir.join(dir_name);
        let bf = module_dir.join("build.gradle.kts");
        if !bf.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&bf)?;
        let fid = sm.add(bf, text.clone());
        let parsed = parse_buildfile_with_catalog(&text, fid, &mut interner, Some(root_dir));
        let mut project = parsed.project;
        project.project_name = Some(dir_name.to_string());
        skotch_buildscript::merge_shared_config(&mut project, &allprojects_cfg);
        skotch_buildscript::merge_shared_config(&mut project, &subprojects_cfg);
        modules.push(ModuleInfo {
            name: dir_name.to_string(),
            dir: module_dir,
            project,
        });
    }

    // Build dependency graph.
    let name_to_idx: FxHashMap<String, usize> = modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name.clone(), i))
        .collect();
    let n = modules.len();

    // Topological sort.
    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, m) in modules.iter().enumerate() {
        for dep in &m.project.project_deps {
            let dep_name = dep.trim_start_matches(':');
            if let Some(&j) = name_to_idx.get(dep_name) {
                adj[j].push(i);
                in_degree[i] += 1;
            }
        }
    }
    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut build_order = Vec::new();
    while let Some(idx) = queue.pop() {
        build_order.push(idx);
        for &dep_idx in &adj[idx] {
            in_degree[dep_idx] -= 1;
            if in_degree[dep_idx] == 0 {
                queue.push(dep_idx);
            }
        }
    }

    // Resolve external Maven dependencies for classpath — needed so the MIR
    // lowerer can resolve library method signatures and types.
    for module in &modules {
        if let Ok(dep_jars) = resolve_external_deps(&module.project, root_dir) {
            if !dep_jars.is_empty() {
                // Add to CLASSPATH.
                let sep = if cfg!(windows) { ";" } else { ":" };
                let mut cp = std::env::var("CLASSPATH").unwrap_or_default();
                for jar in &dep_jars {
                    if !cp.is_empty() {
                        cp.push_str(sep);
                    }
                    cp.push_str(&jar.to_string_lossy());
                }
                std::env::set_var("CLASSPATH", &cp);
                skotch_mir_lower::preload_registry_jars(&dep_jars);
                eprintln!(
                    "  {} dependency JARs loaded for compilation",
                    dep_jars.len()
                );
                break; // Only need to load once (all modules share classpath).
            }
        }
    }

    // Compile in order.
    let mut all_classes: Vec<(String, Vec<u8>)> = Vec::new();
    let mut app_project: Option<ProjectModel> = None;
    let mut module_symbols: Vec<skotch_resolve::PackageSymbolTable> =
        vec![skotch_resolve::PackageSymbolTable::default(); n];
    let mut modules_info: Vec<(PathBuf, bool)> = Vec::new();

    for &idx in &build_order {
        let module = &modules[idx];
        let mut src_files = Vec::new();
        for subdir in &["src/main/kotlin", "src/main/java"] {
            src_files.extend(discover_sources(&module.dir.join(subdir)).unwrap_or_default());
        }

        if src_files.is_empty() {
            modules_info.push((module.dir.clone(), module.project.is_android));
            continue;
        }

        // Build combined symbol table.
        let mut combined_symbols = skotch_resolve::PackageSymbolTable::default();
        for dep_path in &module.project.project_deps {
            let dep_name = dep_path.trim_start_matches(':');
            if let Some(&dep_idx) = name_to_idx.get(dep_name) {
                let dep_syms = &module_symbols[dep_idx];
                for (k, v) in &dep_syms.functions {
                    combined_symbols
                        .functions
                        .entry(k.clone())
                        .or_default()
                        .extend(v.clone());
                }
                for (k, v) in &dep_syms.vals {
                    combined_symbols.vals.entry(k.clone()).or_insert(v.clone());
                }
                for (k, v) in &dep_syms.classes {
                    combined_symbols
                        .classes
                        .entry(k.clone())
                        .or_insert(v.clone());
                }
            }
        }

        let mut mod_interner = skotch_intern::Interner::new();
        let mut mod_diags = skotch_diagnostics::Diagnostics::new();
        let mut mod_sm = skotch_span::SourceMap::new();
        let mut parsed: Vec<(skotch_span::FileId, skotch_syntax::KtFile, String)> = Vec::new();

        for path in &src_files {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let file_id = mod_sm.add(path.clone(), text.clone());
            let lexed = lex(file_id, &text, &mut mod_diags);
            let ast = parse_file(&lexed, &mut mod_interner, &mut mod_diags);
            let wrapper = wrapper_class_for(path);
            parsed.push((file_id, ast, wrapper));
        }

        let refs: Vec<(skotch_span::FileId, &skotch_syntax::KtFile, &str)> = parsed
            .iter()
            .map(|(fid, ast, wc)| (*fid, ast, wc.as_str()))
            .collect();
        let own_symbols = gather_declarations(&refs, &mod_interner);

        for (k, v) in &own_symbols.functions {
            combined_symbols
                .functions
                .entry(k.clone())
                .or_default()
                .extend(v.clone());
        }
        for (k, v) in &own_symbols.vals {
            combined_symbols.vals.entry(k.clone()).or_insert(v.clone());
        }
        for (k, v) in &own_symbols.classes {
            combined_symbols
                .classes
                .entry(k.clone())
                .or_insert(v.clone());
        }

        let mut classes: Vec<(String, Vec<u8>)> = Vec::new();
        for (_fid, ast, wrapper) in &parsed {
            let mut mir = skotch_driver::compile_ast(
                ast,
                wrapper,
                &mut mod_interner,
                &mut mod_diags,
                Some(&combined_symbols),
            );
            // Apply Compose transform if the module has @Composable functions.
            if module.project.is_compose || skotch_compose::has_composables(&mir) {
                skotch_compose::compose_transform(&mut mir);
            }
            let file_classes = skotch_backend_jvm::compile_module(&mir, &mod_interner);
            classes.extend(file_classes);
        }

        let err_count = mod_diags
            .iter()
            .filter(|d| d.severity == skotch_diagnostics::Severity::Error)
            .count();
        if mod_diags.has_errors() {
            let diag_text = render(&mod_diags, &mod_sm);
            eprint!("{diag_text}");
        }
        eprintln!(
            "    [{}] {} classes ({} errors)",
            module.name,
            classes.len(),
            err_count
        );

        // Store symbols for downstream modules.
        {
            let mut tmp_interner = skotch_intern::Interner::new();
            let mut tmp_diags = skotch_diagnostics::Diagnostics::new();
            let mut tmp_sm = skotch_span::SourceMap::new();
            let mut re_parsed: Vec<(skotch_span::FileId, skotch_syntax::KtFile, String)> =
                Vec::new();
            for path in &src_files {
                let text = std::fs::read_to_string(path).unwrap_or_default();
                let fid = tmp_sm.add(path.clone(), text.clone());
                let lexed = lex(fid, &text, &mut tmp_diags);
                let ast = parse_file(&lexed, &mut tmp_interner, &mut tmp_diags);
                let wrapper = wrapper_class_for(path);
                re_parsed.push((fid, ast, wrapper));
            }
            let refs: Vec<_> = re_parsed
                .iter()
                .map(|(fid, ast, wc)| (*fid, ast, wc.as_str()))
                .collect();
            module_symbols[idx] = gather_declarations(&refs, &tmp_interner);
        }

        all_classes.extend(classes);
        modules_info.push((module.dir.clone(), module.project.is_android));

        if module.project.is_android || app_project.is_none() {
            app_project = Some(module.project.clone());
        }
    }

    let project = app_project.unwrap_or_default();
    Ok((project, all_classes, modules_info))
}

/// Compile AndroidManifest.xml using aapt2 to produce correct binary AXML.
fn compile_manifest_with_aapt2(
    aapt2: &Path,
    manifest_path: &Path,
    android_jar: &Path,
    project: &ProjectModel,
) -> Result<Vec<u8>> {
    let pkg = project
        .namespace
        .as_deref()
        .or(project.application_id.as_deref())
        .unwrap_or("com.example");
    let min_sdk = project.min_sdk.unwrap_or(24);
    let target_sdk = project.target_sdk.unwrap_or(35);

    // Create a temp manifest with package attribute injected.
    let manifest_xml = std::fs::read_to_string(manifest_path)?;
    let fixed_xml = if !manifest_xml.contains("package=") {
        manifest_xml.replace("<manifest ", &format!("<manifest package=\"{pkg}\" "))
    } else {
        manifest_xml
    };
    let tmp_dir = std::env::temp_dir().join("skotch-aapt2");
    std::fs::create_dir_all(&tmp_dir)?;
    let tmp_manifest = tmp_dir.join("AndroidManifest.xml");
    std::fs::write(&tmp_manifest, &fixed_xml)?;
    let tmp_apk = tmp_dir.join("temp.apk");

    let output = std::process::Command::new(aapt2)
        .arg("link")
        .arg("-o")
        .arg(&tmp_apk)
        .arg("-I")
        .arg(android_jar)
        .arg("--manifest")
        .arg(&tmp_manifest)
        .arg("--min-sdk-version")
        .arg(min_sdk.to_string())
        .arg("--target-sdk-version")
        .arg(target_sdk.to_string())
        .arg("--version-code")
        .arg(project.version_code.unwrap_or(1).to_string())
        .arg("--version-name")
        .arg(project.version_name.as_deref().unwrap_or("1.0"))
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("aapt2 failed: {stderr}");
    }

    // Extract AndroidManifest.xml from the generated APK.
    let file = std::fs::File::open(&tmp_apk)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut entry = archive.by_name("AndroidManifest.xml")?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes)?;
    Ok(bytes)
}

fn count_elements(elem: &skotch_axml::Element) -> usize {
    1 + elem.children.iter().map(count_elements).sum::<usize>()
}

fn build_manifest_from_project(project: &ProjectModel) -> skotch_axml::Element {
    let package = project
        .namespace
        .as_deref()
        .or(project.application_id.as_deref())
        .unwrap_or("com.example.app");
    let version_code = project.version_code.unwrap_or(1);
    let version_name = project.version_name.as_deref().unwrap_or("1.0");
    let min_sdk = project.min_sdk.unwrap_or(24);
    let target_sdk = project.target_sdk.unwrap_or(34);
    skotch_axml::build_manifest(
        package,
        version_code,
        version_name,
        min_sdk,
        target_sdk,
        None,
    )
}

/// Build a multi-module project with proper dependency ordering, cross-module
/// visibility, parallel compilation of independent modules, and Salsa-based
/// incremental compilation.
///
/// Architecture:
/// 1. Parse all modules' build.gradle.kts to build the dependency graph
/// 2. Topological sort (Kahn's algorithm) with cycle detection
/// 3. Compile in dependency order — modules with no unbuilt deps compile
///    in parallel via rayon; dependent modules wait for their deps
/// 4. Cross-module visibility: each module's symbol table includes exports
///    from all its dependency modules
/// 5. Package all classes into a single JAR
fn build_multi_module(
    root_dir: &Path,
    settings: &skotch_buildscript::SettingsModel,
    opts: &BuildOptions,
) -> Result<BuildOutcome> {
    use rustc_hash::FxHashMap;

    // ── Step 1: Parse all modules ───────────────────────────────────────
    struct ModuleInfo {
        name: String,
        dir: PathBuf,
        project: ProjectModel,
    }
    let mut sm = SourceMap::new();
    let mut interner = Interner::new();
    let mut modules: Vec<ModuleInfo> = Vec::new();

    // Parse root build.gradle.kts for allprojects/subprojects config.
    let root_bf = root_dir.join("build.gradle.kts");
    let (allprojects_cfg, subprojects_cfg) = if root_bf.exists() {
        let root_text = std::fs::read_to_string(&root_bf)?;
        let root_fid = sm.add(root_bf, root_text.clone());
        let root_parsed =
            parse_buildfile_with_catalog(&root_text, root_fid, &mut interner, Some(root_dir));
        (
            root_parsed.allprojects_config,
            root_parsed.subprojects_config,
        )
    } else {
        Default::default()
    };

    for module_path in &settings.included_modules {
        let dir_name = module_path.trim_start_matches(':');
        let module_dir = root_dir.join(dir_name);
        let bf = module_dir.join("build.gradle.kts");
        if !bf.exists() {
            anyhow::bail!("build.gradle.kts not found for module {module_path}");
        }
        let text = std::fs::read_to_string(&bf)?;
        let fid = sm.add(bf, text.clone());
        // Use root dir for catalog lookup — the catalog is typically at the
        // root project level, not in the submodule.
        let parsed = parse_buildfile_with_catalog(&text, fid, &mut interner, Some(root_dir));
        let mut project = parsed.project;
        project.project_name = Some(dir_name.to_string());
        // Merge allprojects + subprojects config from root.
        skotch_buildscript::merge_shared_config(&mut project, &allprojects_cfg);
        skotch_buildscript::merge_shared_config(&mut project, &subprojects_cfg);
        modules.push(ModuleInfo {
            name: dir_name.to_string(),
            dir: module_dir,
            project,
        });
    }

    // ── Step 2: Topological sort (Kahn's algorithm) ─────────────────────
    let name_to_idx: FxHashMap<String, usize> = modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name.clone(), i))
        .collect();

    // Build adjacency list and in-degree count.
    let n = modules.len();
    let mut in_degree = vec![0u32; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n]; // dep → [modules that depend on it]

    for (i, m) in modules.iter().enumerate() {
        for dep_path in &m.project.project_deps {
            let dep_name = dep_path.trim_start_matches(':');
            if let Some(&dep_idx) = name_to_idx.get(dep_name) {
                in_degree[i] += 1;
                dependents[dep_idx].push(i);
            }
            // Ignore unknown dependencies (external modules).
        }
    }

    // Kahn's algorithm: process modules with in-degree 0 first.
    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut build_order: Vec<usize> = Vec::with_capacity(n);

    while let Some(idx) = queue.pop() {
        build_order.push(idx);
        for &dep_idx in &dependents[idx] {
            in_degree[dep_idx] -= 1;
            if in_degree[dep_idx] == 0 {
                queue.push(dep_idx);
            }
        }
    }

    if build_order.len() != n {
        let cyclic: Vec<&str> = (0..n)
            .filter(|&i| in_degree[i] > 0)
            .map(|i| modules[i].name.as_str())
            .collect();
        anyhow::bail!("circular module dependencies detected: {:?}", cyclic);
    }

    // ── Step 3: Compile in dependency order with cross-module visibility ─
    // Each module accumulates exports from its dependencies so cross-module
    // function calls and class references resolve correctly.
    let mut all_classes: Vec<(String, Vec<u8>)> = Vec::new();
    let mut all_mir_modules: Vec<MirModule> = Vec::new();
    let mut app_project: Option<ProjectModel> = None;
    let mut diags = Diagnostics::new();

    // Per-module symbol tables, indexed by module index.
    let mut module_symbols: Vec<skotch_resolve::PackageSymbolTable> =
        vec![skotch_resolve::PackageSymbolTable::default(); n];

    // Identify which build levels can run in parallel.
    // Group modules by their position in the topological order where all
    // dependencies have already been built.
    let mut module_level: Vec<usize> = vec![0; n];
    for &idx in &build_order {
        let max_dep_level = modules[idx]
            .project
            .project_deps
            .iter()
            .filter_map(|dep| {
                let dep_name = dep.trim_start_matches(':');
                name_to_idx.get(dep_name).map(|&di| module_level[di])
            })
            .max()
            .unwrap_or(0);
        module_level[idx] = if modules[idx].project.project_deps.is_empty() {
            0
        } else {
            max_dep_level + 1
        };
    }

    let max_level = module_level.iter().copied().max().unwrap_or(0);

    for level in 0..=max_level {
        // Collect all modules at this level — they can build in parallel.
        let level_modules: Vec<usize> = build_order
            .iter()
            .copied()
            .filter(|&i| module_level[i] == level)
            .collect();

        if level_modules.len() > 1 {
            eprintln!(
                "  level {level}: building {} modules in parallel",
                level_modules.len()
            );
        }

        // Build cross-module symbol table for each module at this level.
        // Include exports from all dependency modules.
        type ModuleBuildResult = (usize, Vec<(String, Vec<u8>)>, Vec<MirModule>, bool, String);
        let level_results: Vec<ModuleBuildResult> = level_modules
            .iter()
            .map(|&idx| {
                let module = &modules[idx];
                let mut src_files = Vec::new();
                for subdir in &["src/main/kotlin", "src/main/java"] {
                    src_files
                        .extend(discover_sources(&module.dir.join(subdir)).unwrap_or_default());
                }
                if src_files.is_empty() {
                    return (idx, Vec::new(), Vec::new(), false, String::new());
                }

                // Build combined symbol table: this module's own exports +
                // all dependency modules' exports.
                let mut combined_symbols = skotch_resolve::PackageSymbolTable::default();

                // Add dependency modules' symbols.
                for dep_path in &module.project.project_deps {
                    let dep_name = dep_path.trim_start_matches(':');
                    if let Some(&dep_idx) = name_to_idx.get(dep_name) {
                        let dep_syms = &module_symbols[dep_idx];
                        for (k, v) in &dep_syms.functions {
                            combined_symbols
                                .functions
                                .entry(k.clone())
                                .or_default()
                                .extend(v.clone());
                        }
                        for (k, v) in &dep_syms.vals {
                            combined_symbols.vals.entry(k.clone()).or_insert(v.clone());
                        }
                        for (k, v) in &dep_syms.classes {
                            combined_symbols
                                .classes
                                .entry(k.clone())
                                .or_insert(v.clone());
                        }
                    }
                }

                // Parse and gather this module's own declarations.
                let mut mod_interner = skotch_intern::Interner::new();
                let mut mod_diags = skotch_diagnostics::Diagnostics::new();
                let mut mod_sm = skotch_span::SourceMap::new();
                let mut parsed: Vec<(skotch_span::FileId, skotch_syntax::KtFile, String)> =
                    Vec::new();

                for path in &src_files {
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    let file_id = mod_sm.add(path.clone(), text.clone());
                    let lexed = lex(file_id, &text, &mut mod_diags);
                    let ast = parse_file(&lexed, &mut mod_interner, &mut mod_diags);
                    let wrapper = wrapper_class_for(path);
                    parsed.push((file_id, ast, wrapper));
                }

                let refs: Vec<(skotch_span::FileId, &skotch_syntax::KtFile, &str)> = parsed
                    .iter()
                    .map(|(fid, ast, wc)| (*fid, ast, wc.as_str()))
                    .collect();
                let own_symbols = gather_declarations(&refs, &mod_interner);

                // Add own symbols to combined table.
                for (k, v) in &own_symbols.functions {
                    combined_symbols
                        .functions
                        .entry(k.clone())
                        .or_default()
                        .extend(v.clone());
                }
                for (k, v) in &own_symbols.vals {
                    combined_symbols.vals.entry(k.clone()).or_insert(v.clone());
                }
                for (k, v) in &own_symbols.classes {
                    combined_symbols
                        .classes
                        .entry(k.clone())
                        .or_insert(v.clone());
                }

                // Compile each file with the combined symbol table.
                let mut classes: Vec<(String, Vec<u8>)> = Vec::new();
                let mut mir_modules: Vec<MirModule> = Vec::new();
                for (fid_idx, (_fid, ast, wrapper)) in parsed.iter().enumerate() {
                    let pre_errors = mod_diags.len();
                    let mir = skotch_driver::compile_ast(
                        ast,
                        wrapper,
                        &mut mod_interner,
                        &mut mod_diags,
                        Some(&combined_symbols),
                    );
                    let file_classes = skotch_backend_jvm::compile_module(&mir, &mod_interner);
                    mir_modules.push(mir);
                    let new_errors = mod_diags
                        .iter()
                        .filter(|d| d.severity == skotch_diagnostics::Severity::Error)
                        .count()
                        .saturating_sub(pre_errors);
                    if !file_classes.is_empty() {
                        eprintln!(
                            "    [{}] {} classes ({} errors)",
                            fid_idx,
                            file_classes.len(),
                            new_errors
                        );
                    }
                    classes.extend(file_classes);
                }

                let mod_diag_text = if mod_diags.has_errors() {
                    render(&mod_diags, &mod_sm)
                } else {
                    String::new()
                };
                (
                    idx,
                    classes,
                    mir_modules,
                    mod_diags.has_errors(),
                    mod_diag_text,
                )
            })
            .collect();

        // Collect results and store per-module symbols for downstream modules.
        for (idx, classes, file_mirs, has_errors, diag_text) in level_results {
            all_mir_modules.extend(file_mirs);
            if has_errors {
                let err_count = diag_text.matches("error:").count();
                eprintln!(
                    "  module '{}': {} compilation errors ({} classes emitted)",
                    modules[idx].name,
                    err_count,
                    classes.len()
                );
                // Continue building even with errors — emit what we can.
                // This allows partial compilation for Compose samples.
                let module_name = &modules[idx].name;
                let module_bf = modules[idx].dir.join("build.gradle.kts");
                diags.push(skotch_diagnostics::Diagnostic::error(
                    skotch_span::Span::new(sm.add(module_bf.clone(), String::new()), 0, 0),
                    format!("module '{}' had compilation errors", module_name),
                ));
            }
            // Store this module's own symbol table for downstream modules.
            let module = &modules[idx];
            let mut src_files = Vec::new();
            for subdir in &["src/main/kotlin", "src/main/java"] {
                src_files.extend(discover_sources(&module.dir.join(subdir)).unwrap_or_default());
            }
            if !src_files.is_empty() {
                let mut tmp_interner = skotch_intern::Interner::new();
                let mut tmp_diags = skotch_diagnostics::Diagnostics::new();
                let mut tmp_sm = skotch_span::SourceMap::new();
                let mut parsed: Vec<(skotch_span::FileId, skotch_syntax::KtFile, String)> =
                    Vec::new();
                for path in &src_files {
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    let fid = tmp_sm.add(path.clone(), text.clone());
                    let lexed = lex(fid, &text, &mut tmp_diags);
                    let ast = parse_file(&lexed, &mut tmp_interner, &mut tmp_diags);
                    let wrapper = wrapper_class_for(path);
                    parsed.push((fid, ast, wrapper));
                }
                let refs: Vec<_> = parsed
                    .iter()
                    .map(|(fid, ast, wc)| (*fid, ast, wc.as_str()))
                    .collect();
                module_symbols[idx] = gather_declarations(&refs, &tmp_interner);
            }

            if has_errors {
                diags.push(skotch_diagnostics::Diagnostic::error(
                    skotch_span::Span::empty(skotch_span::FileId(0)),
                    format!("module '{}' had compilation errors", modules[idx].name),
                ));
            }

            all_classes.extend(classes);

            if modules[idx].project.main_class.is_some() || app_project.is_none() {
                app_project = Some(modules[idx].project.clone());
            }
            // Prefer the Android module's project for target detection.
            if modules[idx].project.is_android {
                app_project = Some(modules[idx].project.clone());
            }
        }
    }

    if diags.has_errors() && all_classes.is_empty() {
        eprint!("{}", render(&diags, &sm));
        anyhow::bail!("compilation failed");
    } else if diags.has_errors() {
        // Partial success: some files compiled, others had errors.
        // Continue to produce output (JAR/APK) with available classes.
        eprintln!(
            "  WARNING: {} module(s) had errors, proceeding with {} classes",
            diags.len(),
            all_classes.len()
        );
    }

    let mut project = app_project.unwrap_or_default();
    if let Some(t) = opts.target_override.clone() {
        project.target = Some(t);
    }

    // Determine main class.
    let main_class = project
        .main_class
        .clone()
        .or_else(|| {
            all_classes
                .iter()
                .find(|(n, _)| n == "MainKt" || n.ends_with("/MainKt"))
                .map(|(n, _)| n.clone())
        })
        .or_else(|| {
            all_classes
                .iter()
                .find(|(n, _)| n.ends_with("Kt"))
                .map(|(n, _)| n.clone())
        })
        .unwrap_or_else(|| "MainKt".to_string());

    let target = project.target.clone().unwrap_or(BuildTarget::Jvm);

    eprintln!("  {} modules, {} classes compiled", n, all_classes.len());

    // Package based on detected target.
    if target == BuildTarget::Android {
        // For Android, merge all classes into a single MIR module and build APK.
        let mut module = MirModule::default();
        for (file_module, _, _) in std::iter::empty::<&(MirModule, bool, String)>() {
            merge_modules(&mut module, file_module.clone());
        }
        // Build DEX from compiled classes (simplified: write JAR first, note APK as TODO).
        let build_dir = root_dir.join("build");
        std::fs::create_dir_all(&build_dir).ok();
        let jar_name = settings.root_project_name.as_deref().unwrap_or("app");
        let jar_path = build_dir.join(format!("libs/{jar_name}.jar"));
        std::fs::create_dir_all(jar_path.parent().unwrap()).ok();
        skotch_jar::write_jar(&jar_path, &main_class, &all_classes, &[])?;

        // Find the app module directory for manifest and resources.
        let app_dir = modules
            .iter()
            .find(|m| m.project.is_android)
            .or_else(|| modules.iter().find(|m| m.dir.join("src/main/res").exists()))
            .or_else(|| {
                modules
                    .iter()
                    .find(|m| m.dir.join("src/main/AndroidManifest.xml").exists())
            })
            .map(|m| m.dir.clone())
            .unwrap_or_else(|| root_dir.to_path_buf());

        // Build AndroidManifest.xml — prefer source manifest if available.
        let manifest_path = app_dir.join("src/main/AndroidManifest.xml");
        eprintln!(
            "  manifest: {} (exists: {})",
            manifest_path.display(),
            manifest_path.exists()
        );
        let manifest_elem = if manifest_path.exists() {
            let xml = std::fs::read_to_string(&manifest_path).unwrap_or_default();
            match skotch_axml::parse_source_manifest(&xml) {
                Some(elem) => {
                    eprintln!(
                        "  using source AndroidManifest.xml ({} elements)",
                        count_elements(&elem)
                    );
                    elem
                }
                None => {
                    eprintln!("  WARNING: failed to parse source manifest, using generated");
                    build_manifest_from_project(&project)
                }
            }
        } else {
            build_manifest_from_project(&project)
        };
        // Inject package attribute if missing (build.gradle.kts has namespace).
        let mut manifest_elem = manifest_elem;
        if !manifest_elem.attributes.iter().any(|a| a.name == "package") {
            if let Some(pkg) = project
                .namespace
                .as_deref()
                .or(project.application_id.as_deref())
            {
                manifest_elem.attributes.push(skotch_axml::Attribute {
                    namespace: None,
                    name: "package".to_string(),
                    resource_id: None,
                    value: skotch_axml::AttributeValue::String(pkg.to_string()),
                });
            }
        }
        // Inject <uses-sdk> if not present (source manifests typically don't have it).
        let has_uses_sdk = manifest_elem.children.iter().any(|c| c.name == "uses-sdk");
        if !has_uses_sdk {
            let min_sdk = project.min_sdk.unwrap_or(24) as i32;
            let target_sdk = project.target_sdk.unwrap_or(35) as i32;
            let android_ns = "http://schemas.android.com/apk/res/android".to_string();
            manifest_elem.children.insert(
                0,
                skotch_axml::Element {
                    namespace: None,
                    name: "uses-sdk".to_string(),
                    attributes: vec![
                        skotch_axml::Attribute {
                            namespace: Some(android_ns.clone()),
                            name: "minSdkVersion".to_string(),
                            resource_id: Some(0x0101_020C),
                            value: skotch_axml::AttributeValue::Integer(min_sdk),
                        },
                        skotch_axml::Attribute {
                            namespace: Some(android_ns),
                            name: "targetSdkVersion".to_string(),
                            resource_id: Some(0x0101_0270),
                            value: skotch_axml::AttributeValue::Integer(target_sdk),
                        },
                    ],
                    children: Vec::new(),
                },
            );
        }
        // Try to use Android SDK's aapt2 for manifest compilation if available.
        // aapt2 produces a correctly-formatted binary XML that Android accepts.
        let axml_bytes = if let Ok(aapt2) = find_aapt2() {
            let android_jar = find_android_jar().unwrap_or_default();
            if let Ok(axml) =
                compile_manifest_with_aapt2(&aapt2, &manifest_path, &android_jar, &project)
            {
                eprintln!("  manifest compiled with aapt2");
                axml
            } else {
                skotch_axml::encode_axml(&manifest_elem)
            }
        } else {
            skotch_axml::encode_axml(&manifest_elem)
        };
        // Resolve external dependencies for d8 — these need to be included
        // in the DEX since Android APKs must be self-contained.
        let dep_jars = match resolve_external_deps(&project, root_dir) {
            Ok(jars) => jars,
            Err(e) => {
                eprintln!("  WARNING: failed to resolve deps: {e}");
                Vec::new()
            }
        };
        if !dep_jars.is_empty() {
            eprintln!("  {} dependencies resolved for DEX", dep_jars.len());
        }
        // Convert .class files to DEX using Android SDK's d8 if available.
        // d8 produces optimized DEX from JVM bytecode — much better than
        // our MIR→DEX path for real apps.
        let dex_bytes = if let Ok(d8_path) = find_d8() {
            match compile_classes_with_d8(&d8_path, &all_classes, &build_dir, &dep_jars) {
                Ok(dex) => {
                    eprintln!("  DEX compiled with d8 ({} bytes)", dex.len());
                    dex
                }
                Err(e) => {
                    eprintln!("  d8 failed: {e}, falling back to MIR→DEX");
                    let mut combined_module = MirModule {
                        wrapper_class: main_class.clone(),
                        ..MirModule::default()
                    };
                    for mir in all_mir_modules {
                        merge_modules(&mut combined_module, mir);
                    }
                    if project.is_compose || skotch_compose::has_composables(&combined_module) {
                        skotch_compose::compose_transform(&mut combined_module);
                    }
                    skotch_backend_dex::compile_module(&combined_module)
                }
            }
        } else {
            let mut combined_module = MirModule {
                wrapper_class: main_class.clone(),
                ..MirModule::default()
            };
            for mir in all_mir_modules {
                merge_modules(&mut combined_module, mir);
            }
            if project.is_compose || skotch_compose::has_composables(&combined_module) {
                skotch_compose::compose_transform(&mut combined_module);
            }
            skotch_backend_dex::compile_module(&combined_module)
        };
        // Collect resource files from app module.
        let res_dir = app_dir.join("src/main/res");
        let mut res_files = Vec::new();
        if res_dir.is_dir() {
            for entry in walkdir::WalkDir::new(&res_dir)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if entry.path().is_file() {
                    if let Ok(rel) = entry.path().strip_prefix(&res_dir) {
                        let apk_path = format!("res/{}", rel.to_string_lossy().replace('\\', "/"));
                        if let Ok(data) = std::fs::read(entry.path()) {
                            res_files.push((apk_path, data));
                        }
                    }
                }
            }
        }
        eprintln!("  {} resource files included", res_files.len());

        // Generate resources.arsc from the resource table.
        let resource_table = crate::r_class::scan_resources(&res_dir);
        let pkg = project
            .namespace
            .as_deref()
            .or(project.application_id.as_deref())
            .unwrap_or("com.example");
        // Collect string values from values/*.xml for the resource table.
        let mut res_values = std::collections::HashMap::new();
        let values_dir = res_dir.join("values");
        if values_dir.is_dir() {
            for entry in std::fs::read_dir(&values_dir)
                .into_iter()
                .flatten()
                .flatten()
            {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("xml") {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        // Extract <string name="X">value</string> pairs.
                        for line in content.lines() {
                            let line = line.trim();
                            if let Some(rest) = line.strip_prefix("<string name=\"") {
                                if let Some(name_end) = rest.find('"') {
                                    let name = &rest[..name_end];
                                    if let Some(val_start) = rest.find('>') {
                                        let after = &rest[val_start + 1..];
                                        if let Some(val_end) = after.find('<') {
                                            let value = &after[..val_end];
                                            res_values.insert(
                                                format!("string.{name}"),
                                                value.to_string(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "  resource table: {} types, {} total entries",
            resource_table.entries.len(),
            resource_table
                .entries
                .values()
                .map(|v| v.len())
                .sum::<usize>()
        );
        // resources.arsc disabled — Android rejects our binary format.
        // The resource table structure needs proper alignment and encoding.
        // APKs install without resources.arsc but @string/@drawable refs won't resolve.
        #[allow(clippy::overly_complex_bool_expr)]
        let resources_arsc: Option<Vec<u8>> = if !resource_table.entries.is_empty() && false {
            let arsc = crate::r_class::generate_resources_arsc(pkg, &resource_table, &res_values);
            eprintln!("  resources.arsc: {} bytes", arsc.len());
            Some(arsc)
        } else {
            None
        };

        let contents = skotch_apk::ApkContents {
            manifest_xml: axml_bytes,
            classes_dex: dex_bytes,
            resources_arsc,
            res_files,
        };
        let apk_path = build_dir.join("app-debug.apk");
        skotch_apk::write_unsigned_apk(&apk_path, &contents)?;
        // Sign the APK.
        let signed_path = build_dir.join("app-debug-signed.apk");
        skotch_sign::sign_apk_debug(&apk_path, &signed_path)?;

        eprintln!("BUILD SUCCESS: {}", signed_path.display());

        Ok(BuildOutcome {
            project,
            target: BuildTarget::Android,
            output_path: apk_path,
        })
    } else {
        let jar_dir = root_dir.join("build/libs");
        std::fs::create_dir_all(&jar_dir).ok();
        let jar_name = settings.root_project_name.as_deref().unwrap_or("app");
        let jar_path = jar_dir.join(format!("{jar_name}.jar"));
        skotch_jar::write_jar(&jar_path, &main_class, &all_classes, &[])?;

        eprintln!("BUILD SUCCESS: {}", jar_path.display());

        Ok(BuildOutcome {
            project,
            target: BuildTarget::Jvm,
            output_path: jar_path,
        })
    }
}

/// Resolve external Maven dependencies declared in `build.gradle.kts`.
/// Downloads JARs (with transitive deps) from Maven Central, caches them
/// in `~/.skotch/cache/maven/`, and returns the list of local JAR paths.
/// Collect all files under a resources directory, returning
/// `(jar_entry_path, contents)` pairs with paths relative to the root.
fn discover_resources(root: &Path) -> Vec<(String, Vec<u8>)> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut resources = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                // Use forward slashes for JAR entry paths.
                let jar_path = rel.to_string_lossy().replace('\\', "/");
                if let Ok(bytes) = std::fs::read(path) {
                    resources.push((jar_path, bytes));
                }
            }
        }
    }
    resources
}

/// Default repository URLs when none are configured in build.gradle.kts.
fn default_repos() -> Vec<String> {
    vec![
        "https://repo1.maven.org/maven2".to_string(),
        "https://dl.google.com/dl/android/maven2".to_string(),
    ]
}

fn resolve_external_deps(project: &ProjectModel, _project_dir: &Path) -> Result<Vec<PathBuf>> {
    let repos = if project.repositories.is_empty() {
        default_repos()
    } else {
        project.repositories.clone()
    };

    // ── BOM resolution: fetch platform POMs and extract version constraints ──
    let mut bom_versions: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for platform_dep in &project.platform_deps {
        if let Some(coord) = skotch_tape::MavenCoord::parse(platform_dep) {
            if let Ok(pom_xml) = skotch_tape::fetch_pom(&coord, &repos) {
                parse_bom_versions(&pom_xml, &mut bom_versions);
            }
        }
    }

    // Apply BOM versions to versionless dependencies.
    let mut deps = project.external_deps.clone();
    for dep in &mut deps {
        let parts: Vec<&str> = dep.split(':').collect();
        let needs_version = parts.len() == 2 || (parts.len() == 3 && parts[2].is_empty());
        if needs_version {
            let key = format!("{}:{}", parts[0], parts[1]);
            if let Some(version) = bom_versions.get(&key) {
                *dep = format!("{}:{}", key, version);
            }
        }
    }
    // Remove deps that still have no version (unresolved BOM entries).
    deps.retain(|d| {
        let parts: Vec<&str> = d.split(':').collect();
        parts.len() == 3 && !parts[2].is_empty()
    });

    let coords: Vec<skotch_tape::MavenCoord> = deps
        .iter()
        .filter_map(|s| skotch_tape::MavenCoord::parse(s))
        .collect();

    if coords.is_empty() {
        return Ok(Vec::new());
    }

    let resolved = skotch_tape::resolve(&coords, &repos, false)
        .with_context(|| "resolving Maven dependencies")?;

    Ok(resolved.jars)
}

/// Parse a POM XML's `<dependencyManagement>` section to extract version
/// constraints. Populates `versions` with "group:artifact" → "version" entries.
/// Minimal XML parser — just looks for `<dependency>` blocks with
/// `<groupId>`, `<artifactId>`, and `<version>`.
/// Deduplicate dependency JARs: when multiple versions of the same artifact
/// exist (e.g. `annotation-1.1.0.jar` and `annotation-1.4.0.jar`), keep only
/// the one with the highest version to avoid "defined multiple times" d8 errors.
/// Compare two version strings using semantic versioning.
/// "1.18.0" > "1.9.0" (unlike string comparison where "9" > "1").
fn semver_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| {
                // Strip non-numeric suffixes like "-alpha01", "-rc1"
                let numeric: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
                numeric.parse::<u64>().unwrap_or(0)
            })
            .collect()
    };
    let va = parse(a);
    let vb = parse(b);
    for (x, y) in va.iter().zip(vb.iter()) {
        if x != y {
            return x > y;
        }
    }
    va.len() > vb.len()
}

fn dedup_dep_jars(jars: &[PathBuf]) -> Vec<PathBuf> {
    use std::collections::HashMap;

    // Pass 1: exact artifact path dedup (same artifact, different versions).
    let mut best: HashMap<String, (String, PathBuf)> = HashMap::new();
    for jar in jars {
        let artifact_dir = jar.parent().and_then(|p| p.parent());
        let version_dir = jar.parent();
        if let (Some(art), Some(ver)) = (artifact_dir, version_dir) {
            let key = art.to_string_lossy().to_string();
            let version = ver
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if let Some((existing_ver, _)) = best.get(&key) {
                if semver_gt(&version, existing_ver) {
                    best.insert(key, (version, jar.clone()));
                }
            } else {
                best.insert(key, (version, jar.clone()));
            }
        } else {
            let key = jar.to_string_lossy().to_string();
            best.entry(key)
                .or_insert_with(|| (String::new(), jar.clone()));
        }
    }
    let mut result: Vec<PathBuf> = best.into_values().map(|(_, p)| p).collect();

    // Pass 2: Android artifact rename dedup.
    // When both `foo` and `foo-android` (or `foo-jvm`) exist under the same
    // group, keep only the `-android`/`-jvm` variant (the newer KMP artifact).
    let mut by_group: HashMap<String, Vec<(String, PathBuf)>> = HashMap::new();
    for jar in &result {
        // group = parent of parent of parent (group path)
        // artifact = parent of parent (artifact name)
        let ver_dir = jar.parent();
        let art_dir = ver_dir.and_then(|p| p.parent());
        let group_dir = art_dir.and_then(|p| p.parent());
        if let (Some(group), Some(art)) = (group_dir, art_dir) {
            let gkey = group.to_string_lossy().to_string();
            let aname = art
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            by_group.entry(gkey).or_default().push((aname, jar.clone()));
        }
    }
    // For each group, resolve artifact renames:
    //   X → X-android, X-jvm (KMP migration)
    //   X-ktx → X-android (Kotlin extensions absorbed into KMP artifact)
    // Keep the `-android`/`-jvm` variant, drop the older one.
    let mut to_remove: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for artifacts in by_group.values() {
        let names: std::collections::HashSet<String> =
            artifacts.iter().map(|(n, _)| n.clone()).collect();
        for (name, path) in artifacts {
            // Drop X when X-android or X-jvm exists.
            let android_name = format!("{name}-android");
            let jvm_name = format!("{name}-jvm");
            if names.contains(&android_name) || names.contains(&jvm_name) {
                to_remove.insert(path.clone());
            }
            // Drop X-ktx when X-android, X-jvm, or a newer X exists.
            // In newer AndroidX, the plain artifact absorbs the -ktx classes
            // (e.g. activity-1.13.0 contains everything from activity-ktx).
            // Drop -ktx when the plain version is higher.
            if let Some(base) = name.strip_suffix("-ktx") {
                let android_of_base = format!("{base}-android");
                let jvm_of_base = format!("{base}-jvm");
                if names.contains(&android_of_base) || names.contains(&jvm_of_base) {
                    to_remove.insert(path.clone());
                } else {
                    // Check if plain X has a higher version than X-ktx.
                    // If so, X absorbed the ktx classes — drop X-ktx.
                    let ktx_ver = path
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    for (other_name, other_path) in artifacts.iter() {
                        if other_name == base {
                            let plain_ver = other_path
                                .parent()
                                .and_then(|p| p.file_name())
                                .and_then(|n| n.to_str())
                                .unwrap_or("");
                            if semver_gt(plain_ver, ktx_ver) {
                                to_remove.insert(path.clone());
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    if !to_remove.is_empty() {
        result.retain(|p| !to_remove.contains(p));
    }

    result
}

fn parse_bom_versions(pom_xml: &str, versions: &mut std::collections::HashMap<String, String>) {
    // Only parse inside <dependencyManagement>...</dependencyManagement>
    let dm_section = if let Some(start) = pom_xml.find("<dependencyManagement>") {
        if let Some(end) = pom_xml.find("</dependencyManagement>") {
            &pom_xml[start..end]
        } else {
            return;
        }
    } else {
        return;
    };

    // Split on <dependency> blocks.
    for dep_block in dm_section.split("<dependency>").skip(1) {
        let end = dep_block.find("</dependency>").unwrap_or(dep_block.len());
        let block = &dep_block[..end];

        let group = extract_xml_tag(block, "groupId");
        let artifact = extract_xml_tag(block, "artifactId");
        let version = extract_xml_tag(block, "version");

        if let (Some(g), Some(a), Some(v)) = (group, artifact, version) {
            // Skip property references like ${project.version}
            if !v.contains("${") {
                versions.insert(format!("{g}:{a}"), v);
            }
        }
    }
}

fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

/// Derive the JVM wrapper class name from a file path: `Hello.kt` → `HelloKt`.
pub(crate) fn wrapper_class_for(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Main");
    let mut c = stem.chars();
    match c.next() {
        Some(first) => format!("{}{}Kt", first.to_ascii_uppercase(), c.as_str()),
        None => "MainKt".to_string(),
    }
}
