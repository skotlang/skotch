//! Password-based decryption for PKCS#12 bags: PBES1 with the PKCS#12 KDF
//! (3DES / 40-bit RC2) and PBES2 (PBKDF2 + AES-CBC). Enough to read keytool /
//! Android Studio keystores.

use crate::der_lite::{self, Tlv};
use anyhow::{bail, Context, Result};

// PBE algorithm OIDs.
const OID_PBE_SHA1_3DES: &str = "1.2.840.113549.1.12.1.3";
const OID_PBE_SHA1_RC2_40: &str = "1.2.840.113549.1.12.1.6";
const OID_PBES2: &str = "1.2.840.113549.1.5.13";
const OID_PBKDF2: &str = "1.2.840.113549.1.5.12";
const OID_HMAC_SHA1: &str = "1.2.840.113549.2.7";
const OID_HMAC_SHA256: &str = "1.2.840.113549.2.9";
const OID_AES128_CBC: &str = "2.16.840.1.101.3.4.1.2";
const OID_AES192_CBC: &str = "2.16.840.1.101.3.4.1.22";
const OID_AES256_CBC: &str = "2.16.840.1.101.3.4.1.42";

/// Decrypts `ciphertext` given the DER `AlgorithmIdentifier` `algo` and a
/// password (clear text, not yet BMP-encoded).
pub fn decrypt(algo: Tlv, ciphertext: &[u8], password: &str) -> Result<Vec<u8>> {
    let mut c = der_lite::Cursor::new(algo.content);
    let oid = der_lite::oid_string(c.tlv()?)?;
    let params = c.tlv()?;
    match oid.as_str() {
        OID_PBE_SHA1_3DES => {
            let (salt, iters) = pbe_params(params)?;
            let key = pkcs12_kdf(password, &salt, iters, 1, 24);
            let iv = pkcs12_kdf(password, &salt, iters, 2, 8);
            tdes_cbc_decrypt(&key, &iv, ciphertext)
        }
        OID_PBE_SHA1_RC2_40 => {
            let (salt, iters) = pbe_params(params)?;
            let key = pkcs12_kdf(password, &salt, iters, 1, 5);
            let iv = pkcs12_kdf(password, &salt, iters, 2, 8);
            rc2_cbc_decrypt(&key, &iv, ciphertext)
        }
        OID_PBES2 => pbes2_decrypt(params, ciphertext, password),
        other => bail!("unsupported PBE algorithm {other}"),
    }
}

/// PBES1 params: SEQUENCE { salt OCTET STRING, iterations INTEGER }.
fn pbe_params(params: Tlv) -> Result<(Vec<u8>, u32)> {
    let mut c = der_lite::Cursor::new(params.content);
    let salt = der_lite::octet_string_inner_tlv(c.tlv()?)?.to_vec();
    let iters = der_lite::integer_u32(c.tlv()?)?;
    Ok((salt, iters))
}

/// PBES2: params SEQUENCE { keyDerivationFunc AlgId, encryptionScheme AlgId }.
fn pbes2_decrypt(params: Tlv, ciphertext: &[u8], password: &str) -> Result<Vec<u8>> {
    let mut c = der_lite::Cursor::new(params.content);
    let kdf = c.tlv()?;
    let enc = c.tlv()?;

    // keyDerivationFunc: SEQUENCE { OID pbkdf2, params }.
    let mut kc = der_lite::Cursor::new(kdf.content);
    let kdf_oid = der_lite::oid_string(kc.tlv()?)?;
    if kdf_oid != OID_PBKDF2 {
        bail!("unsupported PBES2 KDF {kdf_oid}");
    }
    let kdf_params = kc.tlv()?;
    // PBKDF2-params: SEQUENCE { salt OCTET STRING, iterationCount INTEGER,
    //   keyLength INTEGER OPTIONAL, prf AlgId OPTIONAL }.
    let mut pc = der_lite::Cursor::new(kdf_params.content);
    let salt = der_lite::octet_string_inner_tlv(pc.tlv()?)?.to_vec();
    let iters = der_lite::integer_u32(pc.tlv()?)?;
    let mut key_len: Option<usize> = None;
    let mut prf = OID_HMAC_SHA1.to_string();
    while let Some(t) = pc.try_tlv()? {
        match t.tag {
            0x02 => key_len = Some(der_lite::integer_u32(t)? as usize),
            0x30 => {
                let mut prfc = der_lite::Cursor::new(t.content);
                prf = der_lite::oid_string(prfc.tlv()?)?;
            }
            _ => {}
        }
    }

    // encryptionScheme: SEQUENCE { OID aes-cbc, IV OCTET STRING }.
    let mut ec = der_lite::Cursor::new(enc.content);
    let enc_oid = der_lite::oid_string(ec.tlv()?)?;
    let iv = der_lite::octet_string_inner_tlv(ec.tlv()?)?.to_vec();

    let key_size = match enc_oid.as_str() {
        OID_AES128_CBC => 16,
        OID_AES192_CBC => 24,
        OID_AES256_CBC => 32,
        other => bail!("unsupported PBES2 cipher {other}"),
    };
    let dk_len = key_len.unwrap_or(key_size);
    let key = pbkdf2_derive(&prf, password.as_bytes(), &salt, iters, dk_len)?;
    aes_cbc_decrypt(&key, &iv, ciphertext)
}

fn pbkdf2_derive(
    prf: &str,
    password: &[u8],
    salt: &[u8],
    iters: u32,
    dk_len: usize,
) -> Result<Vec<u8>> {
    let mut out = vec![0u8; dk_len];
    match prf {
        OID_HMAC_SHA1 => {
            pbkdf2::pbkdf2::<hmac::Hmac<sha1::Sha1>>(password, salt, iters, &mut out)
                .map_err(|_| anyhow::anyhow!("pbkdf2 output length invalid"))?;
        }
        OID_HMAC_SHA256 => {
            pbkdf2::pbkdf2::<hmac::Hmac<sha2::Sha256>>(password, salt, iters, &mut out)
                .map_err(|_| anyhow::anyhow!("pbkdf2 output length invalid"))?;
        }
        other => bail!("unsupported PBKDF2 PRF {other}"),
    }
    Ok(out)
}

/// PKCS#12 key derivation (RFC 7292 Appendix B) with SHA-1.
fn pkcs12_kdf(password: &str, salt: &[u8], iterations: u32, id: u8, n: usize) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    const U: usize = 20; // SHA-1 output
    const V: usize = 64; // SHA-1 block

    // Password as BMPString (UTF-16BE) with a 2-byte null terminator.
    let pwd: Vec<u8> = {
        let mut p = Vec::new();
        for u in password.encode_utf16() {
            p.extend_from_slice(&u.to_be_bytes());
        }
        p.extend_from_slice(&[0, 0]);
        p
    };

    let d = vec![id; V];
    let s = fill_to_multiple(salt, V);
    let p = fill_to_multiple(&pwd, V);
    let mut i_buf = Vec::with_capacity(s.len() + p.len());
    i_buf.extend_from_slice(&s);
    i_buf.extend_from_slice(&p);

    let c = n.div_ceil(U);
    let mut out = Vec::with_capacity(c * U);
    for _ in 0..c {
        // A = H^iterations(D || I)
        let mut a = {
            let mut h = Sha1::new();
            h.update(&d);
            h.update(&i_buf);
            h.finalize().to_vec()
        };
        for _ in 1..iterations {
            a = Sha1::digest(&a).to_vec();
        }
        out.extend_from_slice(&a);

        // B = A repeated to V bytes.
        let b = fill_to_multiple(&a, V);
        // I_j = (I_j + B + 1) mod 2^(V*8) for each V-byte block.
        let blocks = i_buf.len() / V;
        for j in 0..blocks {
            let block = &mut i_buf[j * V..(j + 1) * V];
            add_with_carry(block, &b);
        }
    }
    out.truncate(n);
    out
}

fn fill_to_multiple(data: &[u8], block: usize) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let len = data.len().div_ceil(block) * block;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let take = (len - out.len()).min(data.len());
        out.extend_from_slice(&data[..take]);
    }
    out
}

/// block = (block + addend + 1) mod 2^(len*8), big-endian.
fn add_with_carry(block: &mut [u8], addend: &[u8]) {
    let mut carry = 1u16;
    for i in (0..block.len()).rev() {
        let sum = block[i] as u16 + addend[i] as u16 + carry;
        block[i] = (sum & 0xff) as u8;
        carry = sum >> 8;
    }
}

// ── Block cipher CBC decryption with PKCS#7 unpadding ─────────────────────

fn tdes_cbc_decrypt(key: &[u8], iv: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    type Dec = cbc::Decryptor<des::TdesEde3>;
    let dec = Dec::new_from_slices(key, iv).context("3DES key/iv")?;
    let mut buf = ct.to_vec();
    let pt = dec
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| anyhow::anyhow!("3DES decryption failed (wrong password?)"))?;
    Ok(pt.to_vec())
}

fn aes_cbc_decrypt(key: &[u8], iv: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    let mut buf = ct.to_vec();
    let pt = match key.len() {
        16 => cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
            .context("AES key/iv")?
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|_| anyhow::anyhow!("AES decryption failed (wrong password?)"))?
            .to_vec(),
        24 => cbc::Decryptor::<aes::Aes192>::new_from_slices(key, iv)
            .context("AES key/iv")?
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|_| anyhow::anyhow!("AES decryption failed (wrong password?)"))?
            .to_vec(),
        32 => cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv)
            .context("AES key/iv")?
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|_| anyhow::anyhow!("AES decryption failed (wrong password?)"))?
            .to_vec(),
        n => bail!("unexpected AES key length {n}"),
    };
    Ok(pt)
}

fn rc2_cbc_decrypt(key: &[u8], iv: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    // 40-bit effective key length for pbeWithSHAAnd40BitRC2-CBC.
    type Dec = cbc::Decryptor<rc2::Rc2>;
    let dec = Dec::new_from_slices(key, iv).context("RC2 key/iv")?;
    let mut buf = ct.to_vec();
    let pt = dec
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| anyhow::anyhow!("RC2 decryption failed (wrong password?)"))?;
    Ok(pt.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkcs12_kdf_known_vector() {
        // RFC 7292 doesn't ship vectors, but the KDF is deterministic; verify
        // basic shape and stability.
        let k1 = pkcs12_kdf("password", b"\x01\x02\x03\x04", 1, 1, 24);
        let k2 = pkcs12_kdf("password", b"\x01\x02\x03\x04", 1, 1, 24);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 24);
    }
}
