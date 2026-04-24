//! Project-level build pipeline.
//!
//! Finds `build.gradle.kts`, discovers `.kt` sources, compiles them,
//! merges the resulting MIR modules, and dispatches to the appropriate
//! backend for packaging (JVM → JAR, Android → APK).

pub mod cache;
pub mod discover;
pub mod merge;
pub mod pipeline;
pub mod test_runner;

pub use pipeline::{build_project, BuildOptions, BuildOutcome};
pub use skotch_buildscript::BuildTarget;
pub use test_runner::{run_tests, TestOptions, TestResult};

// Internal re-export for test_runner.
pub(crate) use pipeline::wrapper_class_for as pipeline_wrapper_class_for;
