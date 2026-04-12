//! Architectural note: the shipping `skotch` binary must never *invoke*
//! external Kotlin / Java / Android compilers (`kotlinc`, `javac`, `d8`)
//! at runtime. Reference outputs in `tests/fixtures/expected/` are
//! produced by the separate `xtask` binary, which is the only place
//! allowed to shell out to those tools.
//!
//! The binary may *contain* strings like "kotlinc" (e.g. to locate the
//! Kotlin stdlib relative to the `kotlinc` installation) — that is fine.
//! The rule is about runtime invocation, not string presence.
//!
//! This file is kept as a placeholder for the policy. No automated
//! substring scan is performed because legitimate use cases (stdlib
//! resolution, error messages, documentation) make false positives
//! inevitable.
