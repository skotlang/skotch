//! Dalvik Executable (`.dex`) file format: a symbolic program [`model`], a
//! byte-identical [`writer`] (targeting Android `d8` 8.10.x output), a
//! [`reader`], and a [`validator`].
//!
//! This crate is the format layer of the native `skotch d8` dexer. It is
//! dependency-light and independent of Skotch's compiler backends, so it can be
//! reused by the dexer, the (future) R8 driver, and tests.

pub mod leb128;
pub mod model;
pub mod mutf8;
pub mod reader;
pub mod validator;
pub mod writer;

pub use model::DexFile;
pub use writer::write;

/// The `~~D8{…}` marker string d8 embeds in every DEX. It records the d8 build,
/// so reproducing it exactly is required for byte-identity. The version/SHA are
/// pinned to the reference d8 (8.10.9-dev).
pub fn d8_marker(compilation_mode: &str, min_api: u32) -> String {
    format!(
        "~~D8{{\"backend\":\"dex\",\"compilation-mode\":\"{compilation_mode}\",\
         \"has-checksums\":false,\"min-api\":{min_api},\
         \"sha-1\":\"a7ad18a70460b799d0482e497c109a75bf7f91de\",\
         \"version\":\"8.10.9-dev\"}}"
    )
}
