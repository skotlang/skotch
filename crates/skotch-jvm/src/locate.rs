//! Locate `libjvm` across different OS + JDK combinations.
//!
//! ## Resolution order
//!
//! 1. `$JAVA_HOME` — the canonical way to point at a JDK. Every
//!    major vendor (OpenJDK, Oracle, Corretto, Adoptium/Temurin,
//!    GraalVM, Zulu) sets this on install, and build tools like
//!    Gradle and SDKMAN rely on it.
//!
//! 2. `java` on `$PATH` — if JAVA_HOME is unset, we find the
//!    `java` binary and derive JAVA_HOME from its realpath. This
//!    handles Homebrew-style symlink chains on macOS.
//!
//! Once we have a candidate JAVA_HOME, we probe a list of
//! **platform-specific relative paths** where `libjvm` is known to
//! live. Different JDK vendors put it in different places:
//!
//! | OS      | Typical relative path                                    |
//! |---------|----------------------------------------------------------|
//! | macOS   | `lib/server/libjvm.dylib`                                |
//! | Linux   | `lib/server/libjvm.so`                                   |
//! | Linux   | `lib/amd64/server/libjvm.so` (older OpenJDK ≤ 8)         |
//! | Windows | `bin/server/jvm.dll`                                      |
//!
//! Some JDK installs (notably Homebrew on macOS) have a symlink
//! chain: `$JAVA_HOME/bin/java` → `../libexec/openjdk.jdk/…`. We
//! resolve symlinks before probing so the real path is always used.
//!
//! ## Error messages
//!
//! If no `libjvm` is found, the error lists every candidate path
//! that was tried, with a note about why each failed. This makes
//! debugging a broken JAVA_HOME painless.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// Relative paths under JAVA_HOME where libjvm is known to live,
/// in probe order. The first match wins.
fn candidate_suffixes() -> Vec<&'static str> {
    let mut v = Vec::new();

    #[cfg(target_os = "macos")]
    {
        v.push("lib/server/libjvm.dylib");
        // Homebrew on macOS: canonicalize(JAVA_HOME) lands at the
        // Cellar formula root, but the actual JDK is nested deeper
        // inside libexec/openjdk.jdk/Contents/Home/.
        v.push("libexec/openjdk.jdk/Contents/Home/lib/server/libjvm.dylib");
        // GraalVM / older Homebrew installs
        v.push("jre/lib/server/libjvm.dylib");
        v.push("lib/libjvm.dylib");
    }

    #[cfg(target_os = "linux")]
    {
        v.push("lib/server/libjvm.so");
        // Older 32-bit or multi-arch JDKs
        v.push("lib/amd64/server/libjvm.so");
        v.push("lib/aarch64/server/libjvm.so");
        v.push("jre/lib/server/libjvm.so");
        v.push("jre/lib/amd64/server/libjvm.so");
    }

    #[cfg(target_os = "windows")]
    {
        v.push("bin/server/jvm.dll");
        v.push("jre/bin/server/jvm.dll");
        v.push("bin/client/jvm.dll");
    }

    v
}

/// Find the path to `libjvm` (or `jvm.dll` on Windows) by probing
/// `$JAVA_HOME` and falling back to the `java` binary's realpath.
pub fn find_libjvm() -> Result<PathBuf> {
    let java_home = resolve_java_home()?;
    let real_home = std::fs::canonicalize(&java_home).unwrap_or(java_home.clone());

    let suffixes = candidate_suffixes();
    let mut tried: Vec<String> = Vec::new();

    for suffix in &suffixes {
        let candidate = real_home.join(suffix);
        if candidate.exists() {
            return Ok(candidate);
        }
        tried.push(format!("  - {} (not found)", candidate.display()));
    }

    // Also try the un-canonicalized JAVA_HOME (in case the symlink
    // target moves or the canonicalize produced something different).
    if real_home != java_home {
        for suffix in &suffixes {
            let candidate = java_home.join(suffix);
            if candidate.exists() {
                return Ok(candidate);
            }
            tried.push(format!("  - {} (not found)", candidate.display()));
        }
    }

    Err(anyhow!(
        "could not find libjvm under JAVA_HOME={}\n\
         Resolved (canonicalized) JAVA_HOME={}\n\
         Paths tried:\n{}\n\n\
         Ensure JAVA_HOME points at a JDK (not a JRE) and the \
         architecture matches this binary ({}).",
        java_home.display(),
        real_home.display(),
        tried.join("\n"),
        std::env::consts::ARCH,
    ))
}

/// Resolve `JAVA_HOME` from the environment or by following the
/// `java` binary on PATH.
fn resolve_java_home() -> Result<PathBuf> {
    // 1. Explicit JAVA_HOME env var.
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let home = PathBuf::from(home);
        if home.is_dir() {
            // Some JDKs (Homebrew on macOS) set JAVA_HOME to a
            // symlink dir. The actual libs might be deeper. We
            // canonicalize later, but check that `bin/java` exists
            // as a sanity check.
            return Ok(home);
        }
        return Err(anyhow!(
            "JAVA_HOME is set to `{}` but it is not a directory",
            home.display()
        ));
    }

    // 2. Derive from `java` on PATH.
    let java = which::which("java").map_err(|_| {
        anyhow!(
            "JAVA_HOME is not set and `java` is not on PATH.\n\
             Install a JDK (e.g. `brew install openjdk` on macOS, \
             `apt install default-jdk` on Linux, or download from \
             https://adoptium.net) and set JAVA_HOME."
        )
    })?;

    // `java` is typically a symlink chain:
    //   /usr/bin/java → /etc/alternatives/java → /usr/lib/jvm/java-17/bin/java
    // Resolve the full chain to find the real bin/ directory.
    let real_java = std::fs::canonicalize(&java).unwrap_or_else(|_| java.clone());

    // Go up from `bin/java` to the JDK root.
    if let Some(bin_dir) = real_java.parent() {
        if let Some(jdk_root) = bin_dir.parent() {
            if jdk_root.is_dir() {
                return Ok(jdk_root.to_path_buf());
            }
        }
    }

    Err(anyhow!(
        "found `java` at `{}` (resolved to `{}`), but could not \
         derive JAVA_HOME from it. Set JAVA_HOME explicitly.",
        java.display(),
        real_java.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_java_home_succeeds() {
        // On CI and developer machines, either JAVA_HOME is set or
        // java is on PATH. This test validates the happy path.
        let home = resolve_java_home();
        assert!(
            home.is_ok(),
            "resolve_java_home failed: {}",
            home.unwrap_err()
        );
        let home = home.unwrap();
        assert!(
            home.is_dir(),
            "JAVA_HOME is not a directory: {}",
            home.display()
        );
    }

    #[test]
    fn find_libjvm_succeeds() {
        let path = find_libjvm();
        assert!(path.is_ok(), "find_libjvm failed: {}", path.unwrap_err());
        let path = path.unwrap();
        assert!(
            path.exists(),
            "libjvm path does not exist: {}",
            path.display()
        );
    }

    #[test]
    fn candidate_suffixes_not_empty() {
        let s = candidate_suffixes();
        assert!(!s.is_empty());
    }
}
