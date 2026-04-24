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
use skotch_buildscript::{parse_buildfile, parse_settings, BuildTarget, ProjectModel};
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
    let parsed = parse_buildfile(&buildfile_text, buildfile_id, &mut interner);

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
        eprintln!("  {} dependencies resolved", resolved_jars.len());
    }

    // Discover sources.
    let src_dir = project_dir.join("src/main/kotlin");
    let src_files =
        discover_sources(&src_dir).with_context(|| format!("scanning {}", src_dir.display()))?;
    if src_files.is_empty() {
        anyhow::bail!("no .kt sources found under {}", src_dir.display());
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
        skotch_jar::write_jar(&jar_path, &main_class, classes)
            .with_context(|| format!("writing {}", jar_path.display()))?;
    } else {
        skotch_jar::write_fat_jar(&jar_path, &main_class, classes, dep_jars)
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
    skotch_jar::write_jar(&jar_path, &main_class, &classes)
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
    // 1. Compile to DEX.
    let dex_bytes = skotch_backend_dex::compile_module(module);

    // 2. Encode AndroidManifest.xml to binary AXML.
    //    Try to read a source AndroidManifest.xml from the project, or
    //    build a minimal one from the ProjectModel.
    let manifest_path = project_dir.join("src/main/AndroidManifest.xml");
    let manifest_elem = if manifest_path.exists() {
        // TODO: parse the source XML and convert to Element tree.
        // For now, build from ProjectModel even if the file exists.
        build_manifest_from_project(project)
    } else {
        build_manifest_from_project(project)
    };
    let axml_bytes = skotch_axml::encode_axml(&manifest_elem);

    // 3. Assemble unsigned APK.
    let contents = skotch_apk::ApkContents {
        manifest_xml: axml_bytes,
        classes_dex: dex_bytes,
        resources_arsc: None,
        res_files: vec![],
    };

    let build_dir = project_dir.join("build");
    std::fs::create_dir_all(&build_dir).ok();
    let apk_path = build_dir.join("app-unsigned.apk");
    skotch_apk::write_unsigned_apk(&apk_path, &contents)
        .with_context(|| format!("writing {}", apk_path.display()))?;

    eprintln!("BUILD SUCCESS: {}", apk_path.display());

    Ok(BuildOutcome {
        project: project.clone(),
        target: BuildTarget::Android,
        output_path: apk_path,
    })
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

    for module_path in &settings.included_modules {
        let dir_name = module_path.trim_start_matches(':');
        let module_dir = root_dir.join(dir_name);
        let bf = module_dir.join("build.gradle.kts");
        if !bf.exists() {
            anyhow::bail!("build.gradle.kts not found for module {module_path}");
        }
        let text = std::fs::read_to_string(&bf)?;
        let fid = sm.add(bf, text.clone());
        let parsed = parse_buildfile(&text, fid, &mut interner);
        let mut project = parsed.project;
        project.project_name = Some(dir_name.to_string());
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
        type ModuleBuildResult = (usize, Vec<(String, Vec<u8>)>, bool);
        let level_results: Vec<ModuleBuildResult> = level_modules
            .iter()
            .map(|&idx| {
                let module = &modules[idx];
                let src_dir = module.dir.join("src/main/kotlin");
                let src_files = discover_sources(&src_dir).unwrap_or_default();
                if src_files.is_empty() {
                    return (idx, Vec::new(), false);
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
                for (_fid, ast, wrapper) in &parsed {
                    let mir = skotch_driver::compile_ast(
                        ast,
                        wrapper,
                        &mut mod_interner,
                        &mut mod_diags,
                        Some(&combined_symbols),
                    );
                    classes.extend(skotch_backend_jvm::compile_module(&mir, &mod_interner));
                }

                (idx, classes, mod_diags.has_errors())
            })
            .collect();

        // Collect results and store per-module symbols for downstream modules.
        for (idx, classes, has_errors) in level_results {
            if has_errors {
                // Re-gather to store symbols even on error.
            }
            // Store this module's own symbol table for downstream modules.
            let module = &modules[idx];
            let src_dir = module.dir.join("src/main/kotlin");
            let src_files = discover_sources(&src_dir).unwrap_or_default();
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
        }
    }

    if diags.has_errors() {
        eprint!("{}", render(&diags, &sm));
        anyhow::bail!("compilation failed");
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

    // Package as JAR (Gradle-compatible: build/libs/{project-name}.jar).
    let jar_dir = root_dir.join("build/libs");
    std::fs::create_dir_all(&jar_dir).ok();
    let jar_name = settings.root_project_name.as_deref().unwrap_or("app");
    let jar_path = jar_dir.join(format!("{jar_name}.jar"));
    skotch_jar::write_jar(&jar_path, &main_class, &all_classes)?;

    eprintln!("  {} modules, {} classes compiled", n, all_classes.len());
    eprintln!("BUILD SUCCESS: {}", jar_path.display());

    Ok(BuildOutcome {
        project,
        target: BuildTarget::Jvm,
        output_path: jar_path,
    })
}

/// Resolve external Maven dependencies declared in `build.gradle.kts`.
/// Downloads JARs (with transitive deps) from Maven Central, caches them
/// in `~/.skotch/cache/maven/`, and returns the list of local JAR paths.
fn resolve_external_deps(project: &ProjectModel, _project_dir: &Path) -> Result<Vec<PathBuf>> {
    if project.external_deps.is_empty() {
        return Ok(Vec::new());
    }

    let coords: Vec<skotch_tape::MavenCoord> = project
        .external_deps
        .iter()
        .filter_map(|s| skotch_tape::MavenCoord::parse(s))
        .collect();

    if coords.is_empty() {
        return Ok(Vec::new());
    }

    let repos = vec!["https://repo1.maven.org/maven2".to_string()];
    let resolved = skotch_tape::resolve(&coords, &repos, false)
        .with_context(|| "resolving Maven dependencies")?;

    Ok(resolved.jars)
}

/// Derive the JVM wrapper class name from a file path: `Hello.kt` → `HelloKt`.
fn wrapper_class_for(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Main");
    let mut c = stem.chars();
    match c.next() {
        Some(first) => format!("{}{}Kt", first.to_ascii_uppercase(), c.as_str()),
        None => "MainKt".to_string(),
    }
}
