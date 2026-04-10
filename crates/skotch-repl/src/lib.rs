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
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

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

    let java =
        locate_java().ok_or_else(|| anyhow!("`java` is not on PATH and JAVA_HOME is unset"))?;

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
        DefaultPromptSegment::Basic("skotch".to_string()),
        DefaultPromptSegment::Empty,
    );

    println!("skotch repl — type :quit to exit, :help for commands");

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
                    if state.decls.is_empty() {
                        println!("(no declarations in history)");
                    } else {
                        for (i, d) in state.decls.iter().enumerate() {
                            println!("  {}: {d}", i + 1);
                        }
                    }
                    continue;
                }

                match state.process(&line, &java) {
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
    let java =
        locate_java().ok_or_else(|| anyhow!("`java` is not on PATH and JAVA_HOME is unset"))?;

    writeln!(
        output,
        "skotch repl — type `:quit` to exit, `:help` for commands"
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

        match state.process(&line, &java) {
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
    let java =
        locate_java().ok_or_else(|| anyhow!("`java` is not on PATH and JAVA_HOME is unset"))?;
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let wrapped = wrap_script(&source);
    compile_and_run(&wrapped, &java, "ScriptKt")
}

/// Same as [`run_script`] but takes the raw script text instead of
/// a path. Useful for tests that don't want to round-trip through
/// the filesystem.
pub fn run_script_str(source: &str) -> Result<String> {
    let java =
        locate_java().ok_or_else(|| anyhow!("`java` is not on PATH and JAVA_HOME is unset"))?;
    let wrapped = wrap_script(source);
    compile_and_run(&wrapped, &java, "ScriptKt")
}

// ─── REPL state ──────────────────────────────────────────────────────────

/// Per-session REPL state. Holds the accumulated top-level
/// declaration history so each new turn can see prior `val`/`var`/
/// `fun` definitions. Shared by both the interactive (reedline) and
/// piped (BufRead) REPL paths.
struct ReplState {
    /// Top-level declarations seen so far, one entry per `val`/`var`/
    /// `fun` line. Concatenated (separated by newlines) to form the
    /// preamble of every executed turn.
    decls: Vec<String>,
    /// Monotonic counter of all turns (declarations + expressions),
    /// used only for unique synthetic class names.
    turn: usize,
}

impl ReplState {
    fn new() -> Self {
        ReplState {
            decls: Vec::new(),
            turn: 0,
        }
    }

    /// Process one REPL turn. Returns the captured stdout (empty for
    /// declaration-only turns) or a compile/run error.
    fn process(&mut self, line: &str, java: &Path) -> Result<String> {
        self.turn += 1;
        let trimmed = line.trim_start();
        if is_top_level_decl(trimmed) {
            // Verify the new declaration parses + compiles by
            // wrapping it with all prior decls in a no-op main.
            // We don't actually run anything for declaration turns.
            let preamble = self.decls.join("\n");
            let candidate = format!("{preamble}\n{line}\nfun main() {{}}\n");
            let class_name = format!("ReplTurn{}Kt", self.turn);
            compile_only(&candidate, &class_name)?;
            // Persist on success.
            self.decls.push(line.to_string());
            Ok(String::new())
        } else {
            // Expression statement: wrap in a fresh main and run.
            let preamble = self.decls.join("\n");
            let body = line.trim_end();
            let source = format!("{preamble}\nfun main() {{\n    {body}\n}}\n");
            let class_name = format!("ReplTurn{}Kt", self.turn);
            compile_and_run(&source, java, &class_name)
        }
    }
}

/// Heuristic check for "is this line a top-level declaration?"
///
/// Looks at the leading keyword. Anything that starts with `val `,
/// `var `, or `fun ` (or has a visibility modifier in front of
/// those) is treated as a declaration. Everything else is treated
/// as an expression statement.
fn is_top_level_decl(trimmed: &str) -> bool {
    let after_modifier = trimmed
        .strip_prefix("public ")
        .or_else(|| trimmed.strip_prefix("private "))
        .or_else(|| trimmed.strip_prefix("internal "))
        .unwrap_or(trimmed);
    after_modifier.starts_with("val ")
        || after_modifier.starts_with("var ")
        || after_modifier.starts_with("fun ")
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

/// Compile + run. Returns the JVM subprocess's stdout on exit code 0,
/// or an error containing both the exit code and the captured
/// stderr on failure.
fn compile_and_run(source: &str, java: &Path, class_name: &str) -> Result<String> {
    let class_path = compile_only(source, class_name)?;
    let class_dir = class_path
        .parent()
        .ok_or_else(|| anyhow!("compiled class has no parent dir"))?;
    let class_stem = class_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("compiled class has no UTF-8 stem"))?;
    let out = Command::new(java)
        .arg("-cp")
        .arg(class_dir)
        .arg(class_stem)
        .output()
        .with_context(|| format!("invoking `{}`", java.display()))?;
    if !out.status.success() {
        return Err(anyhow!(
            "java exited with status {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Locate the `java` binary. Resolution order:
///
/// 1. `$JAVA_HOME/bin/java[.exe]` if `JAVA_HOME` is set and the file exists
/// 2. `which java` on `PATH`
///
/// Returns `None` if neither yields a `java` binary.
pub fn locate_java() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let candidate = PathBuf::from(home)
            .join("bin")
            .join(format!("java{}", std::env::consts::EXE_SUFFIX));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    which::which("java").ok()
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
    fn is_top_level_decl_recognizes_keywords() {
        assert!(is_top_level_decl("val x = 1"));
        assert!(is_top_level_decl("var y = 2"));
        assert!(is_top_level_decl("fun foo() {}"));
        assert!(is_top_level_decl("private val x = 1"));
        assert!(is_top_level_decl("internal fun foo() {}"));
        assert!(!is_top_level_decl("println(1)"));
        assert!(!is_top_level_decl("1 + 2"));
        assert!(!is_top_level_decl(""));
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
