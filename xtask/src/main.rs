//! `cargo xtask` driver — refreshes the committed fixture goldens.
//!
//! ## What this binary does (and what it explicitly is allowed to do)
//!
//! `xtask` is the **only** crate in the workspace permitted to invoke
//! external compilers. It shells out to `kotlinc`, `d8`, `java`, and
//! `kotlinc-native` to produce reference outputs for the validation
//! fixtures, and writes the resulting bytes into
//! `tests/fixtures/expected/<target>/<fixture>/`. Those bytes are
//! committed to git so that **CI never needs any of those tools**.
//!
//! The shipping `skotch` binary must contain none of the strings
//! `kotlinc`, `javac`, `d8`, or `dx`. A test in
//! `tests/no_external_compiler.rs` enforces this.
//!
//! ## Subcommands
//!
//! - `cargo xtask gen-fixtures --target jvm` — for each fixture
//!   directory under `tests/fixtures/inputs/` whose `meta.toml` says
//!   `status = "supported"`:
//!     1. Run `skotch emit --target jvm` and write `expected/jvm/<f>/skotch.class`
//!        + `expected/jvm/<f>/skotch.norm.txt`.
//!     2. If `kotlinc` is on `PATH`, run it and write
//!        `expected/jvm/<f>/kotlinc.class` + `kotlinc.norm.txt`.
//!     3. If `java` is on `PATH`, execute the kotlinc-compiled class
//!        and capture its stdout as `expected/jvm/<f>/run.stdout`.
//!
//! - `cargo xtask refresh-skotch-goldens` — same as above but skip
//!   reference tools entirely; just refresh skotch's own outputs.
//!
//! - `cargo xtask verify` — re-run skotch on each fixture and assert
//!   the output is byte-equal to the committed goldens. This is what
//!   the workspace tests do too, but `xtask verify` is convenient
//!   when you've just edited the JVM backend and want a quick check.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::Command;

use skotch_driver::{emit, EmitOptions, Target as SkotchTarget};

#[derive(Parser, Debug)]
#[command(name = "xtask", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Sub,
}

#[derive(Subcommand, Debug)]
enum Sub {
    /// Regenerate committed fixture outputs (skotch's + reference tools').
    GenFixtures {
        #[arg(long, value_enum, default_value_t = TargetArg::Jvm)]
        target: TargetArg,
        /// Only regenerate this one fixture (by directory name).
        #[arg(long)]
        fixture: Option<String>,
        /// Skip running reference tools (kotlinc/d8/kotlinc-native).
        #[arg(long)]
        skotch_only: bool,
    },
    /// Regenerate skotch's own goldens without invoking reference tools.
    RefreshSkotchGoldens {
        #[arg(long, value_enum, default_value_t = TargetArg::Jvm)]
        target: TargetArg,
    },
    /// Re-run skotch on each fixture and verify output matches committed goldens.
    Verify {
        #[arg(long, value_enum, default_value_t = TargetArg::Jvm)]
        target: TargetArg,
    },
    /// Re-normalize committed `.class` goldens using the current
    /// `skotch-classfile-norm` decoder. Useful after the disassembler is
    /// extended (new opcode coverage) — refreshes both `kotlinc.norm.txt`
    /// and `skotch.norm.txt` from the committed `.class` bytes without
    /// invoking `kotlinc` or `skotch`.
    RenormGoldens,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum TargetArg {
    Jvm,
    Dex,
    Llvm,
    Klib,
    Native,
}

impl From<TargetArg> for SkotchTarget {
    fn from(t: TargetArg) -> Self {
        match t {
            TargetArg::Jvm => SkotchTarget::Jvm,
            TargetArg::Dex => SkotchTarget::Dex,
            TargetArg::Llvm => SkotchTarget::Llvm,
            TargetArg::Klib => SkotchTarget::Klib,
            TargetArg::Native => SkotchTarget::Native,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let workspace = workspace_root()?;
    match cli.cmd {
        Sub::GenFixtures {
            target,
            fixture,
            skotch_only,
        } => gen_fixtures(&workspace, target, fixture.as_deref(), skotch_only),
        Sub::RefreshSkotchGoldens { target } => gen_fixtures(&workspace, target, None, true),
        Sub::Verify { target } => verify(&workspace, target),
        Sub::RenormGoldens => renorm_goldens(&workspace),
    }
}

fn renorm_goldens(workspace: &Path) -> Result<()> {
    let jvm_dir = workspace.join("tests/fixtures/expected/jvm");
    let mut count = 0u32;
    let mut entries: Vec<_> = std::fs::read_dir(&jvm_dir)?.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        for inner in std::fs::read_dir(&dir)?.flatten() {
            let p = inner.path();
            let Some(ext) = p.extension() else { continue };
            if ext != "class" {
                continue;
            }
            let bytes = std::fs::read(&p)?;
            let normed = skotch_classfile_norm::normalize_default(&bytes)
                .map_err(|e| anyhow!("normalizing {}: {e}", p.display()))?;
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            let out = dir.join(format!("{stem}.norm.txt"));
            std::fs::write(&out, normed.as_text())?;
            count += 1;
        }
    }
    println!("renormalized {count} files");
    Ok(())
}

fn workspace_root() -> Result<PathBuf> {
    // CARGO_MANIFEST_DIR for xtask is `xtask/`; the workspace root is its parent.
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("could not locate workspace root"))
}

fn gen_fixtures(
    workspace: &Path,
    target: TargetArg,
    only: Option<&str>,
    skotch_only: bool,
) -> Result<()> {
    let inputs = workspace.join("tests/fixtures/inputs");
    if !inputs.exists() {
        bail!("no fixture inputs at {}", inputs.display());
    }
    let fixtures = list_supported_fixtures(&inputs)?;
    let kotlinc = which::which("kotlinc").ok();
    let java = which::which("java").ok();
    let d8 = locate_d8();
    let kotlinc_native = which::which("kotlinc-native").ok();
    let clang = which::which("clang").ok();
    if !skotch_only {
        if kotlinc.is_none() {
            eprintln!("warning: kotlinc not found on PATH; skipping reference outputs");
        }
        if java.is_none() {
            eprintln!("warning: java not found on PATH; skipping reference run.stdout");
        }
        if matches!(target, TargetArg::Dex) && d8.is_none() {
            eprintln!(
                "warning: d8 not found under $ANDROID_HOME/build-tools/*/d8, \
                 $ANDROID_SDK_ROOT/build-tools/*/d8, or on PATH; \
                 skipping dex reference outputs"
            );
        }
        if matches!(
            target,
            TargetArg::Klib | TargetArg::Llvm | TargetArg::Native
        ) && kotlinc_native.is_none()
        {
            eprintln!("warning: kotlinc-native not found on PATH; skipping native references");
        }
        if matches!(target, TargetArg::Llvm | TargetArg::Native) && clang.is_none() {
            eprintln!("warning: clang not found on PATH; skipping LLVM IR conversion");
        }
    }
    for f in fixtures {
        if let Some(name) = only {
            if f.dir_name != name {
                continue;
            }
        }
        match target {
            TargetArg::Jvm => gen_one_jvm(&f, workspace, &kotlinc, &java, skotch_only)?,
            TargetArg::Dex => gen_one_dex(&f, workspace, &kotlinc, &d8, skotch_only)?,
            TargetArg::Klib => gen_one_klib(&f, workspace, &kotlinc_native, skotch_only)?,
            TargetArg::Llvm => gen_one_llvm(&f, workspace, &kotlinc_native, &clang, skotch_only)?,
            TargetArg::Native => gen_one_native(&f, workspace, &kotlinc_native, skotch_only)?,
        }
    }
    Ok(())
}

/// Locate `d8`, preferring the latest Android SDK build-tools install.
///
/// Resolution order:
///
/// 1. `$ANDROID_HOME/build-tools/<version>/d8` — the standard
///    environment variable that the Android SDK manager and Android
///    Studio set when an SDK is installed. We pick the latest
///    version directory by lexical sort, which lines up with semver
///    for Google's `xx.y.z` build-tool versioning.
/// 2. `$ANDROID_SDK_ROOT/build-tools/<version>/d8` — the older but
///    still-recognized variable name. Some CI runners and build
///    systems set this instead of `ANDROID_HOME`.
/// 3. `which d8` — fall back to anything on `PATH`.
///
/// Returns `None` if no `d8` can be found anywhere; the caller
/// (which sets `skot_only` or warns) handles the missing-tool case.
fn locate_d8() -> Option<PathBuf> {
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        let Some(home) = std::env::var_os(var) else {
            continue;
        };
        let build_tools = PathBuf::from(home).join("build-tools");
        let Ok(read_dir) = std::fs::read_dir(&build_tools) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = read_dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("d8").exists())
            .collect();
        // Lexical sort matches Google's `xx.y.z` build-tool versioning,
        // so the last entry is the newest.
        versions.sort();
        if let Some(latest) = versions.last() {
            return Some(latest.join("d8"));
        }
    }
    // Fall back to PATH.
    which::which("d8").ok()
}

#[derive(Debug)]
struct Fixture {
    /// Directory name like `02-println-string-literal`.
    dir_name: String,
    /// Absolute path to its `input.kt` file.
    input: PathBuf,
}

fn list_supported_fixtures(inputs: &Path) -> Result<Vec<Fixture>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(inputs).min_depth(1).max_depth(1) {
        let entry = entry?;
        if !entry.file_type().is_dir() {
            continue;
        }
        let dir = entry.path().to_path_buf();
        let input = dir.join("input.kt");
        if !input.exists() {
            continue;
        }
        let meta_path = dir.join("meta.toml");
        if meta_path.exists() {
            let s = std::fs::read_to_string(&meta_path).context("reading meta.toml")?;
            // Tiny hand-rolled "is supported" check — avoids depending on
            // the `toml` crate for a single key. Look for the value "stub"
            // on any line whose key is `status`, tolerating arbitrary
            // whitespace around the `=`.
            if s.lines().any(|line| {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("status") {
                    let rest = rest.trim_start();
                    if let Some(rest) = rest.strip_prefix('=') {
                        let rest = rest.trim();
                        return rest == "\"stub\"" || rest.starts_with("\"stub\"");
                    }
                }
                false
            }) {
                continue;
            }
        }
        let dir_name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("non-utf8 fixture dir name"))?;
        let _ = dir;
        out.push(Fixture { dir_name, input });
    }
    out.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
    Ok(out)
}

fn gen_one_jvm(
    f: &Fixture,
    workspace: &Path,
    kotlinc: &Option<PathBuf>,
    java: &Option<PathBuf>,
    skotch_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/jvm")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    // 1) skotch's own outputs (skotch.class + skotch.norm.txt).
    println!("[skotch]   {}", f.dir_name);
    let skotch_class = expected.join("skotch.class");
    let skotch_norm = expected.join("skotch.norm.txt");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skotch_class.clone(),
        target: SkotchTarget::Jvm,
        norm_out: Some(skotch_norm.clone()),
    })
    .with_context(|| format!("skotch emit on {}", f.dir_name))?;

    if skotch_only {
        return Ok(());
    }

    // 2) kotlinc reference, if available.
    if let Some(kc) = kotlinc {
        println!("[kotlinc]{}", f.dir_name);
        let tmp = tempdir(&f.dir_name)?;
        let source =
            std::fs::read_to_string(&f.input).with_context(|| format!("reading {:?}", f.input))?;
        let uses_compose = source.contains("@Composable");
        let uses_coroutines = source.contains("kotlinx.coroutines");
        let mut cmd = Command::new(kc);
        cmd.arg(&f.input)
            .arg("-d")
            .arg(&tmp)
            .arg("-jvm-target")
            .arg("17");
        let compose_aux = if uses_compose {
            match locate_compose_assets(kc) {
                Some(assets) => {
                    cmd.arg(format!("-Xplugin={}", assets.plugin_jar.display()));
                    cmd.arg("-cp").arg(&assets.runtime_classpath);
                    // Disable the runtime trace-event prologue so the
                    // emitted bytecode is the smaller "production" form
                    // — easier for skotch's transform to match
                    // byte-for-byte. Production Compose builds typically
                    // ship with traceMarkersEnabled=false anyway.
                    cmd.arg("-P").arg(
                        "plugin:androidx.compose.compiler.plugins.kotlin:traceMarkersEnabled=false",
                    );
                    Some(assets)
                }
                None => {
                    eprintln!(
                        "  compose-compiler-plugin or runtime not found in ~/.gradle/caches; \
                         skipping kotlinc reference for {}",
                        f.dir_name
                    );
                    return Ok(());
                }
            }
        } else if uses_coroutines {
            // Fixtures that import kotlinx.coroutines (delay, runBlocking,
            // launch, withContext, yield, …) need the coroutines library
            // on the classpath for kotlinc to resolve the symbols.
            // Without this kotlinc fails and we never write a reference
            // norm — leaving the suspend cluster permanently uncomparable.
            match locate_coroutines_jar() {
                Some(jar) => {
                    cmd.arg("-cp").arg(&jar);
                    None
                }
                None => {
                    eprintln!(
                        "  kotlinx-coroutines-core-jvm not found in ~/.gradle/caches; \
                         skipping kotlinc reference for {}",
                        f.dir_name
                    );
                    return Ok(());
                }
            }
        } else {
            None
        };
        let status = cmd
            .status()
            .with_context(|| format!("running kotlinc on {}", f.dir_name))?;
        let _ = compose_aux;
        if !status.success() {
            eprintln!("  kotlinc failed; skipping reference for {}", f.dir_name);
        } else {
            // Find the produced .class file. When `kotlinc` emits
            // multiple classes (e.g. coroutines, where
            // the wrapper class is accompanied by a synthetic
            // `$run$1` continuation), prefer a top-level
            // wrapper-shaped name — one whose stem ends with `Kt`
            // and contains no `$` separators — so the primary
            // reference matches the file skotch writes to
            // `skotch.class`. Falls back to the first class file
            // for inputs that don't follow the `*Kt` convention.
            let mut all_classes: Vec<std::path::PathBuf> = Vec::new();
            for e in walkdir::WalkDir::new(&tmp) {
                let e = e?;
                if e.path().extension().and_then(|s| s.to_str()) == Some("class") {
                    all_classes.push(e.path().to_path_buf());
                }
            }
            let class_path = all_classes
                .iter()
                .find(|p| {
                    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    !stem.contains('$') && stem.ends_with("Kt")
                })
                .or_else(|| {
                    all_classes.iter().find(|p| {
                        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        !stem.contains('$')
                    })
                })
                .or_else(|| all_classes.first())
                .cloned();
            if let Some(cp) = class_path {
                let bytes = std::fs::read(&cp)?;
                std::fs::write(expected.join("kotlinc.class"), &bytes)?;
                let normalized = skotch_classfile_norm::normalize_default(&bytes)
                    .map_err(|e| anyhow!("normalizing kotlinc output: {e}"))?;
                std::fs::write(expected.join("kotlinc.norm.txt"), normalized.as_text())?;

                // 3) Run with java to capture run.stdout.
                if let Some(j) = java {
                    let class_dir = cp.parent().unwrap().to_path_buf();
                    let class_name = cp.file_stem().unwrap().to_string_lossy().to_string();
                    // kotlinc output sometimes references
                    // `kotlin/jvm/internal/Intrinsics` for null checks
                    // on parameters, so we add `kotlin-stdlib.jar`
                    // (next to the kotlinc binary) to the classpath
                    // when running the reference. The skotch binary
                    // never does this — only this xtask does, and
                    // only when capturing reference run.stdout.
                    let mut cp_arg = class_dir.as_os_str().to_os_string();
                    if let Some(stdlib) = locate_kotlin_stdlib(kc) {
                        cp_arg.push(":");
                        cp_arg.push(stdlib);
                    }
                    // Coroutines fixtures need the coroutines JAR at
                    // runtime too — without it `java` fails with
                    // NoClassDefFoundError on kotlinx/coroutines/...
                    if uses_coroutines {
                        if let Some(jar) = locate_coroutines_jar() {
                            cp_arg.push(":");
                            cp_arg.push(jar);
                        }
                    }
                    let out = Command::new(j)
                        .arg("-cp")
                        .arg(&cp_arg)
                        .arg(&class_name)
                        .output()
                        .with_context(|| "running java on kotlinc output")?;
                    if out.status.success() {
                        std::fs::write(expected.join("run.stdout"), &out.stdout)?;
                    } else {
                        eprintln!(
                            "  java run failed for {}: {}",
                            f.dir_name,
                            String::from_utf8_lossy(&out.stderr)
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Generate one fixture's `--target dex` outputs:
///
/// 1. Run `skotch emit --target dex` to produce `skotch.dex` + `skotch.norm.txt`.
/// 2. If `kotlinc` is available, run it on the input to produce a
///    `.class`, then run `d8` on that `.class` to produce a reference
///    `d8.dex` + `d8.norm.txt`. The d8 reference is committed alongside
///    skotch's so test failures can show both.
fn gen_one_dex(
    f: &Fixture,
    workspace: &Path,
    kotlinc: &Option<PathBuf>,
    d8: &Option<PathBuf>,
    skotch_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/dex")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    // 1) skotch's outputs.
    println!("[skotch]   {}", f.dir_name);
    let skotch_dex = expected.join("skotch.dex");
    let skotch_norm = expected.join("skotch.norm.txt");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skotch_dex.clone(),
        target: SkotchTarget::Dex,
        norm_out: Some(skotch_norm.clone()),
    })
    .with_context(|| format!("skotch emit --target dex on {}", f.dir_name))?;

    if skotch_only {
        return Ok(());
    }

    // 2) kotlinc → .class → d8 → .dex.
    let (Some(kc), Some(d8_bin)) = (kotlinc, d8) else {
        return Ok(());
    };
    println!("[kotlinc]{}", f.dir_name);
    let tmp = tempdir(&f.dir_name)?;
    let kotlinc_out = tmp.join("kotlinc-out");
    std::fs::create_dir_all(&kotlinc_out).ok();
    let status = Command::new(kc)
        .arg(&f.input)
        .arg("-d")
        .arg(&kotlinc_out)
        .arg("-jvm-target")
        .arg("17")
        .status()
        .with_context(|| format!("running kotlinc on {}", f.dir_name))?;
    if !status.success() {
        eprintln!(
            "  kotlinc failed; skipping dex reference for {}",
            f.dir_name
        );
        return Ok(());
    }
    let class_files: Vec<PathBuf> = walkdir::WalkDir::new(&kotlinc_out)
        .into_iter()
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("class"))
        .map(|e| e.path().to_path_buf())
        .collect();
    if class_files.is_empty() {
        eprintln!("  no .class files produced for {}", f.dir_name);
        return Ok(());
    }

    println!("[d8]     {}", f.dir_name);
    let d8_out = tmp.join("d8-out");
    std::fs::create_dir_all(&d8_out).ok();
    let mut cmd = Command::new(d8_bin);
    for cf in &class_files {
        cmd.arg(cf);
    }
    cmd.arg("--output").arg(&d8_out);
    // d8 needs lib `android.jar` for type resolution. We use
    // `--lib` only if we can find it; otherwise d8 still works for
    // simple cases that don't reference Android types.
    let status = cmd
        .status()
        .with_context(|| format!("running d8 on {}", f.dir_name))?;
    if !status.success() {
        eprintln!("  d8 failed; skipping dex reference for {}", f.dir_name);
        return Ok(());
    }
    let d8_classes_dex = d8_out.join("classes.dex");
    if !d8_classes_dex.exists() {
        eprintln!("  d8 produced no classes.dex for {}", f.dir_name);
        return Ok(());
    }
    let bytes = std::fs::read(&d8_classes_dex)?;
    std::fs::write(expected.join("d8.dex"), &bytes)?;
    let normalized = skotch_dex_norm::normalize_default(&bytes)
        .map_err(|e| anyhow!("normalizing d8 output: {e}"))?;
    std::fs::write(expected.join("d8.norm.txt"), &normalized)?;
    Ok(())
}

/// Generate one fixture's `--target klib` outputs:
///
/// 1. `skotch emit --target klib` → `expected/klib/<f>/skotch.klib` and
///    its normalized text form (manifest + MIR JSON).
/// 2. If `kotlinc-native` is available, run it with `-p library` to
///    produce a reference `kotlinc-native.klib`. We do **not**
///    normalize the kotlinc-native klib (it's protobuf-encoded IR
///    that we don't currently parse) — it's committed as opaque bytes
///    so the round-trip from a fresh kotlinc-native install can be
///    diffed by hand.
fn gen_one_klib(
    f: &Fixture,
    workspace: &Path,
    kotlinc_native: &Option<PathBuf>,
    skotch_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/klib")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    println!("[skotch]   {}", f.dir_name);
    let skotch_klib = expected.join("skotch.klib");
    let skotch_norm = expected.join("skotch.norm.txt");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skotch_klib.clone(),
        target: SkotchTarget::Klib,
        norm_out: Some(skotch_norm),
    })
    .with_context(|| format!("skotch emit --target klib on {}", f.dir_name))?;

    if skotch_only {
        return Ok(());
    }
    let Some(kn) = kotlinc_native else {
        return Ok(());
    };

    println!("[kotlinc-native]{}", f.dir_name);
    let tmp = tempdir(&f.dir_name)?;
    let out_stem = tmp.join("ref");
    let status = Command::new(kn)
        .arg(&f.input)
        .arg("-p")
        .arg("library")
        .arg("-o")
        .arg(&out_stem)
        .status()
        .with_context(|| format!("running kotlinc-native -p library on {}", f.dir_name))?;
    if !status.success() {
        eprintln!(
            "  kotlinc-native failed; skipping reference klib for {}",
            f.dir_name
        );
        return Ok(());
    }
    let ref_klib = out_stem.with_extension("klib");
    if !ref_klib.exists() {
        eprintln!("  kotlinc-native produced no .klib for {}", f.dir_name);
        return Ok(());
    }
    let bytes = std::fs::read(&ref_klib)?;
    std::fs::write(expected.join("kotlinc-native.klib"), &bytes)?;
    Ok(())
}

/// Generate one fixture's `--target llvm` outputs:
///
/// 1. `skotch emit --target llvm` → `expected/llvm/<f>/skotch.ll` and
///    `skotch.norm.txt`.
/// 2. If `kotlinc-native` and `clang` are both available, run
///    kotlinc-native with `-p program` and `-Xtemporary-files-dir` to
///    capture the intermediate `out.bc`, then run `clang -S -emit-llvm`
///    to convert to text. The result is enormous (~7MB) because
///    kotlinc-native bundles its entire runtime, so we commit the
///    *normalized* form as `kotlinc-native.norm.txt` and skip the
///    raw `.ll`.
fn gen_one_llvm(
    f: &Fixture,
    workspace: &Path,
    kotlinc_native: &Option<PathBuf>,
    clang: &Option<PathBuf>,
    skotch_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/llvm")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    println!("[skotch]   {}", f.dir_name);
    let skotch_ll = expected.join("skotch.ll");
    let skotch_norm = expected.join("skotch.norm.txt");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skotch_ll.clone(),
        target: SkotchTarget::Llvm,
        norm_out: Some(skotch_norm),
    })
    .with_context(|| format!("skotch emit --target llvm on {}", f.dir_name))?;

    if skotch_only {
        return Ok(());
    }
    let (Some(kn), Some(clang_bin)) = (kotlinc_native, clang) else {
        return Ok(());
    };

    println!("[kotlinc-native+clang]{}", f.dir_name);
    let tmp = tempdir(&f.dir_name)?;
    let kn_tmp = tmp.join("kn-tmp");
    std::fs::create_dir_all(&kn_tmp).ok();
    let out_stem = tmp.join("ref");
    let status = Command::new(kn)
        .arg(&f.input)
        .arg("-p")
        .arg("program")
        .arg("-o")
        .arg(&out_stem)
        .arg(format!("-Xtemporary-files-dir={}", kn_tmp.display()))
        .status()
        .with_context(|| format!("running kotlinc-native -p program on {}", f.dir_name))?;
    if !status.success() {
        eprintln!(
            "  kotlinc-native failed; skipping LLVM reference for {}",
            f.dir_name
        );
        return Ok(());
    }
    let bc_path = kn_tmp.join("out.bc");
    if !bc_path.exists() {
        eprintln!("  kotlinc-native left no out.bc for {}", f.dir_name);
        return Ok(());
    }

    // Convert bitcode → text LLVM IR via clang.
    let kn_ll = tmp.join("kn.ll");
    let status = Command::new(clang_bin)
        .arg("-S")
        .arg("-emit-llvm")
        .arg(&bc_path)
        .arg("-o")
        .arg(&kn_ll)
        .status()
        .with_context(|| "converting kotlinc-native bitcode to text LLVM IR")?;
    if !status.success() {
        eprintln!("  clang -S -emit-llvm failed for {}; skipping", f.dir_name);
        return Ok(());
    }
    let kn_text = std::fs::read_to_string(&kn_ll)?;
    // The raw kotlinc-native .ll is enormous (~7 MB) because kotlin
    // native bundles its entire runtime. Committing the whole
    // normalized form would bloat the repo for no diffing value.
    // Instead, we extract a tiny *summary* — counts and a short
    // grep for fixture-specific evidence — that's enough to verify
    // kotlinc-native produced something containing the user's
    // strings.
    let summary = summarize_kotlinc_native_ll(&kn_text, &f.dir_name);
    std::fs::write(expected.join("kotlinc-native.summary.txt"), summary)?;
    Ok(())
}

/// Build a small text summary of a kotlinc-native LLVM IR dump:
/// counts of declarations/definitions/globals, and any line
/// containing "main" to confirm the entry point is present.
fn summarize_kotlinc_native_ll(text: &str, fixture_name: &str) -> String {
    let mut declarations = 0usize;
    let mut definitions = 0usize;
    let mut globals = 0usize;
    let mut main_lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("declare ") {
            declarations += 1;
        } else if line.starts_with("define ") {
            definitions += 1;
            if line.contains("main") {
                main_lines.push(line.to_string());
            }
        } else if line.starts_with('@') {
            globals += 1;
        }
    }
    main_lines.sort();
    main_lines.dedup();
    let mut out = String::new();
    out.push_str("# Summary of kotlinc-native LLVM IR for fixture: ");
    out.push_str(fixture_name);
    out.push('\n');
    out.push_str(&format!("declarations: {declarations}\n"));
    out.push_str(&format!("definitions:  {definitions}\n"));
    out.push_str(&format!("globals:      {globals}\n"));
    out.push_str("\n# define lines mentioning `main`:\n");
    for m in main_lines.iter().take(20) {
        out.push_str(m);
        out.push('\n');
    }
    out
}

/// Generate one fixture's `--target native` outputs:
///
/// 1. `skotch emit --target native` → `expected/native/<f>/skotch` (the
///    binary) and `expected/native/<f>/skotch.ll` (the IR that fed it).
/// 2. Run skotch's binary and capture stdout into
///    `expected/native/<f>/run.stdout`. This is what the
///    behavioral test diffs against.
/// 3. If `kotlinc-native` is available, run it with `-p program` to
///    produce a reference binary, run that and capture
///    `kotlinc-native.run.stdout` for cross-compiler comparison.
fn gen_one_native(
    f: &Fixture,
    workspace: &Path,
    kotlinc_native: &Option<PathBuf>,
    skotch_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/native")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    println!("[skotch]   {}", f.dir_name);
    let skotch_bin = expected.join("skotch");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skotch_bin.clone(),
        target: SkotchTarget::Native,
        norm_out: None,
    })
    .with_context(|| format!("skotch emit --target native on {}", f.dir_name))?;

    let out = Command::new(&skotch_bin)
        .output()
        .with_context(|| format!("running skotch binary {}", skotch_bin.display()))?;
    if out.status.success() {
        std::fs::write(expected.join("run.stdout"), &out.stdout)?;
    }

    if skotch_only {
        return Ok(());
    }
    let Some(kn) = kotlinc_native else {
        return Ok(());
    };

    println!("[kotlinc-native]{}", f.dir_name);
    let tmp = tempdir(&f.dir_name)?;
    let out_stem = tmp.join("ref");
    let status = Command::new(kn)
        .arg(&f.input)
        .arg("-p")
        .arg("program")
        .arg("-o")
        .arg(&out_stem)
        .status()
        .with_context(|| format!("running kotlinc-native -p program on {}", f.dir_name))?;
    if !status.success() {
        eprintln!(
            "  kotlinc-native failed; skipping native reference for {}",
            f.dir_name
        );
        return Ok(());
    }
    let kn_bin = out_stem.with_extension("kexe");
    if !kn_bin.exists() {
        eprintln!("  no .kexe produced for {}", f.dir_name);
        return Ok(());
    }
    let kn_out = Command::new(&kn_bin)
        .output()
        .with_context(|| "running kotlinc-native binary")?;
    if kn_out.status.success() {
        std::fs::write(expected.join("kotlinc-native.run.stdout"), &kn_out.stdout)?;
    }
    Ok(())
}

fn verify(workspace: &Path, target: TargetArg) -> Result<()> {
    let (subdir, ext) = match target {
        TargetArg::Jvm => ("jvm", "class"),
        TargetArg::Dex => ("dex", "dex"),
        TargetArg::Klib => ("klib", "klib"),
        TargetArg::Llvm => ("llvm", "ll"),
        TargetArg::Native => {
            bail!("--target native verifies via behavioral run, not byte equality")
        }
    };
    let skotch_target = SkotchTarget::from(target);
    let inputs = workspace.join("tests/fixtures/inputs");
    let fixtures = list_supported_fixtures(&inputs)?;
    let mut bad = 0;
    for f in &fixtures {
        let expected_dir = workspace
            .join("tests/fixtures/expected")
            .join(subdir)
            .join(&f.dir_name);
        let golden = expected_dir.join(format!("skotch.{ext}"));
        if !golden.exists() {
            eprintln!("MISSING: {}", f.dir_name);
            bad += 1;
            continue;
        }
        // Re-run skotch, write to a temp path, byte-compare.
        let tmp_dir = tempdir(&f.dir_name)?;
        let out = tmp_dir.join(format!("out.{ext}"));
        emit(&EmitOptions {
            input: f.input.clone(),
            output: out.clone(),
            target: skotch_target,
            norm_out: None,
        })?;
        let new_bytes = std::fs::read(&out)?;
        let golden_bytes = std::fs::read(&golden)?;
        if new_bytes != golden_bytes {
            eprintln!("MISMATCH: {}", f.dir_name);
            bad += 1;
        } else {
            println!("OK:       {}", f.dir_name);
        }
    }
    if bad > 0 {
        bail!("{bad} fixture(s) failed verification");
    }
    Ok(())
}

/// Try to locate `kotlin-stdlib.jar` relative to the `kotlinc` binary.
/// Returns `None` if the standard layout isn't recognized — which is
/// fine, the caller will just run `java` without it and the test
/// fixtures that don't need it will still work.
fn locate_kotlin_stdlib(kotlinc: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(kotlinc).ok()?;
    let bin = real.parent()?;
    let prefix = bin.parent()?;
    // Try both common layouts:
    //   .../bin/kotlinc → .../lib/kotlin-stdlib.jar          (most distros)
    //   .../bin/kotlinc → .../libexec/lib/kotlin-stdlib.jar  (Homebrew Cellar)
    for rel in ["lib/kotlin-stdlib.jar", "libexec/lib/kotlin-stdlib.jar"] {
        let candidate = prefix.join(rel);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

struct ComposeAssets {
    plugin_jar: PathBuf,
    /// Single classpath string with all Compose runtime JARs joined by `:`.
    runtime_classpath: String,
}

/// Locate the Compose compiler plugin JAR and a runtime classpath for
/// kotlinc. We require the *non-embeddable* plugin variant — the
/// `-embeddable.jar` form bundles its own shaded com.intellij/PsiElement
/// which conflicts with kotlinc's own preloader classpath. The plugin
/// is downloaded from Maven Central on first use and cached.
fn locate_compose_assets(kotlinc: &Path) -> Option<ComposeAssets> {
    // Pick a Compose plugin version compatible with the installed
    // kotlinc. Hard-coded for now — bump when bumping kotlinc.
    const COMPOSE_PLUGIN_VERSION: &str = "2.3.21";

    let cache_dir = std::env::temp_dir().join("skotch-compose-cache");
    std::fs::create_dir_all(&cache_dir).ok();

    // Plugin (non-embeddable). Maven path:
    //   org.jetbrains.kotlin:kotlin-compose-compiler-plugin:<ver>
    let plugin_path = cache_dir.join(format!(
        "kotlin-compose-compiler-plugin-{}.jar",
        COMPOSE_PLUGIN_VERSION
    ));
    if !plugin_path.exists() {
        let url = format!(
            "https://repo1.maven.org/maven2/org/jetbrains/kotlin/\
             kotlin-compose-compiler-plugin/{v}/\
             kotlin-compose-compiler-plugin-{v}.jar",
            v = COMPOSE_PLUGIN_VERSION
        );
        let status = Command::new("curl")
            .arg("-fsSL")
            .arg("-o")
            .arg(&plugin_path)
            .arg(&url)
            .status()
            .ok()?;
        if !status.success() {
            let _ = std::fs::remove_file(&plugin_path);
            return None;
        }
    }

    // Compose runtime: extract androidx.compose.runtime's classes.jar
    // from the matching `runtime.aar` in the user's Gradle cache. We
    // pick the newest version available locally.
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let gradle_cache = home
        .join(".gradle")
        .join("caches")
        .join("modules-2")
        .join("files-2.1");
    let runtime_dir = gradle_cache
        .join("androidx.compose.runtime")
        .join("runtime-android");
    let runtime_aar = walkdir::WalkDir::new(&runtime_dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy() == "runtime.aar")
        .max_by_key(|e| e.path().to_path_buf())?;
    let runtime_classes = cache_dir.join("compose-runtime-classes.jar");
    if !runtime_classes.exists() {
        let aar_bytes = std::fs::read(runtime_aar.path()).ok()?;
        let mut zip_arch = zip::ZipArchive::new(std::io::Cursor::new(aar_bytes)).ok()?;
        let mut classes_entry = zip_arch.by_name("classes.jar").ok()?;
        let mut out = Vec::new();
        std::io::copy(&mut classes_entry, &mut out).ok()?;
        std::fs::write(&runtime_classes, out).ok()?;
    }

    // The kotlin-stdlib is on the kotlinc PATH already, so we don't
    // need to add it here — only the Compose runtime classes.
    let _ = kotlinc;

    Some(ComposeAssets {
        plugin_jar: plugin_path,
        runtime_classpath: runtime_classes.display().to_string(),
    })
}

/// Locate the newest `kotlinx-coroutines-core-jvm-<ver>.jar` in the
/// user's Gradle cache. Returns the JAR path so fixture-gen can pass
/// it to kotlinc via `-cp` when compiling sources that import
/// `kotlinx.coroutines.*`. Returns `None` if the cache is missing or
/// no jar is present — caller should skip the kotlinc reference for
/// that fixture and emit a warning.
fn locate_coroutines_jar() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dir = home
        .join(".gradle/caches/modules-2/files-2.1")
        .join("org.jetbrains.kotlinx/kotlinx-coroutines-core-jvm");
    walkdir::WalkDir::new(&dir)
        .into_iter()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.starts_with("kotlinx-coroutines-core-jvm-") && name.ends_with(".jar")
        })
        .map(|e| e.path().to_path_buf())
        .max() // newest version path sorts last
}

fn tempdir(label: &str) -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!("skotch-xtask-{}-{}", label, std::process::id()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
