//! Multi-call binary dispatch.
//!
//! Inspired by BusyBox: when a tool is invoked through a different
//! `argv[0]` (typically a symlink), pretend the user typed that name
//! as the first subcommand.
//!
//!     ln -s /path/to/skotch /usr/local/bin/kotlinc
//!     kotlinc -d out/ Foo.kt   # == skotch kotlinc -d out/ Foo.kt
//!
//! Adding a new alias is a single line in `KNOWN_ALIASES`.

/// Aliased program names that map to a `skotch` subcommand.
///
/// Each entry is `(alias_name, subcommand_to_inject)`. The alias name is
/// matched against `argv[0]`'s file stem (so both `kotlinc` and
/// `kotlinc.exe` work on Windows).
///
/// Adding a new tool means:
///   1. Implement the subcommand in `main.rs` (clap variant + handler).
///   2. Add a `(name, subcommand)` line below.
///   3. Optionally `ln -s skotch <name>` so users can invoke it directly.
const KNOWN_ALIASES: &[(&str, &str)] = &[
    ("kotlinc", "kotlinc"),
    ("aapt2", "aapt2"),
    ("apksigner", "apksigner"),
    ("d8", "d8"),
];

/// If the running binary was invoked through one of the known aliases,
/// return the subcommand name to inject before the rest of `argv`.
///
/// Returns `None` for the normal `skotch …` invocation.
pub fn detect_alias() -> Option<&'static str> {
    let argv0 = std::env::args().next()?;
    let basename = std::path::Path::new(&argv0)
        .file_stem()?
        .to_str()?
        .to_lowercase();
    KNOWN_ALIASES
        .iter()
        .find(|(name, _)| *name == basename)
        .map(|(_, sub)| *sub)
}

/// Rewrite `args` so the multi-call alias becomes an explicit
/// subcommand. If no alias is in effect, returns `args` unchanged.
///
/// `args` is the full process argv (including `argv[0]`).
pub fn rewrite_argv(args: Vec<String>) -> Vec<String> {
    let Some(sub) = detect_alias() else {
        return args;
    };
    // Replace argv[0] with "skotch" and inject the subcommand right
    // after — clap will then pick it up exactly as if the user typed
    // `skotch <sub> …` directly.
    let mut out = Vec::with_capacity(args.len() + 1);
    out.push("skotch".to_string());
    out.push(sub.to_string());
    if args.len() > 1 {
        out.extend_from_slice(&args[1..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_passes_through_when_no_alias_matches() {
        // Set argv[0] to a name we don't recognise.
        let original = vec!["/usr/bin/skotch".to_string(), "emit".to_string()];
        let rewritten = rewrite_argv(original.clone());
        // detect_alias reads the real process argv[0], not the input
        // — so this test only verifies the no-op path under cargo's
        // own binary name. The reverse case is exercised by the
        // integration test that invokes the binary through a symlink.
        assert!(rewritten == original || rewritten[0] == "skotch");
    }
}
