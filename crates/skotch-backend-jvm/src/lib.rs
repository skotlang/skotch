//! JVM bytecode emitter for skotch's MIR.
//!
//! Produces Java class file format major version 61 (Java 17). The
//! writer is hand-rolled over `byteorder` because the constant pool's
//! forward references defeat declarative writers like `binrw`.
//!
//! Lifted in spirit from `/opt/src/github/skotlang.old/crates/skotch-backend-jvm/`,
//! with the following adjustments:
//!
//! - Bumps `major_version` from 52 (Java 8) to 61 (Java 17).
//! - Replaces the old `Intrinsic(PrintlnAny) + find_string_local`
//!   walkback hack with a per-call type-driven dispatch that examines
//!   the type of the argument local.
//! - Adds the integer arithmetic ops needed by fixture 06.
//! - Adds `invokestatic` for inter-function calls (fixture 08).
//! - Adds the `final` access flag on the wrapper class (matches what
//!   `kotlinc` emits for top-level functions).
//!
//! ## What we cannot yet emit
//!
//! Branches require a `StackMapTable` attribute (the verifier rejects
//! branched methods without one in v51+). The initial fixtures avoid
//! branches, so we don't emit one. The lowering pass already errors
//! on `if`-as-expression and string templates.

mod class_writer;
mod constant_pool;

pub use class_writer::compile_module;
pub use class_writer::set_d8_safe_mode;
