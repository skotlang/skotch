//! End-to-end build pipeline with salsa-based incremental + parallel compilation.
//!
//! The pipeline uses a salsa [`skotch_db::Db`] for memoized, demand-driven
//! compilation. Each source file is a salsa input; the front-end pipeline
//! (lex → parse → resolve → typecheck → MIR) is a tracked function that
//! salsa automatically caches. Files are compiled in parallel via rayon
//! with cloned database handles.

use crate::discover::{discover_sources, find_build_file, find_settings_file};
use crate::merge::merge_modules;
use anyhow::{Context, Result};
use rayon::prelude::*;
use skotch_buildscript::{parse_buildfile, parse_settings, BuildTarget, ProjectModel};
use skotch_diagnostics::{render, Diagnostics};
use skotch_intern::Interner;
use skotch_mir::MirModule;
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
    if let Some(settings_path) = find_settings_file(&opts.project_dir) {
        let settings_dir = settings_path.parent().unwrap().to_path_buf();
        let settings_text = std::fs::read_to_string(&settings_path)?;
        let mut interner = Interner::new();
        let sm_file = skotch_span::FileId(0);
        let parsed = parse_settings(&settings_text, sm_file, &mut interner);
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

    // Discover sources.
    let src_dir = project_dir.join("src/main/kotlin");
    let src_files =
        discover_sources(&src_dir).with_context(|| format!("scanning {}", src_dir.display()))?;
    if src_files.is_empty() {
        anyhow::bail!("no .kt sources found under {}", src_dir.display());
    }

    // ── Salsa-based incremental + parallel compilation ────────────────
    //
    // Each source file is registered as a salsa input. The `compile_file`
    // tracked function runs the full front-end pipeline and is memoized
    // by salsa. On rebuild, only files whose text changed are recompiled.
    // Files are compiled in parallel via rayon with cloned db handles.
    let db = skotch_db::Db::new();
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

    // Compile all files in parallel via salsa + rayon.
    let results = db.compile_all(&salsa_files);

    // Merge MIR modules and check for errors.
    let mut module = MirModule::default();
    let mut error_count = 0;
    for (file_module, has_errors) in results {
        if has_errors {
            error_count += 1;
        }
        merge_modules(&mut module, file_module);
    }

    if error_count > 0 {
        anyhow::bail!("compilation failed with {error_count} file(s) containing errors");
    }

    // Backend dispatch.
    match target {
        BuildTarget::Jvm => build_jvm(&project, &project_dir, &module, &interner),
        BuildTarget::Android => build_android(&project, &project_dir, &module),
        BuildTarget::Native => {
            anyhow::bail!("native target not yet implemented for `skotch build`");
        }
    }
}

fn build_jvm(
    project: &ProjectModel,
    project_dir: &Path,
    module: &MirModule,
    interner: &Interner,
) -> Result<BuildOutcome> {
    let classes = skotch_backend_jvm::compile_module(module, interner);

    // Write individual .class files in parallel.
    let classes_dir = project_dir.join("build/classes");
    std::fs::create_dir_all(&classes_dir)
        .with_context(|| format!("creating {}", classes_dir.display()))?;
    classes.par_iter().for_each(|(name, bytes)| {
        let path = classes_dir.join(format!("{name}.class"));
        // When a package prefix is present, `name` contains `/` separators
        // (e.g. `com/example/Greeter`), so create intermediate directories.
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = std::fs::write(&path, bytes);
    });

    // Determine main class.
    let main_class = project
        .main_class
        .clone()
        .or_else(|| {
            classes
                .iter()
                .find(|(n, _)| n.ends_with("Kt"))
                .map(|(n, _)| n.clone())
        })
        .or_else(|| classes.first().map(|(n, _)| n.clone()))
        .unwrap_or_else(|| "Main".to_string());

    // Build a runnable JAR.
    let jar_dir = project_dir.join("build");
    std::fs::create_dir_all(&jar_dir).ok();
    let jar_name = project
        .group
        .as_deref()
        .and_then(|g| g.rsplit('.').next())
        .unwrap_or("app");
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

/// Build a multi-module project. Compiles each module in dependency
/// order and merges everything into the "app" module's artifact.
fn build_multi_module(
    root_dir: &Path,
    settings: &skotch_buildscript::SettingsModel,
    opts: &BuildOptions,
) -> Result<BuildOutcome> {
    let mut sm = SourceMap::new();
    let mut interner = Interner::new();

    // Parse each module's build.gradle.kts.
    struct ModuleInfo {
        #[allow(dead_code)]
        name: String,
        dir: PathBuf,
        project: ProjectModel,
    }
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
        modules.push(ModuleInfo {
            name: dir_name.to_string(),
            dir: module_dir,
            project: parsed.project,
        });
    }

    // Topological sort: compile dependency modules before dependents.
    // Simple approach: modules with no project_deps go first.
    modules.sort_by_key(|m| m.project.project_deps.len());

    // Compile each module and collect class files.
    let mut all_classes: Vec<(String, Vec<u8>)> = Vec::new();
    let mut app_project: Option<ProjectModel> = None;
    let mut diags = Diagnostics::new();

    for module in &modules {
        let src_dir = module.dir.join("src/main/kotlin");
        let src_files = discover_sources(&src_dir).unwrap_or_default();
        if src_files.is_empty() {
            continue;
        }

        let mut module_mir = MirModule::default();
        for path in &src_files {
            let text = std::fs::read_to_string(path)?;
            let file_id = sm.add(path.clone(), text.clone());
            let class_name = wrapper_class_for(path);
            let file_module = skotch_driver::compile_source(
                &text,
                file_id,
                &class_name,
                &mut interner,
                &mut diags,
            );
            merge_modules(&mut module_mir, file_module);
        }

        let classes = skotch_backend_jvm::compile_module(&module_mir, &interner);
        all_classes.extend(classes);

        // Track the "app" module (the one with a main class or the last one).
        if module.project.main_class.is_some() || app_project.is_none() {
            app_project = Some(module.project.clone());
        }
    }

    if diags.has_errors() {
        eprint!("{}", render(&diags, &sm));
        anyhow::bail!("compilation failed");
    }

    let project = app_project.unwrap_or_default();
    let mut project = project;
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
                .find(|(n, _)| n.ends_with("Kt"))
                .map(|(n, _)| n.clone())
        })
        .unwrap_or_else(|| "MainKt".to_string());

    // Package as JAR.
    let build_dir = root_dir.join("build");
    std::fs::create_dir_all(&build_dir).ok();
    let jar_path = build_dir.join("app.jar");
    skotch_jar::write_jar(&jar_path, &main_class, &all_classes)?;

    eprintln!("BUILD SUCCESS: {}", jar_path.display());

    Ok(BuildOutcome {
        project,
        target: BuildTarget::Jvm,
        output_path: jar_path,
    })
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
