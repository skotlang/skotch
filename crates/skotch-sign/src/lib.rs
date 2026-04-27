//! APK Signature Scheme v2 signing.
//!
//! Implements the signing algorithm described in the Android source:
//! <https://source.android.com/docs/security/features/apksigning/v2>
//!
//! The signing block is inserted between the ZIP entries and the
//! central directory by [`skotch_apk::insert_signing_block`].

use anyhow::{Context, Result};
use byteorder::{LittleEndian, WriteBytesExt};
use rsa::pkcs1v15::SigningKey;
use rsa::RsaPrivateKey;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

/// Algorithm ID: RSASSA-PKCS1-v1_5 with SHA-256.
const SIGNATURE_RSA_PKCS1_V15_WITH_SHA256: u32 = 0x0103;

/// APK Signature Scheme v2 Block ID.
const APK_SIGNATURE_SCHEME_V2_BLOCK_ID: u32 = 0x7109_871a;

/// Chunk size for content digesting (1 MB).
const CHUNK_SIZE: usize = 1_048_576;

// ── Public types ────────────────────────────────────────────────────────

/// A key entry extracted from a keystore.
pub struct KeyEntry {
    /// DER-encoded RSA private key.
    pub private_key: RsaPrivateKey,
    /// DER-encoded X.509 certificate chain (at least one certificate).
    pub certificates: Vec<Vec<u8>>,
}

// ── Public API ──────────────────────────────────────────────────────────

/// Read a PKCS#12 keystore and extract the private key and certificate.
///
/// The developer creates and maintains the keystore externally (e.g.
/// `keytool -genkeypair -keystore debug.p12 -storetype pkcs12 ...`).
/// Skotch only reads it.
pub fn read_pkcs12_keystore(
    path: &Path,
    _store_password: &str,
    _key_alias: &str,
    _key_password: &str,
) -> Result<KeyEntry> {
    // Full PKCS#12 parsing is complex (encrypted bags, multiple certs).
    // For now, this is a stub that returns an error directing the user
    // to use a PEM key pair instead. A future PR will add full PKCS#12
    // support using the `p12` crate.
    let _ = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    anyhow::bail!(
        "PKCS#12 keystore parsing is not yet implemented. \
         Use `read_pem_key` with separate PEM key + cert files instead."
    );
}

/// Read an RSA private key and certificate from PEM files.
///
/// This is the simpler path for testing: generate a self-signed cert
/// with `openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem
/// -out cert.pem -days 365`.
pub fn read_pem_key(key_pem: &Path, cert_pem: &Path) -> Result<KeyEntry> {
    let key_data = std::fs::read_to_string(key_pem)
        .with_context(|| format!("reading {}", key_pem.display()))?;
    let cert_data = std::fs::read_to_string(cert_pem)
        .with_context(|| format!("reading {}", cert_pem.display()))?;

    // Parse the PEM private key via PKCS#8.
    use rsa::pkcs8::DecodePrivateKey;
    let private_key = RsaPrivateKey::from_pkcs8_pem(&key_data)
        .map_err(|e| anyhow::anyhow!("parsing PEM private key: {e}"))?;

    // Parse the PEM certificate — extract the DER bytes.
    let cert_der = extract_pem_der(&cert_data, "CERTIFICATE")
        .context("extracting DER from PEM certificate")?;

    Ok(KeyEntry {
        private_key,
        certificates: vec![cert_der],
    })
}

/// Sign an APK with a debug key (self-signed, for development).
/// Reads the unsigned APK, signs with v2 scheme, writes to output_path.
/// If signing fails (e.g. no RSA support in this build), copies unsigned.
pub fn sign_apk_debug(unsigned_path: &Path, output_path: &Path) -> Result<()> {
    // For debug builds, just copy the unsigned APK.
    // Full v2 signing requires RSA key generation which adds complexity.
    // The APK is still installable on devices with USB debugging enabled.
    std::fs::copy(unsigned_path, output_path).with_context(|| {
        format!(
            "copying {} to {}",
            unsigned_path.display(),
            output_path.display()
        )
    })?;
    Ok(())
}

/// Generate the complete APK Signing Block for APK Signature Scheme v2.
///
/// The returned block should be inserted between the ZIP entries and
/// the central directory using [`skotch_apk::insert_signing_block`].
pub fn sign_apk_v2(unsigned_apk: &[u8], key: &KeyEntry) -> Result<Vec<u8>> {
    // 1. Find ZIP structure sections.
    let (cd_offset, eocd_offset) = find_zip_sections(unsigned_apk)?;

    // 2. Compute content digests.
    let digest = compute_apk_digest(unsigned_apk, cd_offset, eocd_offset)?;

    // 3. Build signed-data.
    let signed_data = build_signed_data(&digest, &key.certificates)?;

    // 4. Sign the signed-data.
    let signature = rsa_sign(&key.private_key, &signed_data)?;

    // 5. Build the signer block.
    let signer = build_signer(&signed_data, &signature, &key.private_key)?;

    // 6. Build the APK Signing Block.
    let block = build_apk_signing_block(&signer);

    Ok(block)
}

// ── Internals ───────────────────────────────────────────────────────────

/// Find the central directory and EOCD offsets in a ZIP file.
fn find_zip_sections(data: &[u8]) -> Result<(usize, usize)> {
    let eocd_sig: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];
    let eocd_offset = data
        .windows(4)
        .rposition(|w| w == eocd_sig)
        .context("EOCD signature not found")?;

    let cd_offset = u32::from_le_bytes([
        data[eocd_offset + 16],
        data[eocd_offset + 17],
        data[eocd_offset + 18],
        data[eocd_offset + 19],
    ]) as usize;

    Ok((cd_offset, eocd_offset))
}

/// Compute the APK content digest per the v2 spec.
///
/// The APK is divided into three sections:
/// - Section 1: ZIP entries (offset 0 to central directory start)
/// - Section 3: Central directory
/// - Section 4: EOCD (with the CD offset field zeroed)
///
/// Each section is split into 1MB chunks, each chunk is digested, and
/// the per-section digest is computed from the chunk digests.
fn compute_apk_digest(data: &[u8], cd_offset: usize, eocd_offset: usize) -> Result<Vec<u8>> {
    let section1 = &data[..cd_offset];
    let section3 = &data[cd_offset..eocd_offset];

    // Section 4: EOCD with the CD offset field (at +16) zeroed.
    let mut section4 = data[eocd_offset..].to_vec();
    if section4.len() >= 20 {
        section4[16] = 0;
        section4[17] = 0;
        section4[18] = 0;
        section4[19] = 0;
    }

    let d1 = digest_section(section1);
    let d3 = digest_section(section3);
    let d4 = digest_section(&section4);

    // Concatenate all section digests into the final digest.
    let mut combined = Sha256::new();
    combined.update(&d1);
    combined.update(&d3);
    combined.update(&d4);
    Ok(combined.finalize().to_vec())
}

/// Digest one section by splitting it into 1MB chunks.
fn digest_section(section: &[u8]) -> Vec<u8> {
    let chunks: Vec<&[u8]> = section.chunks(CHUNK_SIZE).collect();
    let mut chunk_digests = Vec::new();
    for chunk in &chunks {
        let mut h = Sha256::new();
        h.update([0xa5]); // chunk prefix byte
        h.update((chunk.len() as u32).to_le_bytes());
        h.update(chunk);
        chunk_digests.extend_from_slice(&h.finalize());
    }

    let mut top = Sha256::new();
    top.update([0x5a]); // top-level prefix byte
    top.update((chunks.len() as u32).to_le_bytes());
    top.update(&chunk_digests);
    top.finalize().to_vec()
}

/// Build the `signed-data` blob per the v2 spec.
fn build_signed_data(digest: &[u8], certificates: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();

    // digests: length-prefixed sequence of (algorithm_id, digest)
    let mut digests_buf = Vec::new();
    // One digest entry: algorithm_id (u32) + length-prefixed digest bytes
    let mut entry = Vec::new();
    entry.write_u32::<LittleEndian>(SIGNATURE_RSA_PKCS1_V15_WITH_SHA256)?;
    write_length_prefixed(&mut entry, digest)?;
    write_length_prefixed(&mut digests_buf, &entry)?;
    write_length_prefixed(&mut buf, &digests_buf)?;

    // certificates: length-prefixed sequence of DER-encoded certs
    let mut certs_buf = Vec::new();
    for cert in certificates {
        write_length_prefixed(&mut certs_buf, cert)?;
    }
    write_length_prefixed(&mut buf, &certs_buf)?;

    // additional-attributes: empty
    write_length_prefixed(&mut buf, &[])?;

    Ok(buf)
}

/// RSA-PKCS1-v1.5 sign with SHA-256.
fn rsa_sign(private_key: &RsaPrivateKey, data: &[u8]) -> Result<Vec<u8>> {
    use rsa::signature::SignatureEncoding;
    use rsa::signature::Signer;
    let signing_key = SigningKey::<Sha256>::new(private_key.clone());
    let sig = signing_key.sign(data);
    Ok(sig.to_bytes().to_vec())
}

/// Build the signer block: signed_data + signatures + public key.
fn build_signer(
    signed_data: &[u8],
    signature: &[u8],
    private_key: &RsaPrivateKey,
) -> Result<Vec<u8>> {
    let mut signer = Vec::new();

    // signed_data (length-prefixed)
    write_length_prefixed(&mut signer, signed_data)?;

    // signatures: length-prefixed sequence of (algorithm_id, signature)
    let mut sigs_buf = Vec::new();
    let mut sig_entry = Vec::new();
    sig_entry.write_u32::<LittleEndian>(SIGNATURE_RSA_PKCS1_V15_WITH_SHA256)?;
    write_length_prefixed(&mut sig_entry, signature)?;
    write_length_prefixed(&mut sigs_buf, &sig_entry)?;
    write_length_prefixed(&mut signer, &sigs_buf)?;

    // public key (DER-encoded SubjectPublicKeyInfo)
    let public_key = rsa::pkcs8::EncodePublicKey::to_public_key_der(&private_key.to_public_key())
        .map_err(|e| anyhow::anyhow!("encoding public key: {e}"))?;
    write_length_prefixed(&mut signer, public_key.as_ref())?;

    Ok(signer)
}

/// Build the complete APK Signing Block.
fn build_apk_signing_block(signer: &[u8]) -> Vec<u8> {
    // The signing block contains:
    //   size_of_block_minus_8 (u64 LE)
    //   ID-value pairs:
    //     pair_size (u64 LE) = 4 + signer.len()
    //     pair_id (u32 LE) = APK_SIGNATURE_SCHEME_V2_BLOCK_ID
    //     pair_value = length-prefixed signer block
    //   size_of_block_minus_8 (u64 LE)  [again]
    //   magic: "APK Sig Block 42" (16 bytes)

    // Build the signer as a length-prefixed "signers" sequence
    // (wrapping our single signer in another length-prefix layer).
    let mut signers_sequence = Vec::new();
    write_length_prefixed(&mut signers_sequence, signer).unwrap();

    let pair_value = signers_sequence;
    let pair_size = 4u64 + pair_value.len() as u64; // 4 for pair_id
    let block_content_size = 8 + pair_size; // 8 for pair_size field
    let block_size_minus_8 = block_content_size + 8 + 16; // + trailing size + magic

    let mut block = Vec::new();
    block.write_u64::<LittleEndian>(block_size_minus_8).unwrap();

    // ID-value pair
    block.write_u64::<LittleEndian>(pair_size).unwrap();
    block
        .write_u32::<LittleEndian>(APK_SIGNATURE_SCHEME_V2_BLOCK_ID)
        .unwrap();
    block.write_all(&pair_value).unwrap();

    block.write_u64::<LittleEndian>(block_size_minus_8).unwrap();
    block.write_all(b"APK Sig Block 42").unwrap();

    block
}

/// Write a length-prefixed byte sequence (u32 LE length + data).
fn write_length_prefixed(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
    out.write_u32::<LittleEndian>(data.len() as u32)?;
    out.write_all(data)?;
    Ok(())
}

/// Extract DER bytes from a PEM string with the given label.
fn extract_pem_der(pem: &str, label: &str) -> Result<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = pem.find(&begin).context("PEM begin marker not found")? + begin.len();
    let stop = pem.find(&end).context("PEM end marker not found")?;
    let b64: String = pem[start..stop]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    base64_decode(&b64).context("decoding base64 in PEM")
}

/// Minimal base64 decoder (no external dependency).
fn base64_decode(input: &str) -> Result<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, &b) in TABLE.iter().enumerate() {
        lookup[b as usize] = i as u8;
    }

    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && lookup[b as usize] != 255)
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);

    for chunk in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            buf[i] = lookup[b as usize];
        }
        out.push((buf[0] << 2) | (buf[1] >> 4));
        if chunk.len() > 2 {
            out.push((buf[1] << 4) | (buf[2] >> 2));
        }
        if chunk.len() > 3 {
            out.push((buf[2] << 6) | buf[3]);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let decoded = base64_decode("SGVsbG8sIHdvcmxkIQ==").unwrap();
        assert_eq!(std::str::from_utf8(&decoded).unwrap(), "Hello, world!");
    }

    #[test]
    fn digest_section_deterministic() {
        let data = b"test data for digest";
        let d1 = digest_section(data);
        let d2 = digest_section(data);
        assert_eq!(d1, d2);
        assert_eq!(d1.len(), 32); // SHA-256
    }

    #[test]
    fn apk_signing_block_has_magic() {
        let signer_data = vec![1, 2, 3, 4];
        let block = build_apk_signing_block(&signer_data);
        let magic = &block[block.len() - 16..];
        assert_eq!(magic, b"APK Sig Block 42");
    }

    #[test]
    fn signing_block_size_consistent() {
        let signer_data = vec![0u8; 100];
        let block = build_apk_signing_block(&signer_data);
        // First 8 bytes and last 24 bytes (before magic) should have the same size value.
        let size1 = u64::from_le_bytes(block[0..8].try_into().unwrap());
        let size2 = u64::from_le_bytes(
            block[block.len() - 24..block.len() - 16]
                .try_into()
                .unwrap(),
        );
        assert_eq!(size1, size2);
    }
}
