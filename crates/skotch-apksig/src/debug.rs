//! In-process debug signing for the build pipeline.
//!
//! Replaces the old "shell out to the SDK `apksigner`" path: the build
//! pipeline calls [`sign_debug_apk`] directly, so producing an installable
//! APK no longer depends on an external tool. The debug keystore is read with
//! the standard Android conventions (password `android`, alias
//! `androiddebugkey`).

use crate::keystore;
use crate::sign::{ApkSigner, SignerConfig};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const DEBUG_KEYSTORE_PASSWORD: &str = "android";
const DEBUG_KEY_ALIAS: &str = "androiddebugkey";

/// Signs `unsigned_apk` with the Android debug keystore and returns the signed
/// bytes (v1 + v2 + v3 enabled, matching `apksigner sign` defaults).
pub fn sign_debug_apk(unsigned_apk: &[u8]) -> Result<Vec<u8>> {
    let keystore_path = find_debug_keystore()
        .context("Android debug keystore not found at ~/.android/debug.keystore")?;
    let data = std::fs::read(&keystore_path)
        .with_context(|| format!("reading {}", keystore_path.display()))?;
    let entry = keystore::load(
        &data,
        DEBUG_KEYSTORE_PASSWORD,
        Some(DEBUG_KEYSTORE_PASSWORD),
        Some(DEBUG_KEY_ALIAS),
    )
    .context("loading debug keystore")?;

    let signer = SignerConfig {
        name: "CERT".to_string(),
        key: entry.key,
        certificates: entry.certificates,
        min_sdk_version: 0,
        deterministic_dsa: false,
    };
    let result = ApkSigner::new(vec![signer])
        .v1_signing_enabled(true)
        .v2_signing_enabled(true)
        .v3_signing_enabled(true)
        .v4_signing_enabled(false)
        .sign(unsigned_apk)
        .context("signing APK with debug key")?;
    Ok(result.apk)
}

/// Convenience wrapper: read `unsigned_path`, sign, write to `output_path`.
pub fn sign_debug_apk_file(unsigned_path: &Path, output_path: &Path) -> Result<()> {
    let unsigned = std::fs::read(unsigned_path)
        .with_context(|| format!("reading {}", unsigned_path.display()))?;
    let signed = sign_debug_apk(&unsigned)?;
    std::fs::write(output_path, signed)
        .with_context(|| format!("writing {}", output_path.display()))?;
    Ok(())
}

/// Locates `~/.android/debug.keystore`.
pub fn find_debug_keystore() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".android/debug.keystore");
    path.exists().then_some(path)
}
