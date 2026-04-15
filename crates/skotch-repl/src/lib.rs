//! Interactive REPL and `.kts` script runner for skotch.
//!
//! Both modes share the same backend pipeline:
//!
//! 1. Wrap the user's source in a synthetic `fun main() { … }`
//!    (REPL: built from accumulated history; script: the `.kts`
//!    file's whole content).
//! 2. Hand it to `skotch-driver`'s JVM target to compile a `.class`
//!    file into a temp directory.
//! 3. Spawn `java -cp <tmp> <ClassName>` from `JAVA_HOME` (falling
//!    back to `which java`) and capture its stdout.
//! 4. Print or return the captured output.
//!
//! ## Line editing
//!
//! The interactive REPL uses the **reedline** crate for line editing
//! and command history. This gives us:
//!
//! - Arrow-key navigation within a line
//! - Up/Down history browsal
//! - Ctrl-R reverse-search through history
//! - Ctrl-C to abort the current line
//! - Ctrl-D on an empty line to exit
//! - **Tab completion** for REPL commands, Kotlin keywords, locally
//!   defined variables/classes/functions, and member access on typed
//!   variables via live JVM reflection
//!
//! ## Stateful REPL
//!
//! The REPL accumulates a per-turn history of *top-level
//! declarations* (`val`, `var`, `fun`). Each time the user types
//! one, it's appended to a `Vec<String>` and parsed by recompiling
//! the whole accumulated source so any syntax error surfaces at the
//! turn that introduced it. Expression statements are *not* added
//! to the history — they're wrapped in a fresh `fun main() { … }`
//! along with all prior declarations and executed once.
//!
//! ## Script runner
//!
//! `.kts` files are read whole, wrapped in `fun main() { … }`, and
//! sent through the same pipeline. For PR scope a `.kts` file may
//! contain only top-level statements and `val`/`var` declarations
//! (which become locals inside the synthetic `main`). Top-level
//! `fun` declarations inside `.kts` files are not yet supported
//! because they would need to be lifted out of the synthetic main.
//!
//! ## Locating `java`
//!
//! Resolution order:
//!
//! 1. `$JAVA_HOME/bin/java[.exe]`
//! 2. `which java` on `PATH`
//! 3. `None` — caller falls back to a clear error

mod completion;
pub mod highlight;

use anyhow::{anyhow, Context, Result};
use skotch_jvm::EmbeddedJvm;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use skotch_diagnostics::Diagnostics;
use skotch_driver::{emit, EmitOptions, Target};
use skotch_intern::Interner;
use skotch_span::FileId;
use skotch_syntax::Decl;

pub use completion::ScanMode;
use completion::{expand_tilde, scan_dir_classes, CompletionCtx, SkotchCompleter};

/// Load `kotlin-stdlib.jar` onto the embedded JVM's classpath.
///
/// Uses `skotch_classinfo::find_kotlin_lib_dir()` to locate the Kotlin
/// stdlib, then adds it via `add_jar_to_classpath`. This is called once
/// per JVM initialization so that `kotlin.collections.CollectionsKt.listOf`
/// and other real stdlib functions are available at runtime.
fn load_kotlin_stdlib(jvm: &EmbeddedJvm) {
    match skotch_classinfo::find_kotlin_lib_dir() {
        Ok(lib_dir) => {
            let stdlib = lib_dir.join("kotlin-stdlib.jar");
            if stdlib.exists() {
                // Best effort: on Java 9+ the system class loader is
                // not a URLClassLoader, so `add_jar_to_classpath`
                // will fail. That's fine — `skotch-jvm::create_jvm`
                // already puts kotlin-stdlib.jar on the JVM's
                // startup classpath via `-Djava.class.path=`, so no
                // runtime injection is required.
                let _ = jvm.add_jar_to_classpath(&stdlib);
            } else {
                eprintln!(
                    "  warning: kotlin-stdlib.jar not found at {}",
                    stdlib.display()
                );
            }
        }
        Err(e) => {
            eprintln!(
                "  warning: could not locate kotlin-stdlib.jar: {e}\n  \
                 hint: install Kotlin (brew install kotlin) or set KOTLIN_HOME"
            );
        }
    }
}

// ─── public entry points ─────────────────────────────────────────────────

/// Run the interactive REPL with **reedline** line editing.
///
/// This is the entry point that `skotch repl` uses when stdin is a
/// terminal. It gives the user arrow-key editing, history, and
/// Ctrl-R search. The function returns when the user types `:quit`,
/// `:exit`, or Ctrl-D on an empty prompt.
///
/// The reedline `Editor` owns stdin/stdout internally, so this
/// function does not take I/O parameters. For piped/test input use
/// [`run_repl`] instead, which takes generic `BufRead`/`Write`
/// streams.
pub fn run_repl_interactive_default() -> Result<()> {
    run_repl_interactive(ScanMode::Background)
}

pub fn run_repl_interactive(scan_mode: ScanMode) -> Result<()> {
    use reedline::{
        default_emacs_keybindings, ColumnarMenu, DefaultPrompt, DefaultPromptSegment, EditCommand,
        Emacs, FileBackedHistory, KeyCode, KeyModifiers, MenuBuilder, Reedline, ReedlineEvent,
        ReedlineMenu, Signal,
    };

    let jvm = EmbeddedJvm::new()?;
    load_kotlin_stdlib(&jvm);

    // ── shared completion state ─────────────────────────────────────
    let completion_ctx = Arc::new(Mutex::new(CompletionCtx::new()));

    // ── classpath scan (mode-dependent) ─────────────────────────────
    match scan_mode {
        ScanMode::Eager => {
            let t0 = std::time::Instant::now();
            match jvm.scan_system_classes() {
                Ok(classes) => {
                    let count = classes.len();
                    let secs = t0.elapsed().as_secs_f64();
                    let mut ctx = completion_ctx.lock().unwrap();
                    ctx.add_extra_classes(classes);
                    drop(ctx);
                    eprintln!("  classpath: {count} system classes indexed ({secs:.1}s)");
                }
                Err(e) => eprintln!("  classpath scan failed: {e}"),
            }
        }
        ScanMode::Background => {
            let ctx_bg = Arc::clone(&completion_ctx);
            std::thread::Builder::new()
                .name("scanlib".into())
                .spawn(move || {
                    if let Ok(bg_jvm) = EmbeddedJvm::new() {
                        if let Ok(classes) = bg_jvm.scan_system_classes() {
                            let mut ctx = ctx_bg.lock().unwrap();
                            ctx.add_extra_classes(classes);
                        }
                    }
                })
                .ok();
        }
        ScanMode::Lazy => {
            completion_ctx.lock().unwrap().lazy_scan_pending = true;
        }
        ScanMode::None => {}
    }

    // ── reedline setup ──────────────────────────────────────────────
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('l'),
        ReedlineEvent::Edit(vec![EditCommand::Clear]),
    );
    // Tab opens the completion menu (or cycles to the next item).
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    // Shift-Tab cycles backwards.
    keybindings.add_binding(
        KeyModifiers::SHIFT,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );
    let edit_mode = Box::new(Emacs::new(keybindings));

    // Tab completer backed by REPL state + JVM reflection.
    let completer = Box::new(SkotchCompleter {
        ctx: Arc::clone(&completion_ctx),
    });

    // Columnar completion menu (idiomatic 4-column layout).
    let completion_menu = Box::new(
        ColumnarMenu::default()
            .with_name("completion_menu")
            .with_columns(4)
            .with_column_padding(2),
    );

    // Persistent history across REPL sessions (~/.skotch/repl_history).
    let history_path = history_path();
    let history: Box<FileBackedHistory> = Box::new(
        FileBackedHistory::with_file(1000, history_path.clone())
            .or_else(|_| FileBackedHistory::new(1000))
            .expect("failed to create history"),
    );

    // Syntax highlighter backed by the real skotch lexer.
    let highlighter = Box::new(highlight::SkotchHighlighter);

    let mut editor = Reedline::create()
        .with_edit_mode(edit_mode)
        .with_completer(completer)
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_history(history)
        .with_highlighter(highlighter);

    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("\x1b[35mskotch\x1b[0m".to_string()),
        DefaultPromptSegment::Empty,
    );

    let debug_star = if cfg!(debug_assertions) { "*" } else { "" };
    let jvm_ver = jvm.java_version().unwrap_or_else(|_| "?".into());
    println!(
        "skotch repl {} (jvm {jvm_ver}){debug_star} — type :help for commands, :quit to exit",
        env!("CARGO_PKG_VERSION")
    );
    if history_path.exists() {
        eprintln!("  history: {}", history_path.display());
    }

    let mut state = ReplState::new();

    loop {
        let sig = editor.read_line(&prompt);
        match sig {
            Ok(Signal::Success(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // ── REPL commands (colon-prefixed) ──────────────
                if let Some(cmd) = trimmed.strip_prefix(':') {
                    let (cmd_name, cmd_arg) = cmd
                        .split_once(' ')
                        .map(|(n, a)| (n, a.trim()))
                        .unwrap_or((cmd, ""));

                    match cmd_name {
                        "quit" | "exit" | "q" => {
                            println!("bye");
                            return Ok(());
                        }
                        "help" | "h" | "?" => {
                            println!("  :quit / :q     — exit the REPL");
                            println!("  :help / :?     — show this help");
                            println!("  :history       — show accumulated declarations");
                            println!("  :reset         — clear all declarations");
                            println!("  :cpadd <path>  — add JAR or directory to classpath");
                            println!("  :cplist        — list classpath entries");
                            println!("  :type <expr>   — show the inferred type of an expression");
                            println!("  <kotlin>       — compile and run");
                            println!();
                            println!("  Tab            — complete identifier or member");
                            println!("  Shift-Tab      — previous completion");
                            println!("  Up/Down        — navigate history");
                            println!("  Ctrl-R         — reverse history search");
                            println!("  Ctrl-L         — clear screen");
                            println!("  Ctrl-D         — exit");
                        }
                        "history" | "hist" => {
                            let all_decls: Vec<&str> = state
                                .top_decls
                                .iter()
                                .chain(state.local_decls.iter())
                                .map(|s| s.as_str())
                                .collect();
                            if all_decls.is_empty() {
                                println!("(no declarations)");
                            } else {
                                for (i, d) in all_decls.iter().enumerate() {
                                    println!("  {}: {d}", i + 1);
                                }
                            }
                        }
                        "reset" | "clear" => {
                            state.reset();
                            state.sync_completions(&completion_ctx);
                            println!("(state cleared)");
                        }
                        "cpadd" => {
                            if cmd_arg.is_empty() {
                                eprintln!("usage: :cpadd <jar-or-directory>");
                            } else {
                                handle_cpadd(
                                    cmd_arg,
                                    &jvm,
                                    &mut state,
                                    Some(&completion_ctx),
                                    scan_mode,
                                );
                            }
                        }
                        "cplist" => {
                            handle_cplist(&state);
                        }
                        other => {
                            eprintln!("unknown command :{other} — type :help for options");
                        }
                    }
                    continue;
                }

                // ── Kotlin code ─────────────────────────────────
                match state.process(&line, &jvm) {
                    Ok(stdout) => {
                        state.sync_completions(&completion_ctx);
                        if !stdout.is_empty() {
                            print!("{stdout}");
                            if !stdout.ends_with('\n') {
                                println!();
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {e:#}");
                    }
                }
            }
            Ok(Signal::CtrlD) => {
                println!("bye");
                return Ok(());
            }
            Ok(Signal::CtrlC) => continue,
            Err(e) => {
                eprintln!("reedline error: {e}");
                return Err(e.into());
            }
        }
    }
}

/// Handle `:cpadd <path>` — add a JAR or directory to the classpath.
///
/// The class-name scan of the new entry follows the active `ScanMode`:
/// eager/lazy/background behave the same way as at startup; `None`
/// skips scanning entirely.
fn handle_cpadd(
    path_str: &str,
    jvm: &EmbeddedJvm,
    state: &mut ReplState,
    ctx: Option<&Arc<Mutex<CompletionCtx>>>,
    scan_mode: ScanMode,
) {
    let path = expand_tilde(path_str);
    if !path.exists() {
        eprintln!("error: path not found: {}", path.display());
        return;
    }

    if let Err(e) = jvm.add_jar_to_classpath(&path) {
        eprintln!("warning: {e}");
    }
    state.classpath.push(path.clone());

    let kind = if path.is_dir() { "directory" } else { "JAR" };

    if scan_mode == ScanMode::None {
        println!("added {kind}: {}", path.display());
        return;
    }

    let scan_entry = |p: &std::path::Path| -> Vec<String> {
        if p.is_file() {
            jvm.scan_jar_classes(p).unwrap_or_default()
        } else {
            scan_dir_classes(p)
        }
    };

    match scan_mode {
        ScanMode::Eager | ScanMode::Lazy => {
            let t0 = std::time::Instant::now();
            let classes = scan_entry(&path);
            let count = classes.len();
            let secs = t0.elapsed().as_secs_f64();
            if let Some(ctx) = ctx {
                let mut ctx = ctx.lock().unwrap();
                ctx.add_extra_classes(classes);
            }
            println!(
                "added {kind}: {} ({count} classes, {secs:.1}s)",
                path.display()
            );
        }
        ScanMode::Background => {
            println!("added {kind}: {} (indexing…)", path.display());
            if let Some(ctx) = ctx {
                let ctx_bg = Arc::clone(ctx);
                let path_bg = path.clone();
                std::thread::Builder::new()
                    .name("cpadd-scan".into())
                    .spawn(move || {
                        if let Ok(bg_jvm) = EmbeddedJvm::new() {
                            let classes = if path_bg.is_file() {
                                bg_jvm.scan_jar_classes(&path_bg).unwrap_or_default()
                            } else {
                                scan_dir_classes(&path_bg)
                            };
                            let mut ctx = ctx_bg.lock().unwrap();
                            ctx.add_extra_classes(classes);
                        }
                    })
                    .ok();
            }
        }
        ScanMode::None => unreachable!(),
    }
}

/// Handle `:cplist` — print classpath entries.
fn handle_cplist(state: &ReplState) {
    if state.classpath.is_empty() {
        println!("(no extra classpath entries — JDK runtime is always available)");
    } else {
        for (i, p) in state.classpath.iter().enumerate() {
            println!("  {}: {}", i + 1, p.display());
        }
    }
}

/// XDG-compliant data directory for skotch.
///
/// Uses the `directories` crate to find the right platform path:
/// - Linux: `$XDG_DATA_HOME/skotch` (default `~/.local/share/skotch`)
/// - macOS: `~/Library/Application Support/skotch`
/// - Windows: `{FOLDERID_LocalAppData}/skotch`
///
/// Falls back to `~/.skotch` if the platform dirs can't be determined.
pub fn data_dir() -> std::path::PathBuf {
    directories::ProjectDirs::from("", "", "skotch")
        .map(|pd| pd.data_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".skotch")
        })
}

/// XDG-compliant config directory for skotch.
///
/// - Linux: `$XDG_CONFIG_HOME/skotch` (default `~/.config/skotch`)
/// - macOS: `~/Library/Application Support/skotch`
/// - Windows: `{FOLDERID_RoamingAppData}/skotch`
pub fn config_dir() -> std::path::PathBuf {
    directories::ProjectDirs::from("", "", "skotch")
        .map(|pd| pd.config_dir().to_path_buf())
        .unwrap_or_else(data_dir)
}

/// History file path (inside the data directory).
fn history_path() -> std::path::PathBuf {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir.join("repl_history")
}

/// Run the REPL on the given input/output streams (non-interactive).
///
/// This is the piped-input / test-suite entry point. It does NOT use
/// reedline (which requires a real terminal); instead it reads lines
/// from `input` via `BufRead` and writes prompts + output to
/// `output`. The REPL state and compilation pipeline are identical
/// to the interactive version.
///
/// Used by `skotch repl` when stdin is not a terminal, and by the
/// test suite which drives the REPL with canned input and asserts
/// against canned output.
pub fn run_repl<R: BufRead, W: Write>(input: R, mut output: W) -> Result<()> {
    let jvm = EmbeddedJvm::new()?;
    load_kotlin_stdlib(&jvm);

    let debug_star = if cfg!(debug_assertions) { "*" } else { "" };
    writeln!(
        output,
        "skotch repl{debug_star} — type `:quit` to exit, `:help` for commands"
    )?;

    let mut state = ReplState::new();
    for (turn_idx, line) in input.lines().enumerate() {
        let line = line.context("reading REPL input")?;
        write!(output, "skotch[{}]> ", turn_idx + 1)?;
        output.flush()?;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            writeln!(output)?;
            continue;
        }
        // ── REPL commands (colon-prefixed) ──────────────────────────
        if let Some(cmd) = trimmed.strip_prefix(':') {
            writeln!(output, "{trimmed}")?;
            let (cmd_name, cmd_arg) = cmd
                .split_once(' ')
                .map(|(n, a)| (n, a.trim()))
                .unwrap_or((cmd, ""));
            match cmd_name {
                "quit" | "exit" | "q" => {
                    writeln!(output, "bye")?;
                    return Ok(());
                }
                "help" | "h" | "?" => {
                    writeln!(output, "  :quit / :exit  — leave the REPL")?;
                    writeln!(output, "  :help          — show this help")?;
                    writeln!(output, "  :reset         — clear all declarations")?;
                    writeln!(output, "  :cpadd <path>  — add JAR/dir to classpath")?;
                    writeln!(output, "  :cplist        — list classpath entries")?;
                    writeln!(
                        output,
                        "  <kotlin>       — compile and run one expression or declaration"
                    )?;
                }
                "reset" | "clear" => {
                    state.reset();
                    writeln!(output, "(state cleared)")?;
                }
                "history" | "hist" => {
                    let all: Vec<&str> = state
                        .top_decls
                        .iter()
                        .chain(state.local_decls.iter())
                        .map(|s| s.as_str())
                        .collect();
                    if all.is_empty() {
                        writeln!(output, "(no declarations)")?;
                    } else {
                        for (i, d) in all.iter().enumerate() {
                            writeln!(output, "  {}: {d}", i + 1)?;
                        }
                    }
                }
                "cpadd" => {
                    if cmd_arg.is_empty() {
                        writeln!(output, "usage: :cpadd <jar-or-directory>")?;
                    } else {
                        handle_cpadd(cmd_arg, &jvm, &mut state, None, ScanMode::Eager);
                    }
                }
                "cplist" => {
                    handle_cplist(&state);
                }
                other => {
                    writeln!(output, "unknown command :{other}")?;
                }
            }
            continue;
        }
        // Echo the line (the prompt was already written, so this
        // appears immediately after the prompt, mimicking what a
        // terminal user would have typed).
        writeln!(output, "{line}")?;

        match state.process(&line, &jvm) {
            Ok(stdout) => {
                if !stdout.is_empty() {
                    output.write_all(stdout.as_bytes())?;
                    if !stdout.ends_with('\n') {
                        writeln!(output)?;
                    }
                }
            }
            Err(e) => {
                writeln!(output, "error: {e:#}")?;
            }
        }
    }

    Ok(())
}

/// Run a `.kts` script and return its stdout as a string.
///
/// The whole file's content is wrapped in `fun main() { … }`,
/// compiled to a `.class`, and executed in a JVM subprocess.
/// Returns the subprocess's captured stdout on success, or an error
/// containing the compiler diagnostic / JVM stderr on failure.
pub fn run_script(path: &Path) -> Result<String> {
    let jvm = EmbeddedJvm::new()?;
    load_kotlin_stdlib(&jvm);
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    // Resolve @file:DependsOn / @file:Repository annotations.
    let (deps, clean_source) = skotch_tape::resolve_script_deps(&source)?;
    if !deps.is_empty() {
        // Add resolved JARs to the JVM classpath.
        for jar in &deps.jars {
            jvm.add_jar_to_classpath(jar)?;
        }
    }

    let wrapped = wrap_script(&clean_source);
    let class_name = unique_class_name("Script");
    compile_and_run_jni(&wrapped, &jvm, &class_name)
}

/// Same as [`run_script`] but takes the raw script text instead of
/// a path. Useful for tests that don't want to round-trip through
/// the filesystem.
pub fn run_script_str(source: &str) -> Result<String> {
    let jvm = EmbeddedJvm::new()?;
    load_kotlin_stdlib(&jvm);
    let wrapped = wrap_script(source);
    let class_name = unique_class_name("Script");
    compile_and_run_jni(&wrapped, &jvm, &class_name)
}

/// Generate a unique class name per invocation. The JNI
/// `DefineClass` call only works once per class name per
/// ClassLoader, so reusing "ScriptKt" across REPL turns or
/// test invocations fails. A monotonic counter ensures each
/// call gets a fresh name.
fn unique_class_name(prefix: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{prefix}{n}Kt")
}

// ─── REPL state ──────────────────────────────────────────────────────────

/// Per-session REPL state. Holds the accumulated top-level
/// declaration history so each new turn can see prior `val`/`var`/
/// `fun` definitions. Shared by both the interactive (reedline) and
/// piped (BufRead) REPL paths.
struct ReplState {
    /// Top-level declarations (class, fun) that go outside fun main().
    top_decls: Vec<String>,
    /// Local declarations (val, var) that go inside fun main().
    local_decls: Vec<String>,
    /// Monotonic counter of all turns (declarations + expressions),
    /// used only for unique synthetic class names.
    turn: usize,
    /// Counter for auto-assigned result variables (res0, res1, ...).
    res_counter: usize,
    /// Known variable types from expression evaluation or annotation.
    var_types: HashMap<String, String>,
    /// Extra classpath entries added via `:cpadd`.
    classpath: Vec<PathBuf>,
}

impl ReplState {
    fn new() -> Self {
        ReplState {
            top_decls: Vec::new(),
            local_decls: Vec::new(),
            turn: 0,
            res_counter: 0,
            var_types: HashMap::new(),
            classpath: Vec::new(),
        }
    }

    /// Clear accumulated declarations but keep the turn counter so
    /// generated class names don't collide with already-loaded JVM classes.
    fn reset(&mut self) {
        self.top_decls.clear();
        self.local_decls.clear();
        self.var_types.clear();
        // Classpath entries are NOT cleared on reset.
    }

    /// Push current REPL state into the shared completion context.
    fn sync_completions(&self, ctx: &Arc<Mutex<CompletionCtx>>) {
        let mut ctx = ctx.lock().unwrap();
        ctx.sync_from_repl(
            &self.top_decls,
            &self.local_decls,
            self.res_counter,
            &self.var_types,
        );
    }

    /// Process one REPL turn. Returns the captured stdout (empty for
    /// declaration-only turns) or a compile/run error.
    fn process(&mut self, line: &str, jvm: &EmbeddedJvm) -> Result<String> {
        // Split on semicolons to handle multi-statement lines like:
        // "data class User(...); val user = User(...); println(user)"
        let parts: Vec<&str> = line
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if parts.len() > 1 {
            let mut combined_output = String::new();
            for part in parts {
                let out = self.process_single(part, jvm)?;
                combined_output.push_str(&out);
            }
            return Ok(combined_output);
        }
        self.process_single(line, jvm)
    }

    fn process_single(&mut self, line: &str, jvm: &EmbeddedJvm) -> Result<String> {
        self.turn += 1;

        match classify_input(line) {
            DeclKind::TopLevel => {
                // Class/fun declarations go at top level.
                let top = self.top_decls.join("\n");
                let locals = self.local_decls.join("\n    ");
                let candidate = format!("{top}\n{line}\nfun main() {{\n    {locals}\n}}\n");
                let class_name = format!("ReplTurn{}Kt", self.turn);
                compile_only(&candidate, &class_name)?;
                self.top_decls.push(line.to_string());
                Ok(String::new())
            }
            DeclKind::LocalDecl => {
                // val/var declarations go inside fun main() as locals.
                let top = self.top_decls.join("\n");
                let mut locals: Vec<String> = self.local_decls.clone();
                locals.push(line.to_string());
                let locals_str = locals.join("\n    ");
                let candidate = format!("{top}\nfun main() {{\n    {locals_str}\n}}\n");
                let class_name = format!("ReplTurn{}Kt", self.turn);
                compile_only(&candidate, &class_name)?;
                self.local_decls.push(line.to_string());
                Ok(String::new())
            }
            DeclKind::Statement => {
                // Statements (for, while, if, etc.) — execute inside fun main().
                let top = self.top_decls.join("\n");
                let locals = self.local_decls.join("\n    ");
                let body = line.trim_end();
                let source = format!("{top}\nfun main() {{\n    {locals}\n    {body}\n}}\n");
                let class_name = format!("ReplTurn{}Kt", self.turn);
                compile_and_run_jni(&source, jvm, &class_name)
            }
            DeclKind::Expr => {
                // Expression: assign to resN, print type + value, store for future use.
                let top = self.top_decls.join("\n");
                let locals = self.local_decls.join("\n    ");
                let body = line.trim_end();

                // Check if the expression is a bare `println`/`print` call — if so,
                // just execute it without capturing (it returns Unit).
                let is_print = body.starts_with("println(") || body.starts_with("print(");
                if is_print {
                    let source = format!("{top}\nfun main() {{\n    {locals}\n    {body}\n}}\n");
                    let class_name = format!("ReplTurn{}Kt", self.turn);
                    return compile_and_run_jni(&source, jvm, &class_name);
                }

                // Capture the expression result in a resN variable and print it.
                let res_name = format!("res{}", self.res_counter);
                let source = format!(
                    "{top}\nfun main() {{\n    {locals}\n    val {res_name} = {body}\n    \
                 println({res_name})\n}}\n"
                );
                let class_name = format!("ReplTurn{}Kt", self.turn);

                match compile_and_run_jni(&source, jvm, &class_name) {
                    Ok(stdout) => {
                        // Determine the display type from the value.
                        let value_str = stdout.trim_end();
                        let type_name = infer_display_type(value_str, body);
                        self.var_types
                            .insert(res_name.clone(), type_name.to_string());
                        self.local_decls.push(format!("val {res_name} = {body}"));
                        self.res_counter += 1;
                        Ok(format!("{res_name}: {type_name} = {value_str}\n"))
                    }
                    Err(_) => {
                        // Fallback: execute as a plain statement (no result capture).
                        self.turn += 1;
                        let source =
                            format!("{top}\nfun main() {{\n    {locals}\n    {body}\n}}\n");
                        let class_name = format!("ReplTurn{}Kt", self.turn);
                        compile_and_run_jni(&source, jvm, &class_name)
                    }
                }
            }
        }
    }
}

/// Classify a REPL line by actually parsing it and inspecting the AST.
/// Returns `Some(Decl variant)` for declarations, `None` for expressions.
enum DeclKind {
    TopLevel,  // class, fun, interface, object, enum, typealias
    LocalDecl, // val, var
    Statement, // for, while, if, try — statements that don't produce a value
    Expr,      // everything else (expressions to be captured in resN)
}

fn classify_input(line: &str) -> DeclKind {
    let mut interner = Interner::new();
    let mut diags = Diagnostics::new();
    let file_id = FileId(0);
    let lexed = skotch_lexer::lex(file_id, line, &mut diags);
    let ast = skotch_parser::parse_file(&lexed, &mut interner, &mut diags);

    // If parsing produced a single top-level declaration, classify it.
    if let Some(decl) = ast.decls.first() {
        return match decl {
            Decl::Fun(_)
            | Decl::Class(_)
            | Decl::Interface(_)
            | Decl::Object(_)
            | Decl::Enum(_)
            | Decl::TypeAlias(_) => DeclKind::TopLevel,
            Decl::Val(v) if v.is_var => DeclKind::LocalDecl,
            Decl::Val(_) => DeclKind::LocalDecl,
            Decl::Unsupported { .. } => DeclKind::Expr,
        };
    }

    // Check if the line starts with a statement keyword that should
    // execute inside fun main() without result capture.
    let trimmed = line.trim_start();
    if trimmed.starts_with("for ")
        || trimmed.starts_with("for(")
        || trimmed.starts_with("while ")
        || trimmed.starts_with("while(")
        || trimmed.starts_with("do ")
        || trimmed.starts_with("do{")
        || trimmed.starts_with("if ")
        || trimmed.starts_with("if(")
        || trimmed.starts_with("when ")
        || trimmed.starts_with("when(")
        || trimmed.starts_with("when{")
        || trimmed.starts_with("try ")
        || trimmed.starts_with("try{")
        || trimmed.starts_with("var ")
    {
        return DeclKind::Statement;
    }

    DeclKind::Expr
}

/// Infer a display type name from the printed value and expression text.
fn infer_display_type(value: &str, expr: &str) -> &'static str {
    // Try to parse as Int.
    if value.parse::<i64>().is_ok() && !value.contains('.') {
        if value.len() > 10 || value.parse::<i64>().unwrap_or(0).abs() > i32::MAX as i64 {
            return "kotlin.Long";
        }
        return "kotlin.Int";
    }
    // Try to parse as Double.
    if value.parse::<f64>().is_ok() && value.contains('.') {
        return "kotlin.Double";
    }
    // Boolean.
    if value == "true" || value == "false" {
        return "kotlin.Boolean";
    }
    // String (if the expression is a string literal or string operation).
    if expr.contains('"')
        || expr.contains(".uppercase")
        || expr.contains(".lowercase")
        || expr.contains(".trim")
        || expr.contains(".let")
        || expr.contains(" + \"")
    {
        return "kotlin.String";
    }
    // Default to String (println always produces string output).
    "kotlin.String"
}

// ─── compilation + execution helpers ─────────────────────────────────────

/// Wrap a `.kts` script's body in a synthetic `fun main()`.
fn wrap_script(source: &str) -> String {
    // Strip shebang line (e.g., `#!/usr/bin/env skotch`) if present.
    let body = if source.starts_with("#!") {
        source.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        source
    };
    format!("fun main() {{\n{body}\n}}\n")
}

/// Compile a synthetic source through `skotch-driver`'s JVM target,
/// returning the path to the produced `.class` file.
fn compile_only(source: &str, class_name: &str) -> Result<PathBuf> {
    let stem = class_name.strip_suffix("Kt").unwrap_or(class_name);
    let tmp = unique_tempdir("skotch-repl");
    std::fs::create_dir_all(&tmp).context("creating REPL temp dir")?;
    let mut chars = stem.chars();
    let first = chars.next().unwrap_or('s').to_ascii_lowercase();
    let rest: String = chars.collect();
    let lowered_stem = format!("{first}{rest}");
    let kt_path = tmp.join(format!("{lowered_stem}.kt"));
    std::fs::write(&kt_path, source).context("writing REPL temp source")?;
    let out_class = tmp.join(format!("{stem}Kt.class"));
    emit(&EmitOptions {
        input: kt_path,
        output: out_class.clone(),
        target: Target::Jvm,
        norm_out: None,
    })?;
    Ok(out_class)
}

/// Compile a synthetic source, then run its `main` method inside
/// the in-process JVM via JNI. Returns captured stdout.
fn compile_and_run_jni(source: &str, jvm: &EmbeddedJvm, class_name: &str) -> Result<String> {
    let class_path = compile_only(source, class_name)?;
    let class_dir = class_path
        .parent()
        .ok_or_else(|| anyhow!("class file has no parent directory"))?;

    // Load all additional class files (user-defined classes like data classes)
    // into the JVM before running main.
    let main_stem = class_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("compiled class has no UTF-8 stem"))?;
    if let Ok(entries) = std::fs::read_dir(class_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("class") {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                // Skip the main class — we'll load it when running.
                if stem == main_stem {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(&path) {
                    // Define the class in the JVM (ignore errors for already-defined).
                    let _ = jvm.define_class(stem, &bytes);
                }
            }
        }
    }

    let class_bytes = std::fs::read(&class_path)
        .with_context(|| format!("reading compiled class {}", class_path.display()))?;
    jvm.run_class_main(main_stem, &class_bytes)
}

/// Check whether the JVM can be initialized. Returns `Ok(())` if
/// `JAVA_HOME` is set and a JDK is found; `Err` with a clear
/// message otherwise. Called by test gating.
pub fn locate_java() -> Option<PathBuf> {
    // Delegate to the EmbeddedJvm's locator so both paths agree.
    skotch_jvm::locate::find_libjvm().ok()
}

fn unique_tempdir(label: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("{label}-{pid}-{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decl_classification() {
        // Top-level declarations
        assert!(matches!(classify_input("fun foo() {}"), DeclKind::TopLevel));
        assert!(matches!(classify_input("class Foo {}"), DeclKind::TopLevel));
        assert!(matches!(
            classify_input("data class Point(val x: Int)"),
            DeclKind::TopLevel
        ));
        assert!(matches!(
            classify_input("interface Greeter { fun greet(): String }"),
            DeclKind::TopLevel
        ));
        assert!(matches!(
            classify_input("object Singleton { fun greet(): String = \"hi\" }"),
            DeclKind::TopLevel
        ));

        // Local declarations (val, var)
        assert!(matches!(classify_input("val x = 1"), DeclKind::LocalDecl));
        assert!(matches!(classify_input("var y = 2"), DeclKind::LocalDecl));

        // Expressions
        assert!(matches!(classify_input("println(1)"), DeclKind::Expr));
        assert!(matches!(classify_input("1 + 2"), DeclKind::Expr));
    }

    #[test]
    fn wrap_script_produces_main() {
        let wrapped = wrap_script("println(\"hi\")");
        assert!(wrapped.contains("fun main()"));
        assert!(wrapped.contains("println(\"hi\")"));
    }

    #[test]
    fn locate_java_uses_java_home_when_set() {
        let _ = locate_java();
    }
}
