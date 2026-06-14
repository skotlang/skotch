//! Legacy `KtFile`-producing parser (removed).
//!
//! The legacy `parse_file` function that produced a Box-tree
//! `skotch_syntax::KtFile` was deleted as part of the typed-AST
//! cutover. The crate stays in the workspace for now so existing
//! Cargo manifests don't have to update in lock-step; the body is
//! intentionally empty. Consumers should use [`skotch_ast::parse`]
//! (SIL-backed typed AST) instead.
