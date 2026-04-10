//! Workspace-wide constants for skotch.
//!
//! Pure data with no behavior. Backends and the build engine import these
//! to avoid scattering magic numbers across the workspace.

/// JVM class file format constants.
pub mod jvm {
    /// Minor version field. Always zero for "real" Java releases.
    pub const CLASS_FILE_MINOR: u16 = 0;

    /// Major version 61 = Java 17 (JEP 410). PR #1 targets this.
    pub const CLASS_FILE_MAJOR_JAVA_17: u16 = 61;

    /// Major version 52 = Java 8. Kept as a constant for the test fixture
    /// generator, which sometimes targets older bytecode for comparison.
    pub const CLASS_FILE_MAJOR_JAVA_8: u16 = 52;

    /// `cafebabe` magic.
    pub const CLASS_FILE_MAGIC: u32 = 0xCAFE_BABE;

    /// Default for newly emitted classes.
    pub const DEFAULT_CLASS_FILE_MAJOR: u16 = CLASS_FILE_MAJOR_JAVA_17;
}

/// DEX file format constants.
pub mod dex {
    /// DEX format version "035" (the byte sequence "035\0").
    pub const DEX_VERSION_035: [u8; 4] = [b'0', b'3', b'5', 0];

    /// DEX file magic "dex\n".
    pub const DEX_MAGIC: [u8; 4] = [b'd', b'e', b'x', b'\n'];
}

/// Android-side constants used by the (later) APK packaging crates.
pub mod android {
    /// Default `compileSdk` for projects that don't pin one.
    pub const DEFAULT_COMPILE_SDK: u32 = 34;
    /// Default `minSdk` for projects that don't pin one.
    pub const DEFAULT_MIN_SDK: u32 = 24;
    /// Default `targetSdk` for projects that don't pin one.
    pub const DEFAULT_TARGET_SDK: u32 = 34;
}
