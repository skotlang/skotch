//! MIR lowering pass — the AST → MIR transformer.
//!
//! The legacy `lower_file` (consuming `&skotch_syntax::KtFile`) was
//! removed in the cutover to the typed pipeline. New consumers should
//! call [`typed::lower_file`], which takes a `skotch_ast::KtFile`
//! (the SIL-backed typed AST). This crate root keeps:
//!
//! - [`typed`] — the production MIR lowering pass
//! - [`registry`] — classpath registry helpers (preload, static-
//!   method-on-class, JVM interface check) shared by both pipelines
//! - [`descriptors`] — JVM descriptor parsing helpers, used by the
//!   typed pass and by external consumers (`skotch-backend-*`)

pub mod descriptors;
pub mod registry;
pub mod typed;

pub use registry::{is_jvm_interface, is_static_method_on_class, preload_registry_jars};
