//! Core of the native `skotch d8` dexer.
//!
//! This crate will hold the shared compiler core (program graph, SSA IR,
//! CF→IR builder, linear-scan register allocator, IR→DEX instruction
//! selection, desugaring) that D8 and a future R8 driver both use, designed
//! around an injected `AppView<Info>` so R8 is additive — see
//! `docs/skotch-d8-design.md`.
//!
//! What exists today is [`bootstrap`]: an IR-less, straight-line CF→DEX
//! translator that produces byte-identical output to d8 for trivial methods
//! (synthesized constructors, simple expression bodies). It is the bootstrap
//! that proves the end-to-end pipeline; the full SSA IR + register allocator
//! (Phase 1) replaces it.

pub mod bootstrap;

pub use bootstrap::dex_class;
