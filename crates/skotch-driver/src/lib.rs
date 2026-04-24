//! Library entry points for skotch's CLI subcommands.
//!
//! Splitting the driver out from `skotch-cli` lets a future `skotch-lsp`
//! server reuse all of compilation without dragging in `clap`. It
//! also lets integration tests call [`emit`] directly without
//! spawning a subprocess.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

use skotch_diagnostics::{render, Diagnostics};
use skotch_intern::Interner;
use skotch_lexer::lex;
use skotch_mir_lower::lower_file;
use skotch_parser::parse_file;
use skotch_resolve::resolve_file;
use skotch_span::SourceMap;
use skotch_typeck::type_check;

/// Selected output target for [`emit`].
///
/// Targets `Klib`, `Llvm`, and `Native` together implement the
/// kotlin-native-style multi-stage pipeline:
///
/// ```text
///     .kt source ──► MIR ──► .klib ──► LLVM IR ──► native binary
///                           ^         ^           ^
///                           │         │           │
///                       Klib stop  Llvm stop  Native stop
/// ```
///
/// Picking `Klib` writes a `.klib` and stops. `Llvm` runs the same
/// pipeline plus the LLVM IR conversion (which itself reads back the
/// klib). `Native` adds an additional `clang` link step at the end.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Jvm,
    Dex,
    Llvm,
    Klib,
    Native,
    Wasm,
}

impl Target {
    pub fn from_name(s: &str) -> Result<Target> {
        Ok(match s {
            "jvm" => Target::Jvm,
            "dex" => Target::Dex,
            "llvm" => Target::Llvm,
            "klib" => Target::Klib,
            "native" => Target::Native,
            "wasm" => Target::Wasm,
            other => return Err(anyhow!("unknown target `{other}`")),
        })
    }
}

/// Compile a single Kotlin source string to a [`MirModule`].
///
/// This is the shared front-end pipeline used by both `emit` (single-file)
/// and `skotch-build` (project-level builds). Returns the MIR plus the
/// interner so backends can look up strings.
///
/// When `package_symbols` is `Some`, cross-file declarations from the same
/// compilation unit are visible during resolution and lowering.
pub fn compile_source(
    source: &str,
    file_id: skotch_span::FileId,
    wrapper_class: &str,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    package_symbols: Option<&skotch_resolve::PackageSymbolTable>,
) -> skotch_mir::MirModule {
    let lexed = lex(file_id, source, diags);
    let ast = parse_file(&lexed, interner, diags);
    let resolved = resolve_file(&ast, interner, diags, package_symbols);
    let typed = type_check(&ast, &resolved, interner, diags, package_symbols);
    let mir = lower_file(
        &ast,
        &resolved,
        &typed,
        interner,
        diags,
        wrapper_class,
        package_symbols,
    );
    validate_mir(&mir, diags);
    mir
}

/// Compile a pre-parsed AST to a [`MirModule`]. Used by the build pipeline
/// which parses all files in Phase 1 (gather) and compiles in Phase 2.
pub fn compile_ast(
    ast: &skotch_syntax::KtFile,
    wrapper_class: &str,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    package_symbols: Option<&skotch_resolve::PackageSymbolTable>,
) -> skotch_mir::MirModule {
    let resolved = resolve_file(ast, interner, diags, package_symbols);
    let typed = type_check(ast, &resolved, interner, diags, package_symbols);
    let mir = lower_file(
        ast,
        &resolved,
        &typed,
        interner,
        diags,
        wrapper_class,
        package_symbols,
    );
    validate_mir(&mir, diags);
    mir
}

/// Run the MIR validation pass and report errors as diagnostics.
fn validate_mir(mir: &skotch_mir::MirModule, diags: &mut Diagnostics) {
    let errors = skotch_mir::validate::validate_module(mir);
    for e in &errors {
        diags.push(skotch_diagnostics::Diagnostic::error(
            skotch_span::Span::empty(skotch_span::FileId(0)),
            e.to_string(),
        ));
    }
}

/// Options accepted by [`emit`].
#[derive(Clone, Debug)]
pub struct EmitOptions {
    pub input: PathBuf,
    pub output: PathBuf,
    pub target: Target,
    /// Optional path to write the normalized form of the output. Used
    /// by `xtask gen-fixtures` and by tests that want a stable diff.
    pub norm_out: Option<PathBuf>,
}

/// Emit a single Kotlin source file to the requested target. The JVM
/// target is handled end-to-end; the others call into their respective
/// backend crates (some of which are still stubs).
pub fn emit(opts: &EmitOptions) -> Result<()> {
    emit_inner(opts, true)
}

/// Like [`emit`] but suppresses diagnostic output to stderr.
/// Used by the REPL for speculative compilation attempts that are
/// expected to fail (e.g. trying `getClass()` on a primitive).
pub fn emit_quiet(opts: &EmitOptions) -> Result<()> {
    emit_inner(opts, false)
}

fn emit_inner(opts: &EmitOptions, print_diags: bool) -> Result<()> {
    let source = std::fs::read_to_string(&opts.input)
        .with_context(|| format!("reading {}", opts.input.display()))?;

    let mut sm = SourceMap::new();
    let file_id = sm.add(opts.input.clone(), source.clone());

    let mut interner = Interner::new();
    let mut diags = Diagnostics::new();
    let lexed = lex(file_id, &source, &mut diags);
    let ast = parse_file(&lexed, &mut interner, &mut diags);
    let resolved = resolve_file(&ast, &mut interner, &mut diags, None);
    let typed = type_check(&ast, &resolved, &mut interner, &mut diags, None);

    let wrapper = wrapper_class_for(&opts.input);
    let mir = lower_file(
        &ast,
        &resolved,
        &typed,
        &mut interner,
        &mut diags,
        &wrapper,
        None,
    );

    if diags.has_errors() {
        if print_diags {
            eprint!("{}", render(&diags, &sm));
        }
        return Err(anyhow!("compilation failed with {} error(s)", diags.len()));
    }

    match opts.target {
        Target::Jvm => emit_jvm(&mir, &interner, opts)?,
        Target::Dex => emit_dex(&mir, opts)?,
        Target::Klib => emit_klib(&mir, opts)?,
        Target::Llvm => emit_llvm(&mir, opts)?,
        Target::Native => emit_native(&mir, opts)?,
        Target::Wasm => return Err(anyhow!("wasm target not yet implemented")),
    }

    // Drain any non-error diagnostics (warnings, notes).
    if print_diags && !diags.is_empty() {
        eprint!("{}", render(&diags, &sm));
    }
    Ok(())
}

fn emit_klib(mir: &skotch_mir::MirModule, opts: &EmitOptions) -> Result<()> {
    let bytes = skotch_backend_klib::write_klib(mir, skotch_backend_klib::DEFAULT_TARGET)?;
    if let Some(parent) = opts.output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&opts.output, &bytes)
        .with_context(|| format!("writing {}", opts.output.display()))?;
    if let Some(norm_path) = &opts.norm_out {
        // klib is a binary archive; the "normalized" form is just the
        // manifest text plus the embedded MIR JSON. We re-read the
        // klib and dump those two for diffing.
        let (m, manifest) = skotch_backend_klib::read_klib(&bytes)?;
        let mir_json =
            serde_json::to_string_pretty(&m).map_err(|e| anyhow!("re-serializing MIR: {e}"))?;
        let combined = format!(
            "--- manifest ---\n{}\n--- mir.json ---\n{mir_json}\n",
            manifest.to_text()
        );
        std::fs::write(norm_path, combined)
            .with_context(|| format!("writing {}", norm_path.display()))?;
    }
    Ok(())
}

/// Emit LLVM IR. Internally runs the multi-stage pipeline:
/// `MIR → klib → klib reader → LLVM IR`. The klib stage is exercised
/// in-process so that bugs in the klib serializer break the LLVM
/// target's tests too.
fn emit_llvm(mir: &skotch_mir::MirModule, opts: &EmitOptions) -> Result<()> {
    let klib_bytes = skotch_backend_klib::write_klib(mir, skotch_backend_klib::DEFAULT_TARGET)?;
    let llvm_text = skotch_backend_llvm::compile_klib(&klib_bytes)?;
    if let Some(parent) = opts.output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&opts.output, &llvm_text)
        .with_context(|| format!("writing {}", opts.output.display()))?;
    if let Some(norm_path) = &opts.norm_out {
        let normalized = skotch_llvm_norm::normalize(&llvm_text);
        std::fs::write(norm_path, normalized)
            .with_context(|| format!("writing {}", norm_path.display()))?;
    }
    Ok(())
}

/// Emit a native executable. Pipeline:
/// `MIR → klib → LLVM IR → clang → binary`. clang is the only
/// non-skotch tool skotch itself invokes — it's allowed because it's a
/// generic toolchain tool, not a Kotlin/Java/Android-specific tool.
fn emit_native(mir: &skotch_mir::MirModule, opts: &EmitOptions) -> Result<()> {
    let clang = which::which("clang")
        .map_err(|_| anyhow!("`clang` is not on PATH; install Xcode CLT or LLVM"))?;
    let klib_bytes = skotch_backend_klib::write_klib(mir, skotch_backend_klib::DEFAULT_TARGET)?;
    let llvm_text = skotch_backend_llvm::compile_klib(&klib_bytes)?;

    if let Some(parent) = opts.output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // Write the .ll alongside the binary so the user (and the test
    // harness) can inspect it. Use the binary path with `.ll` appended.
    let ll_path = opts.output.with_extension("ll");
    std::fs::write(&ll_path, &llvm_text)
        .with_context(|| format!("writing {}", ll_path.display()))?;

    let status = std::process::Command::new(&clang)
        .arg("-O0")
        .arg("-x")
        .arg("ir")
        .arg(&ll_path)
        .arg("-o")
        .arg(&opts.output)
        .status()
        .with_context(|| "invoking clang")?;
    if !status.success() {
        return Err(anyhow!("clang exited with status {status}"));
    }

    if let Some(norm_path) = &opts.norm_out {
        let normalized = skotch_llvm_norm::normalize(&llvm_text);
        std::fs::write(norm_path, normalized)
            .with_context(|| format!("writing {}", norm_path.display()))?;
    }
    Ok(())
}

fn emit_dex(mir: &skotch_mir::MirModule, opts: &EmitOptions) -> Result<()> {
    let bytes = skotch_backend_dex::compile_module(mir);
    if let Some(parent) = opts.output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&opts.output, &bytes)
        .with_context(|| format!("writing {}", opts.output.display()))?;
    if let Some(norm_path) = &opts.norm_out {
        let normalized = skotch_dex_norm::normalize_default(&bytes)
            .map_err(|e| anyhow!("normalizing emitted .dex: {e}"))?;
        std::fs::write(norm_path, &normalized)
            .with_context(|| format!("writing {}", norm_path.display()))?;
    }
    Ok(())
}

fn emit_jvm(mir: &skotch_mir::MirModule, interner: &Interner, opts: &EmitOptions) -> Result<()> {
    let bytes_list = skotch_backend_jvm::compile_module(mir, interner);
    let (_, bytes) = bytes_list
        .first()
        .ok_or_else(|| anyhow!("JVM backend produced no class files"))?;

    if let Some(parent) = opts.output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&opts.output, bytes)
        .with_context(|| format!("writing {}", opts.output.display()))?;

    // Write additional class files (user-defined classes) next to the main output.
    // When a package prefix is present, `name` may contain `/` separators
    // (e.g. `com/example/Greeter`), so we create intermediate directories.
    if let Some(parent) = opts.output.parent() {
        for (name, class_bytes) in bytes_list.iter().skip(1) {
            let path = parent.join(format!("{name}.class"));
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p).ok();
            }
            std::fs::write(&path, class_bytes)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    if let Some(norm_path) = &opts.norm_out {
        let normalized = skotch_classfile_norm::normalize_default(bytes)
            .map_err(|e| anyhow!("normalizing emitted .class: {e}"))?;
        std::fs::write(norm_path, normalized.as_text())
            .with_context(|| format!("writing {}", norm_path.display()))?;
    }
    Ok(())
}

/// Compute the kotlinc-convention wrapper class name from a source path.
/// `Hello.kt` → `HelloKt`. Used by both the JVM emitter and the test
/// runners that need to know the class to invoke.
pub fn wrapper_class_for(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Main");
    // Capitalize the first letter to follow Java class naming conventions.
    let mut chars = stem.chars();
    let head = chars.next().map(|c| c.to_ascii_uppercase());
    let tail: String = chars.collect();
    let head: String = head.into_iter().collect();
    format!("{head}{tail}Kt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_class_basic() {
        assert_eq!(wrapper_class_for(Path::new("Hello.kt")), "HelloKt");
        assert_eq!(
            wrapper_class_for(Path::new("foo/bar/Greeting.kt")),
            "GreetingKt"
        );
        assert_eq!(wrapper_class_for(Path::new("input.kt")), "InputKt");
    }
}
