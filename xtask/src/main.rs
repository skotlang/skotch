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
//! The shipping `skot` binary must contain none of the strings
//! `kotlinc`, `javac`, `d8`, or `dx`. A test in
//! `tests/no_external_compiler.rs` enforces this.
//!
//! ## Subcommands
//!
//! - `cargo xtask gen-fixtures --target jvm` — for each fixture
//!   directory under `tests/fixtures/inputs/` whose `meta.toml` says
//!   `status = "supported"`:
//!     1. Run `skot emit --target jvm` and write `expected/jvm/<f>/skot.class`
//!        + `expected/jvm/<f>/skot.norm.txt`.
//!     2. If `kotlinc` is on `PATH`, run it and write
//!        `expected/jvm/<f>/kotlinc.class` + `kotlinc.norm.txt`.
//!     3. If `java` is on `PATH`, execute the kotlinc-compiled class
//!        and capture its stdout as `expected/jvm/<f>/run.stdout`.
//!
//! - `cargo xtask refresh-skot-goldens` — same as above but skip
//!   reference tools entirely; just refresh skot's own outputs.
//!
//! - `cargo xtask verify` — re-run skot on each fixture and assert
//!   the output is byte-equal to the committed goldens. This is what
//!   the workspace tests do too, but `xtask verify` is convenient
//!   when you've just edited the JVM backend and want a quick check.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::Command;

use skot_driver::{emit, EmitOptions, Target as SkotTarget};

#[derive(Parser, Debug)]
#[command(name = "xtask", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Sub,
}

#[derive(Subcommand, Debug)]
enum Sub {
    /// Regenerate committed fixture outputs (skot's + reference tools').
    GenFixtures {
        #[arg(long, value_enum, default_value_t = TargetArg::Jvm)]
        target: TargetArg,
        /// Only regenerate this one fixture (by directory name).
        #[arg(long)]
        fixture: Option<String>,
        /// Skip running reference tools (kotlinc/d8/kotlinc-native).
        #[arg(long)]
        skot_only: bool,
    },
    /// Regenerate skot's own goldens without invoking reference tools.
    RefreshSkotGoldens {
        #[arg(long, value_enum, default_value_t = TargetArg::Jvm)]
        target: TargetArg,
    },
    /// Re-run skot on each fixture and verify output matches committed goldens.
    Verify {
        #[arg(long, value_enum, default_value_t = TargetArg::Jvm)]
        target: TargetArg,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum TargetArg {
    Jvm,
    Dex,
    Llvm,
    Klib,
    Native,
}

impl From<TargetArg> for SkotTarget {
    fn from(t: TargetArg) -> Self {
        match t {
            TargetArg::Jvm => SkotTarget::Jvm,
            TargetArg::Dex => SkotTarget::Dex,
            TargetArg::Llvm => SkotTarget::Llvm,
            TargetArg::Klib => SkotTarget::Klib,
            TargetArg::Native => SkotTarget::Native,
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
            skot_only,
        } => gen_fixtures(&workspace, target, fixture.as_deref(), skot_only),
        Sub::RefreshSkotGoldens { target } => gen_fixtures(&workspace, target, None, true),
        Sub::Verify { target } => verify(&workspace, target),
    }
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
    skot_only: bool,
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
    if !skot_only {
        if kotlinc.is_none() {
            eprintln!("warning: kotlinc not found on PATH; skipping reference outputs");
        }
        if java.is_none() {
            eprintln!("warning: java not found on PATH; skipping reference run.stdout");
        }
        if matches!(target, TargetArg::Dex) && d8.is_none() {
            eprintln!(
                "warning: d8 not found at /Users/marc/Library/Android/sdk/build-tools/*/d8 \
                 or on PATH; skipping dex reference outputs"
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
            TargetArg::Jvm => gen_one_jvm(&f, workspace, &kotlinc, &java, skot_only)?,
            TargetArg::Dex => gen_one_dex(&f, workspace, &kotlinc, &d8, skot_only)?,
            TargetArg::Klib => gen_one_klib(&f, workspace, &kotlinc_native, skot_only)?,
            TargetArg::Llvm => gen_one_llvm(&f, workspace, &kotlinc_native, &clang, skot_only)?,
            TargetArg::Native => gen_one_native(&f, workspace, &kotlinc_native, skot_only)?,
        }
    }
    Ok(())
}

/// Locate `d8`, preferring the latest Android SDK build-tools install.
fn locate_d8() -> Option<PathBuf> {
    // First check the user's known SDK location.
    let sdk_root = PathBuf::from("/Users/marc/Library/Android/sdk/build-tools");
    if let Ok(read_dir) = std::fs::read_dir(&sdk_root) {
        let mut versions: Vec<PathBuf> = read_dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("d8").exists())
            .collect();
        // Sort by directory name (version), descending.
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
    skot_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/jvm")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    // 1) skot's own outputs (skot.class + skot.norm.txt).
    println!("[skot]   {}", f.dir_name);
    let skot_class = expected.join("skot.class");
    let skot_norm = expected.join("skot.norm.txt");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skot_class.clone(),
        target: SkotTarget::Jvm,
        norm_out: Some(skot_norm.clone()),
    })
    .with_context(|| format!("skot emit on {}", f.dir_name))?;

    if skot_only {
        return Ok(());
    }

    // 2) kotlinc reference, if available.
    if let Some(kc) = kotlinc {
        println!("[kotlinc]{}", f.dir_name);
        let tmp = tempdir(&f.dir_name)?;
        let status = Command::new(kc)
            .arg(&f.input)
            .arg("-d")
            .arg(&tmp)
            .arg("-jvm-target")
            .arg("17")
            .status()
            .with_context(|| format!("running kotlinc on {}", f.dir_name))?;
        if !status.success() {
            eprintln!("  kotlinc failed; skipping reference for {}", f.dir_name);
        } else {
            // Find the produced .class file.
            let mut class_path = None;
            for e in walkdir::WalkDir::new(&tmp) {
                let e = e?;
                if e.path().extension().and_then(|s| s.to_str()) == Some("class") {
                    class_path = Some(e.path().to_path_buf());
                    break;
                }
            }
            if let Some(cp) = class_path {
                let bytes = std::fs::read(&cp)?;
                std::fs::write(expected.join("kotlinc.class"), &bytes)?;
                let normalized = skot_classfile_norm::normalize_default(&bytes)
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
                    // when running the reference. The skot binary
                    // never does this — only this xtask does, and
                    // only when capturing reference run.stdout.
                    let mut cp_arg = class_dir.as_os_str().to_os_string();
                    if let Some(stdlib) = locate_kotlin_stdlib(kc) {
                        cp_arg.push(":");
                        cp_arg.push(stdlib);
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
/// 1. Run `skot emit --target dex` to produce `skot.dex` + `skot.norm.txt`.
/// 2. If `kotlinc` is available, run it on the input to produce a
///    `.class`, then run `d8` on that `.class` to produce a reference
///    `d8.dex` + `d8.norm.txt`. The d8 reference is committed alongside
///    skot's so test failures can show both.
fn gen_one_dex(
    f: &Fixture,
    workspace: &Path,
    kotlinc: &Option<PathBuf>,
    d8: &Option<PathBuf>,
    skot_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/dex")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    // 1) skot's outputs.
    println!("[skot]   {}", f.dir_name);
    let skot_dex = expected.join("skot.dex");
    let skot_norm = expected.join("skot.norm.txt");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skot_dex.clone(),
        target: SkotTarget::Dex,
        norm_out: Some(skot_norm.clone()),
    })
    .with_context(|| format!("skot emit --target dex on {}", f.dir_name))?;

    if skot_only {
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
    let normalized = skot_dex_norm::normalize_default(&bytes)
        .map_err(|e| anyhow!("normalizing d8 output: {e}"))?;
    std::fs::write(expected.join("d8.norm.txt"), &normalized)?;
    Ok(())
}

/// Generate one fixture's `--target klib` outputs:
///
/// 1. `skot emit --target klib` → `expected/klib/<f>/skot.klib` and
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
    skot_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/klib")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    println!("[skot]   {}", f.dir_name);
    let skot_klib = expected.join("skot.klib");
    let skot_norm = expected.join("skot.norm.txt");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skot_klib.clone(),
        target: SkotTarget::Klib,
        norm_out: Some(skot_norm),
    })
    .with_context(|| format!("skot emit --target klib on {}", f.dir_name))?;

    if skot_only {
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
/// 1. `skot emit --target llvm` → `expected/llvm/<f>/skot.ll` and
///    `skot.norm.txt`.
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
    skot_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/llvm")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    println!("[skot]   {}", f.dir_name);
    let skot_ll = expected.join("skot.ll");
    let skot_norm = expected.join("skot.norm.txt");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skot_ll.clone(),
        target: SkotTarget::Llvm,
        norm_out: Some(skot_norm),
    })
    .with_context(|| format!("skot emit --target llvm on {}", f.dir_name))?;

    if skot_only {
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
/// 1. `skot emit --target native` → `expected/native/<f>/skot` (the
///    binary) and `expected/native/<f>/skot.ll` (the IR that fed it).
/// 2. Run skot's binary and capture stdout into
///    `expected/native/<f>/run.stdout`. This is what the
///    behavioral test diffs against.
/// 3. If `kotlinc-native` is available, run it with `-p program` to
///    produce a reference binary, run that and capture
///    `kotlinc-native.run.stdout` for cross-compiler comparison.
fn gen_one_native(
    f: &Fixture,
    workspace: &Path,
    kotlinc_native: &Option<PathBuf>,
    skot_only: bool,
) -> Result<()> {
    let expected = workspace
        .join("tests/fixtures/expected/native")
        .join(&f.dir_name);
    std::fs::create_dir_all(&expected).ok();

    println!("[skot]   {}", f.dir_name);
    let skot_bin = expected.join("skot");
    emit(&EmitOptions {
        input: f.input.clone(),
        output: skot_bin.clone(),
        target: SkotTarget::Native,
        norm_out: None,
    })
    .with_context(|| format!("skot emit --target native on {}", f.dir_name))?;

    let out = Command::new(&skot_bin)
        .output()
        .with_context(|| format!("running skot binary {}", skot_bin.display()))?;
    if out.status.success() {
        std::fs::write(expected.join("run.stdout"), &out.stdout)?;
    }

    if skot_only {
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
    let skot_target = SkotTarget::from(target);
    let inputs = workspace.join("tests/fixtures/inputs");
    let fixtures = list_supported_fixtures(&inputs)?;
    let mut bad = 0;
    for f in &fixtures {
        let expected_dir = workspace
            .join("tests/fixtures/expected")
            .join(subdir)
            .join(&f.dir_name);
        let golden = expected_dir.join(format!("skot.{ext}"));
        if !golden.exists() {
            eprintln!("MISSING: {}", f.dir_name);
            bad += 1;
            continue;
        }
        // Re-run skot, write to a temp path, byte-compare.
        let tmp_dir = tempdir(&f.dir_name)?;
        let out = tmp_dir.join(format!("out.{ext}"));
        emit(&EmitOptions {
            input: f.input.clone(),
            output: out.clone(),
            target: skot_target,
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

fn tempdir(label: &str) -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!("skot-xtask-{}-{}", label, std::process::id()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
