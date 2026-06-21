//! APK signing and verification — a faithful Rust port of AOSP's apksig library.
//!
//! Implements every signature scheme used by the `apksigner` tool:
//!
//! - **v1** — JAR signing (`META-INF/MANIFEST.MF`, `.SF`, PKCS#7 `.RSA`/`.DSA`/`.EC`)
//! - **v2** — APK Signature Scheme v2 (whole-file, APK Signing Block id `0x7109871a`)
//! - **v3 / v3.1** — key-rotation-capable schemes (`0xf05368c0` / `0x1b93ad61`)
//!   including [`lineage::SigningCertificateLineage`] proof-of-rotation
//! - **v4** — fs-verity based `.idsig` files for incremental installs
//!
//! The top-level entry points are [`sign::ApkSigner`] (a builder mirroring
//! `com.android.apksig.ApkSigner`) and [`verify::ApkVerifier`]. The
//! `skotch apksigner` CLI is a thin wrapper over these; the skotch build
//! pipeline calls them directly so release/debug signing never shells out.
//!
//! Byte-format fidelity is validated against the golden APKs in AOSP's
//! apksig test resources (see `tests/` and `tests/fixtures/apksigner` at the
//! workspace root): signing `golden-*-in.apk` with the same key reproduces
//! `golden-*-out.apk` byte-for-byte.

pub mod axml;
pub mod base64;
pub mod crypto;
pub mod debug;
pub mod der_lite;
pub mod derhelp;
pub mod digest;
pub mod keystore;
pub mod lineage;
pub mod pbe;
pub mod pkcs7;
pub mod sigblock;
pub mod sign;
pub mod v1;
pub mod v1verify;
pub mod v2;
pub mod v3;
pub mod v4;
pub mod verify;
pub mod zip;

pub use crypto::{Certificate, PrivateKey, SignatureAlgorithm};
pub use lineage::{SignerCapabilities, SigningCertificateLineage};
pub use sign::{ApkSigner, SignerConfig};
pub use verify::ApkVerifier;

/// Android SDK version constants used across schemes (AndroidSdkVersion.java).
pub mod sdk {
    pub const INITIAL_RELEASE: u32 = 1;
    pub const GINGERBREAD: u32 = 9;
    pub const HONEYCOMB: u32 = 11;
    pub const JELLY_BEAN_MR2: u32 = 18;
    pub const KITKAT: u32 = 19;
    pub const LOLLIPOP: u32 = 21;
    pub const M: u32 = 23;
    pub const N: u32 = 24;
    pub const O: u32 = 26;
    pub const P: u32 = 28;
    pub const Q: u32 = 29;
    pub const R: u32 = 30;
    pub const S: u32 = 31;
    pub const SV2: u32 = 32;
    pub const T: u32 = 33;
    pub const U: u32 = 34;
}

/// Maximum number of signers supported per scheme (Constants.java).
pub const MAX_APK_SIGNERS: usize = 10;
