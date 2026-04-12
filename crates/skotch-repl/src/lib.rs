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
//!
//! Stubs for tab-completion, syntax highlighting, input validation
//! (multi-line entry), and hints are wired into reedline's extension
//! points but currently return passthrough defaults. They're called
//! out with `// TODO:` comments so a future PR can flesh them out.
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

use anyhow::{anyhow, Context, Result};
use skotch_jvm::EmbeddedJvm;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use skotch_driver::{emit, EmitOptions, Target};

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
pub fn run_repl_interactive() -> Result<()> {
    use reedline::{
        default_emacs_keybindings, DefaultPrompt, DefaultPromptSegment, EditCommand, Emacs,
        KeyCode, KeyModifiers, Reedline, ReedlineEvent, Signal,
    };

    let jvm = EmbeddedJvm::new()?;

    // ── reedline setup ──────────────────────────────────────────────
    //
    // Future extension points are wired here with passthrough stubs.
    // Each stub has a `// TODO:` comment describing what a real
    // implementation would do.
    let mut keybindings = default_emacs_keybindings();
    // Ctrl-D on empty line → exit. Reedline handles this natively
    // for `Signal::CtrlD`, but we also add a keybinding so it works
    // even when the user has typed partial text and then deleted it.
    keybindings.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('l'),
        ReedlineEvent::Edit(vec![EditCommand::Clear]),
    );
    let edit_mode = Box::new(Emacs::new(keybindings));

    // TODO: Tab-completion — implement `reedline::Completer` to
    //   suggest Kotlin keywords, REPL commands (`:quit`, `:help`),
    //   and identifiers from accumulated declarations.
    //
    // TODO: Syntax highlighting — implement `reedline::Highlighter`
    //   that colorizes Kotlin keywords, string literals, and
    //   numbers using ANSI escape codes.
    //
    // TODO: Input validation — implement `reedline::Validator` that
    //   checks for unbalanced braces/parens/quotes so the user can
    //   enter multi-line expressions naturally. When the validator
    //   reports "incomplete", reedline continues prompting for more
    //   input instead of executing the partial line.
    //
    // TODO: Hints — implement `reedline::Hinter` that shows a
    //   dimmed-text preview of the most recent history match as the
    //   user types (fish-shell style). reedline supplies a built-in
    //   `DefaultHinter` that can be wired up with one line once we
    //   decide on the hint color.

    let mut editor = Reedline::create().with_edit_mode(edit_mode);

    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic(if cfg!(debug_assertions) {
            "\x1b[33mskotch\x1b[0m".to_string()
        } else {
            "skotch".to_string()
        }),
        DefaultPromptSegment::Empty,
    );

    let debug_star = if cfg!(debug_assertions) { "*" } else { "" };
    println!(
        "skotch {}{debug_star} — type :quit to exit, :help for commands",
        env!("CARGO_PKG_VERSION")
    );

    let mut state = ReplState::new();

    loop {
        let sig = editor.read_line(&prompt);
        match sig {
            Ok(Signal::Success(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == ":quit" || trimmed == ":exit" {
                    println!("bye");
                    return Ok(());
                }
                if trimmed == ":help" {
                    println!("  :quit / :exit  — leave the REPL");
                    println!("  :help          — show this help");
                    println!("  :history       — show command history");
                    println!("  <kotlin>       — compile and run one expression or declaration");
                    continue;
                }
                if trimmed == ":history" {
                    // TODO: reedline doesn't expose a public iterator
                    // over its in-memory history yet, so we print from
                    // our own declaration history. A future PR could
                    // hook up reedline's FileBackedHistory to persist
                    // across sessions and iterate it here.
                    let all_decls: Vec<&str> = state
                        .top_decls
                        .iter()
                        .chain(state.local_decls.iter())
                        .map(|s| s.as_str())
                        .collect();
                    if all_decls.is_empty() {
                        println!("(no declarations in history)");
                    } else {
                        for (i, d) in all_decls.iter().enumerate() {
                            println!("  {}: {d}", i + 1);
                        }
                    }
                    continue;
                }

                match state.process(&line, &jvm) {
                    Ok(stdout) => {
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
            Ok(Signal::CtrlC) => {
                // Abort the current line, print a fresh prompt.
                continue;
            }
            Err(e) => {
                eprintln!("reedline error: {e}");
                return Err(e.into());
            }
        }
    }
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
        if trimmed == ":quit" || trimmed == ":exit" {
            writeln!(output, "{trimmed}")?;
            writeln!(output, "bye")?;
            return Ok(());
        }
        if trimmed == ":help" {
            writeln!(output, "{trimmed}")?;
            writeln!(output, "  :quit / :exit  — leave the REPL")?;
            writeln!(output, "  :help          — show this help")?;
            writeln!(
                output,
                "  <kotlin>       — compile and run one expression or declaration"
            )?;
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
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let wrapped = wrap_script(&source);
    let class_name = unique_class_name("Script");
    compile_and_run_jni(&wrapped, &jvm, &class_name)
}

/// Same as [`run_script`] but takes the raw script text instead of
/// a path. Useful for tests that don't want to round-trip through
/// the filesystem.
pub fn run_script_str(source: &str) -> Result<String> {
    let jvm = EmbeddedJvm::new()?;
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
}

impl ReplState {
    fn new() -> Self {
        ReplState {
            top_decls: Vec::new(),
            local_decls: Vec::new(),
            turn: 0,
        }
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
        let trimmed = line.trim_start();

        if is_top_level_only_decl(trimmed) {
            // Class/fun declarations go at top level.
            let top = self.top_decls.join("\n");
            let locals = self.local_decls.join("\n    ");
            let candidate = format!("{top}\n{line}\nfun main() {{\n    {locals}\n}}\n");
            let class_name = format!("ReplTurn{}Kt", self.turn);
            compile_only(&candidate, &class_name)?;
            self.top_decls.push(line.to_string());
            Ok(String::new())
        } else if is_local_decl(trimmed) {
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
        } else {
            // Expression statement: wrap in main with all prior decls.
            let top = self.top_decls.join("\n");
            let locals = self.local_decls.join("\n    ");
            let body = line.trim_end();
            let source = format!("{top}\nfun main() {{\n    {locals}\n    {body}\n}}\n");
            let class_name = format!("ReplTurn{}Kt", self.turn);
            compile_and_run_jni(&source, jvm, &class_name)
        }
    }
}

/// Heuristic check for "is this line a top-level declaration?"
///
/// Looks at the leading keyword. Anything that starts with `val `,
/// `var `, or `fun ` (or has a visibility modifier in front of
/// those) is treated as a declaration. Everything else is treated
/// as an expression statement.
/// Strip leading modifier keywords from a declaration line.
fn strip_modifiers(trimmed: &str) -> &str {
    let mut s = trimmed;
    loop {
        let next = s
            .strip_prefix("public ")
            .or_else(|| s.strip_prefix("private "))
            .or_else(|| s.strip_prefix("internal "))
            .or_else(|| s.strip_prefix("data "))
            .or_else(|| s.strip_prefix("enum "))
            .or_else(|| s.strip_prefix("open "))
            .or_else(|| s.strip_prefix("abstract "))
            .or_else(|| s.strip_prefix("const "));
        match next {
            Some(rest) => s = rest.trim_start(),
            None => break,
        }
    }
    s
}

/// Is this a declaration that must be at the top level (class, fun)?
fn is_top_level_only_decl(trimmed: &str) -> bool {
    let s = strip_modifiers(trimmed);
    s.starts_with("class ") || s.starts_with("fun ")
}

/// Is this a local declaration (val, var)?
fn is_local_decl(trimmed: &str) -> bool {
    let s = strip_modifiers(trimmed);
    s.starts_with("val ") || s.starts_with("var ")
}

// ─── compilation + execution helpers ─────────────────────────────────────

/// Wrap a `.kts` script's body in a synthetic `fun main()`.
fn wrap_script(source: &str) -> String {
    format!("fun main() {{\n{source}\n}}\n")
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
        // Top-level declarations (class, fun)
        assert!(is_top_level_only_decl("fun foo() {}"));
        assert!(is_top_level_only_decl("class Foo {}"));
        assert!(is_top_level_only_decl("data class Point(val x: Int)"));
        assert!(is_top_level_only_decl("private fun foo() {}"));
        assert!(!is_top_level_only_decl("val x = 1"));
        assert!(!is_top_level_only_decl("println(1)"));

        // Local declarations (val, var)
        assert!(is_local_decl("val x = 1"));
        assert!(is_local_decl("var y = 2"));
        assert!(is_local_decl("const val PI = 3.14"));
        assert!(!is_local_decl("fun foo() {}"));
        assert!(!is_local_decl("println(1)"));
        assert!(!is_local_decl(""));
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
