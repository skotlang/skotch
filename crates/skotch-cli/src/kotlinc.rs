//! `skotch kotlinc` — drop-in emulation of the Kotlin/JVM compiler
//! command line (https://kotlinlang.org/docs/compiler-reference.html).
//!
//! This is also the dispatch target when `skotch` is invoked through a
//! `kotlinc` symlink via the multi-call binary mechanism.
//!
//! Only the Kotlin/JVM compiler options are recognised; Kotlin/Native
//! and Kotlin/JS options are out of scope (a kotlin/native or kotlin/js
//! flag triggers an "unsupported flag" warning, not an error, so build
//! scripts that pass them by habit still compile).

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

use skotch_diagnostics::{render, Diagnostics};
use skotch_driver::{compile_ast, wrapper_class_for};
use skotch_intern::Interner;
use skotch_lexer::lex;
use skotch_parser::parse_file;
use skotch_resolve::{gather_declarations, PackageSymbolTable};
use skotch_span::SourceMap;

/// Parsed kotlinc invocation.
#[derive(Debug, Default)]
pub struct KotlincOptions {
    /// `-d <dir|jar>` — output directory or jar file.
    pub output: Option<PathBuf>,
    /// `-classpath` / `-cp` — entries joined with the OS path separator.
    pub classpath: Vec<PathBuf>,
    /// `-include-runtime` — bundle kotlin-stdlib into the output jar.
    /// Only meaningful when `output` is a `.jar`.
    pub include_runtime: bool,
    /// `-script` — evaluate the source file as a script (.kts).
    pub script: bool,
    /// `-verbose` — chatty logging on stderr.
    pub verbose: bool,
    /// `-version` — print version and exit.
    pub version_only: bool,
    /// `-kotlin-home <path>` — path to the kotlin compiler install.
    /// Skotch does not need a kotlin install (it has its own backend),
    /// but the flag is accepted so build scripts can pass it.
    pub kotlin_home: Option<PathBuf>,
    /// Positional `.kt` / `.kts` source files (or directories).
    pub sources: Vec<PathBuf>,
}

/// Top-level entry point invoked from `main` for both `skotch kotlinc …`
/// and `kotlinc …` (multi-call binary form).
pub fn run(raw_args: &[String]) -> Result<()> {
    let opts = parse_args(raw_args)?;

    if opts.version_only {
        // kotlinc prints the version to stderr by convention.
        eprintln!("info: skotch-kotlinc {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if opts.verbose {
        eprintln!("verbose: {} source(s)", opts.sources.len());
        if let Some(d) = &opts.output {
            eprintln!("verbose: output -d {}", d.display());
        }
        if !opts.classpath.is_empty() {
            eprintln!("verbose: classpath ({} entries):", opts.classpath.len());
            for p in &opts.classpath {
                eprintln!("verbose:   {}", p.display());
            }
        }
        if let Some(kh) = &opts.kotlin_home {
            eprintln!("verbose: kotlin-home {}", kh.display());
        }
        if opts.include_runtime {
            eprintln!("verbose: -include-runtime is accepted but no-op (skotch is self-contained)");
        }
        if opts.script {
            eprintln!("verbose: script mode");
        }
    }

    // Surface the classpath to the rest of skotch by setting CLASSPATH
    // — that is how `skotch-classinfo` discovers external jars.
    if !opts.classpath.is_empty() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let joined = opts
            .classpath
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(sep);
        let merged = match std::env::var("CLASSPATH") {
            Ok(existing) if !existing.is_empty() => format!("{joined}{sep}{existing}"),
            _ => joined,
        };
        std::env::set_var("CLASSPATH", merged);
    }

    if opts.sources.is_empty() {
        return Err(anyhow!(
            "error: no source files specified (pass one or more `.kt` files)"
        ));
    }

    // Script mode just delegates to the existing `skotch run` path —
    // kotlinc -script source.kts runs the script under the bundled REPL.
    if opts.script {
        if opts.sources.len() != 1 {
            return Err(anyhow!(
                "error: -script accepts exactly one source file, got {}",
                opts.sources.len()
            ));
        }
        let captured = skotch_repl::run_script(&opts.sources[0])?;
        print!("{captured}");
        return Ok(());
    }

    // Multi-file compile path: same shape as the build pipeline's per-
    // module loop, just without the build.gradle.kts wrapper.
    let mut interner = Interner::new();
    let mut diags = Diagnostics::new();
    let mut sm = SourceMap::new();

    // Expand directories to the .kt files they contain (one level deep
    // for `.`, recursive for explicit dir args), filter out non-.kt
    // entries with a warning so a stray .java file in `srcDir` doesn't
    // silently get ignored.
    let source_files = collect_source_files(&opts.sources, opts.verbose)?;
    if source_files.is_empty() {
        return Err(anyhow!("error: no .kt files found in the given paths"));
    }

    let timing_enabled = std::env::var("SKOTCH_TIMING").is_ok();
    let mut t_lex_parse_ms: u128 = 0;
    let mut t_gather_ms: u128 = 0;
    let mut t_compile_ast_ms: u128 = 0;
    let mut t_compose_ms: u128 = 0;
    let mut t_backend_ms: u128 = 0;

    let mut parsed: Vec<(skotch_span::FileId, skotch_syntax::KtFile, String)> = Vec::new();
    for path in &source_files {
        let t0 = std::time::Instant::now();
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file_id = sm.add(path.clone(), text.clone());
        let lexed = lex(file_id, &text, &mut diags);
        let ast = parse_file(&lexed, &mut interner, &mut diags);
        let wrapper = wrapper_class_for(path);
        parsed.push((file_id, ast, wrapper));
        t_lex_parse_ms += t0.elapsed().as_millis();
    }

    // Gather declarations into a shared symbol table so cross-file refs
    // resolve during per-file compilation (kotlinc semantics).
    let refs: Vec<(skotch_span::FileId, &skotch_syntax::KtFile, &str)> = parsed
        .iter()
        .map(|(fid, ast, wc)| (*fid, ast, wc.as_str()))
        .collect();
    let t_gather = std::time::Instant::now();
    let combined_symbols: PackageSymbolTable = gather_declarations(&refs, &interner);
    t_gather_ms += t_gather.elapsed().as_millis();

    let mut all_classes: Vec<(String, Vec<u8>)> = Vec::new();
    for (_fid, ast, wrapper) in &parsed {
        let t_ast = std::time::Instant::now();
        let mut mir = compile_ast(
            ast,
            wrapper,
            &mut interner,
            &mut diags,
            Some(&combined_symbols),
        );
        t_compile_ast_ms += t_ast.elapsed().as_millis();
        if skotch_compose::has_composables(&mir) {
            let t_co = std::time::Instant::now();
            skotch_compose::compose_transform(&mut mir);
            t_compose_ms += t_co.elapsed().as_millis();
        }
        let t_be = std::time::Instant::now();
        let file_classes = skotch_backend_jvm::compile_module(&mir, &interner);
        t_backend_ms += t_be.elapsed().as_millis();
        all_classes.extend(file_classes);
    }

    if timing_enabled {
        eprintln!(
            "skotch-timing: lex+parse={t_lex_parse_ms}ms gather={t_gather_ms}ms \
             compile_ast={t_compile_ast_ms}ms compose={t_compose_ms}ms \
             backend={t_backend_ms}ms"
        );
    }

    if diags.has_errors() {
        eprint!("{}", render(&diags, &sm));
        return Err(anyhow!("compilation failed with errors"));
    }
    if !diags.is_empty() {
        eprint!("{}", render(&diags, &sm));
    }

    let out_dir = match &opts.output {
        Some(p) if p.extension().and_then(|s| s.to_str()) == Some("jar") => {
            // -include-runtime / jar packaging is not implemented yet.
            //
            // TODO: bundle classes into a .jar — for now, emit into a
            // directory named after the jar and warn.
            //
            // Reaching feature parity requires:
            //   1. zip the .class files (with package directories)
            //   2. add a META-INF/MANIFEST.MF
            //   3. when -include-runtime is set, copy in kotlin-stdlib
            //      and kotlinx-coroutines jars (locate via -kotlin-home).
            let dir = p.with_extension("classes");
            eprintln!(
                "warning: jar output not yet supported; writing class files to {}",
                dir.display()
            );
            dir
        }
        Some(p) => p.clone(),
        None => PathBuf::from("."),
    };

    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating output dir {}", out_dir.display()))?;

    for (name, bytes) in &all_classes {
        // `name` is a JVM internal name (slashes) so package directories
        // come for free; just append `.class` and ensure the parent dir
        // exists.
        let rel = PathBuf::from(format!("{name}.class"));
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    }

    if opts.verbose {
        eprintln!(
            "verbose: wrote {} class file(s) under {}",
            all_classes.len(),
            out_dir.display()
        );
    }

    Ok(())
}

/// Expand any directory arguments into the `.kt` files they contain so
/// `skotch kotlinc -d out/ src/` works the same as listing the files
/// explicitly. `.kts` is accepted only when `-script` was passed (filtered
/// upstream); here we keep `.kt` + `.kts`.
fn collect_source_files(roots: &[PathBuf], verbose: bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for root in roots {
        if root.is_file() {
            out.push(root.clone());
        } else if root.is_dir() {
            walk_dir(root, &mut out)?;
        } else {
            return Err(anyhow!(
                "error: source path does not exist: {}",
                root.display()
            ));
        }
    }
    out.retain(|p| {
        let kept = matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("kt") | Some("kts")
        );
        if !kept && verbose {
            eprintln!("verbose: skipping non-Kotlin file {}", p.display());
        }
        kept
    });
    // Compile order: alphabetical, but any file named `Main.kt` (or
    // anything with a `main` function — caller convention) goes LAST.
    // The cross-file resolution path relies on declarations being
    // gathered before they're referenced; this matches what the
    // build pipeline does via topological sort.
    out.sort_by(|a, b| {
        let a_main = a.file_name().and_then(|s| s.to_str()) == Some("Main.kt");
        let b_main = b.file_name().and_then(|s| s.to_str()) == Some("Main.kt");
        match (a_main, b_main) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => a.cmp(b),
        }
    });
    Ok(out)
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Hand-parse kotlinc's flag syntax. kotlinc uses single-dash long
/// options (e.g. `-classpath`) which `clap` cannot model without
/// surprising every other subcommand, so this is bespoke.
fn parse_args(raw: &[String]) -> Result<KotlincOptions> {
    let mut opts = KotlincOptions::default();
    let mut i = 0;
    while i < raw.len() {
        let a = &raw[i];
        match a.as_str() {
            "-d" => {
                i += 1;
                let v = raw.get(i).ok_or_else(|| anyhow!("-d requires a path"))?;
                opts.output = Some(PathBuf::from(v));
            }
            "-classpath" | "-cp" => {
                i += 1;
                let v = raw
                    .get(i)
                    .ok_or_else(|| anyhow!("{a} requires a classpath"))?;
                let sep = if cfg!(windows) { ';' } else { ':' };
                for p in v.split(sep) {
                    if !p.is_empty() {
                        opts.classpath.push(PathBuf::from(p));
                    }
                }
            }
            "-include-runtime" => opts.include_runtime = true,
            "-script" => opts.script = true,
            "-verbose" => opts.verbose = true,
            "-version" => opts.version_only = true,
            "-kotlin-home" => {
                i += 1;
                let v = raw
                    .get(i)
                    .ok_or_else(|| anyhow!("-kotlin-home requires a path"))?;
                opts.kotlin_home = Some(PathBuf::from(v));
            }
            "-help" | "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            // ── Common JVM flags we accept-and-warn ────────────────────
            // These are recognised by upstream build scripts but skotch
            // doesn't implement them yet. Returning an error would
            // break Gradle invocations; emitting a warning lets the
            // build proceed and surfaces what's missing.
            other if is_warn_only(other) => {
                // Some flags take a value (e.g. `-jvm-target 17`),
                // some don't. To keep parsing robust without a per-
                // flag arity table, we only consume the value when
                // the next token doesn't itself start with `-`. This
                // matches kotlinc's own permissive handling.
                let arity = warn_only_arity(other);
                let value = if arity == 1 {
                    raw.get(i + 1)
                        .filter(|t| !t.starts_with('-'))
                        .map(String::as_str)
                } else {
                    None
                };
                if let Some(v) = value {
                    eprintln!("warning: option {a}={v} is accepted but not implemented yet");
                    i += 1;
                } else {
                    eprintln!("warning: option {a} is accepted but not implemented yet");
                }
            }
            // Unknown `-X` advanced flag — warn instead of erroring so
            // builds that pass implementation-specific tuning options
            // (e.g. `-Xjsr305=strict`) still get through.
            other if other.starts_with("-X") || other.starts_with("-P") => {
                eprintln!("warning: unsupported advanced option {a}");
            }
            other if other.starts_with('-') => {
                return Err(anyhow!("error: unknown option {a}"));
            }
            // Positional: source file or directory.
            _ => opts.sources.push(PathBuf::from(a)),
        }
        i += 1;
    }
    Ok(opts)
}

/// Kotlin/JVM compiler options recognised by kotlinc that skotch does
/// not (yet) implement. Each entry includes a short note describing
/// what would be needed to support it.
fn is_warn_only(flag: &str) -> bool {
    matches!(
        flag,
        // ── Diagnostics + warnings ──────────────────────────────
        // To implement -Werror: promote any warning in `Diagnostics`
        // to an error before the `has_errors()` check.
        "-Werror" |
        // To implement -nowarn: filter warnings from `Diagnostics`
        // before rendering.
        "-nowarn" |
        // To implement -progressive: switch behavior on language-
        // version-specific warnings that became errors in newer
        // versions.
        "-progressive" |
        // To implement -suppress-version-warnings: silence the
        // version-mismatch warning kotlinc emits when the language
        // and api versions disagree.
        "-suppress-version-warnings" |
        "-Xsuppress-version-warnings" |

        // ── Module / output shape ───────────────────────────────
        // To implement -module-name <name>: thread the name through
        // the `@Metadata` annotation written by the backend so
        // reflection-based callers see the correct module.
        "-module-name" |
        // To implement -jvm-target <ver>: pass the target class
        // file major version through to `skotch-backend-jvm`'s
        // class-file writer.
        "-jvm-target" |
        // To implement -api-version / -language-version: gate
        // parser / mir-lower behaviour on the requested version
        // (e.g. when stdlib signatures change between Kotlin
        // releases).
        "-api-version" |
        "-language-version" |
        // To implement -opt-in: thread the opted-in annotations
        // into the typeck pass so it suppresses the corresponding
        // experimental-API warnings.
        "-opt-in" |
        // To implement -explicit-api: have typeck require explicit
        // visibility on every public-API declaration.
        "-explicit-api" |
        // To implement -script-templates: register additional
        // .kts.kt script templates beyond the default.
        "-script-templates" |
        // To implement -P: thread plugin options into the relevant
        // compiler plugin (the only one skotch has built-in today
        // is Compose, which auto-detects).
        "-P" |

        // ── Compile / link toggles ──────────────────────────────
        // To implement -no-stdlib: skip the implicit stdlib lookup
        // in `skotch-classinfo` (typeck would then warn on every
        // `kotlin.*` reference).
        "-no-stdlib" |
        // To implement -no-reflect: skip kotlin-reflect from the
        // implicit classpath when emitting `-include-runtime`.
        "-no-reflect" |
        // To implement -no-jdk: don't auto-add the JDK to the
        // compile classpath (we already only consume what's on
        // CLASSPATH, so this is largely a no-op once -classpath
        // works fully).
        "-no-jdk" |
        // To implement -jdk-home: point JDK-class lookups
        // (`java.lang.String` &c.) at the named install instead
        // of the running JVM's default.
        "-jdk-home" |
        // To implement -friend-paths: allow `internal` access
        // across the named module boundary during resolve.
        "-friend-paths" |
        // To implement -Xjsr305=…: thread the JSR-305 enforcement
        // mode into typeck so platform-type nullability is
        // diagnosed accordingly.
        "-Xjsr305"
    )
}

/// Number of follow-on tokens a warn-only flag consumes (0 or 1). Used
/// when parsing to avoid swallowing the next real flag as a value.
fn warn_only_arity(flag: &str) -> usize {
    match flag {
        "-module-name" | "-jvm-target" | "-api-version" | "-language-version" | "-opt-in"
        | "-script-templates" | "-jdk-home" | "-friend-paths" | "-P" | "-Xjsr305" => 1,
        _ => 0,
    }
}

/// Help text shown both by `skotch kotlinc -help` and by clap when
/// it generates `--help` for the `Kotlinc` subcommand.
///
/// Exposed as a single `&'static str` so main.rs's clap attribute and
/// this module's `print_help` share the same source of truth — adding
/// a flag means editing one constant.
pub const HELP_TEXT: &str = "\
skotch kotlinc — drop-in emulation of the Kotlin/JVM compiler.

Compiles one or more `.kt` files using skotch's own front-end and JVM
backend, exposing a kotlinc-compatible command line. Also reached via
the `kotlinc` multi-call alias — symlink the `skotch` binary to
`kotlinc` and invoke it directly.

USAGE:
    skotch kotlinc [OPTIONS] <SOURCE_FILES...>

OPTIONS:
    -d <path>            Destination for class files (directory or .jar)
    -classpath, -cp      Colon-separated classpath entries
    -include-runtime     Include kotlin-stdlib in the output jar
                         (accepted; jar packaging is not yet implemented)
    -script              Evaluate <SOURCE_FILE> as a `.kts` script
    -verbose             Chatty logging on stderr
    -version             Print the compiler version and exit
    -kotlin-home <path>  Path to a kotlin install (informational only)
    -help, -h, --help    Print this help and exit

RECOGNISED-BUT-UNIMPLEMENTED FLAGS:
    The following kotlinc flags are accepted (so build scripts that
    pass them keep working) but emit a warning and are otherwise
    ignored. Each entry's comment in `crates/skotch-cli/src/kotlinc.rs`
    notes what would be needed to support it:

      -Werror, -nowarn, -progressive, -suppress-version-warnings,
      -Xsuppress-version-warnings, -module-name, -jvm-target,
      -api-version, -language-version, -opt-in, -explicit-api,
      -script-templates, -P, -no-stdlib, -no-reflect, -no-jdk,
      -jdk-home, -friend-paths, -Xjsr305 (and any other `-X…`/`-P…`)

EXAMPLES:
    # Compile two files into out/:
    skotch kotlinc -d out/ Main.kt Util.kt

    # With an external dependency on the classpath:
    skotch kotlinc -cp libs/coroutines.jar -d out/ Main.kt

    # Run a .kts script:
    skotch kotlinc -script setup.kts
";

fn print_help() {
    println!("{HELP_TEXT}");
}
