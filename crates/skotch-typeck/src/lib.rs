//! Type checker — shared output types + the typed (SIL-backed) pass.
//!
//! # Soundness Invariant
//!
//! All type checking MUST go through `is_assignable`, which delegates
//! to [`Ty::assignable_to_in`] with the class hierarchy. Never use
//! [`Ty::assignable_to`] directly — it lacks hierarchy info and is
//! only a conservative fallback.
//!
//! When the type of an expression cannot be determined, the typechecker
//! returns `Ty::Error` (NOT `Ty::Any`). `Ty::Error` suppresses
//! cascading diagnostics without silently claiming a value is `Any`.
//!
//! # Output
//!
//! [`TypedFile`] provides:
//! - `functions[i].return_ty` / `.param_tys` — used by MIR lowering
//! - `functions[i].local_tys` — used by LSP hover info
//! - `top_signatures` — used by LSP and internal call resolution

use rustc_hash::FxHashMap;
use skotch_resolve::DefId;
use skotch_types::Ty;

pub mod typed;

// ─── Public output types ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct TypedFunction {
    pub name_index: u32,
    pub return_ty: Ty,
    pub param_tys: Vec<Ty>,
    pub local_tys: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct TypedTopVal {
    pub name_index: u32,
    pub ty: Ty,
}

#[derive(Default, Clone, Debug)]
pub struct TypedFile {
    pub functions: Vec<TypedFunction>,
    pub top_vals: Vec<TypedTopVal>,
    pub top_signatures: FxHashMap<DefId, Signature>,
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub params: Vec<Ty>,
    pub ret: Ty,
}
