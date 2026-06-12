//! Self-consistency validation of a written DEX: header sizes, checksum, and
//! signature. A cheap offline oracle complementing structural parity with d8.

use crate::reader;
use anyhow::{bail, Result};

/// Validates internal consistency of `dex` bytes.
pub fn validate(dex: &[u8]) -> Result<()> {
    let h = reader::parse_header(dex)?;
    if h.file_size as usize != dex.len() {
        bail!("file_size {} != actual {}", h.file_size, dex.len());
    }
    // checksum (Adler-32 over everything after the checksum field)
    let checksum = adler::adler32_slice(&dex[0x0c..]);
    let stored = u32::from_le_bytes([dex[8], dex[9], dex[10], dex[11]]);
    if checksum != stored {
        bail!("checksum mismatch: computed {checksum:#x}, stored {stored:#x}");
    }
    // signature (SHA-1 over everything after the signature field)
    use sha1::{Digest, Sha1};
    let sig = Sha1::digest(&dex[0x20..]);
    if sig.as_slice() != &dex[0x0c..0x20] {
        bail!("signature mismatch");
    }
    // map present and last
    let map = reader::parse_map(dex, h.map_off);
    if map.is_empty() {
        bail!("empty map");
    }
    Ok(())
}
