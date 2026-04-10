//! Sugar-free typed IR. **Placeholder for PR #1.**
//!
//! In the original architectural plan, HIR sits between the type
//! checker's `TypedFile` and `MIR`. For PR #1 we collapse the two
//! lowerings: `skotch-mir-lower` consumes the AST + `ResolvedFile` +
//! `TypedFile` directly and produces MIR. HIR is therefore unused
//! today but the crate exists so that:
//!
//! 1. Future PRs can introduce a real HIR pass without restructuring
//!    the workspace DAG.
//! 2. Backends that want to consume signature-level information
//!    (without depending on the full MIR) have a stable place to
//!    import it from.
//!
//! See `crates/skotch-mir-lower/src/lib.rs` for the AST→MIR walker that
//! does the work HIR will eventually do.

use skotch_intern::Symbol;
use skotch_types::Ty;

/// Stub: a future HIR function. Currently unused; preserved so the
/// crate isn't empty and so cross-crate documentation links resolve.
#[derive(Clone, Debug)]
pub struct HirFunction {
    pub name: Symbol,
    pub params: Vec<Ty>,
    pub ret: Ty,
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder_compiles() {}
}
