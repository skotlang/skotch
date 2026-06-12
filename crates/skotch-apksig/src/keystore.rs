//! Keystore loading for the `--ks` CLI option and the build pipeline's debug
//! keystore. Supports PKCS#12 (the modern keytool/Android Studio default) and
//! legacy JKS, including the encryption schemes those formats use.
//!
//! Only what apksigner needs is implemented: extract one private key + its
//! certificate chain, selected by alias or (if unambiguous) the sole entry.

use crate::crypto::{Certificate, PrivateKey};
use anyhow::{anyhow, bail, Context, Result};

/// A loaded key entry.
pub struct KeyEntry {
    pub key: PrivateKey,
    pub certificates: Vec<Certificate>,
    /// The alias the entry was stored under (for diagnostics / signer naming).
    pub alias: Option<String>,
}

/// Loads a key entry from a keystore file, autodetecting PKCS#12 vs JKS.
///
/// `alias` selects the entry; if `None` and the store has exactly one key
/// entry, that one is used.
pub fn load(
    data: &[u8],
    store_password: &str,
    key_password: Option<&str>,
    alias: Option<&str>,
) -> Result<KeyEntry> {
    if data.starts_with(&[0xfe, 0xed, 0xfe, 0xed]) {
        jks::load(data, store_password, key_password, alias)
    } else {
        pkcs12::load(data, store_password, key_password, alias)
    }
}

// ── PKCS#12 ───────────────────────────────────────────────────────────────

mod pkcs12 {
    use super::*;
    use crate::der_lite::{self, Tlv};

    // Bag / content OIDs.
    const OID_DATA: &str = "1.2.840.113549.1.7.1";
    const OID_ENCRYPTED_DATA: &str = "1.2.840.113549.1.7.6";
    const OID_KEY_BAG: &str = "1.2.840.113549.1.12.10.1.1";
    const OID_PKCS8_SHROUDED_KEY_BAG: &str = "1.2.840.113549.1.12.10.1.2";
    const OID_CERT_BAG: &str = "1.2.840.113549.1.12.10.1.3";
    const OID_X509_CERTIFICATE: &str = "1.2.840.113549.1.9.22.1";
    const OID_FRIENDLY_NAME: &str = "1.2.840.113549.1.9.20";

    pub fn load(
        data: &[u8],
        store_password: &str,
        key_password: Option<&str>,
        alias: Option<&str>,
    ) -> Result<KeyEntry> {
        let pfx = der_lite::sequence(data, 0)?;
        let mut p = der_lite::Cursor::new(pfx.content);
        let _version = p.tlv()?; // INTEGER 3
        let auth_safe_ci = p.tlv()?; // ContentInfo (data)
        // ContentInfo: SEQUENCE { contentType OID, [0] EXPLICIT content }
        let authsafe_bytes = content_info_data(auth_safe_ci)?;

        // AuthenticatedSafe ::= SEQUENCE OF ContentInfo
        let auth_safe = der_lite::sequence(authsafe_bytes, 0)?;
        let mut keys: Vec<(Vec<u8>, Option<String>)> = Vec::new();
        let mut certs: Vec<Vec<u8>> = Vec::new();

        let mut c = der_lite::Cursor::new(auth_safe.content);
        while let Some(ci) = c.try_tlv()? {
            let (content_type, content) = content_info(ci)?;
            let safe_contents = match content_type.as_str() {
                OID_DATA => der_lite::octet_string_inner(content)?.to_vec(),
                OID_ENCRYPTED_DATA => decrypt_encrypted_data(content, store_password)?,
                _ => continue,
            };
            parse_safe_contents(
                &safe_contents,
                store_password,
                key_password,
                &mut keys,
                &mut certs,
            )?;
        }

        // Select the key entry.
        let (key_der, key_alias) = select_key(&keys, alias)?;
        let key = PrivateKey::from_pkcs8_der(&key_der).context("PKCS#12 private key")?;
        let certificates = certs
            .iter()
            .map(|c| Certificate::from_der(c))
            .collect::<Result<Vec<_>>>()?;
        if certificates.is_empty() {
            bail!("keystore entry does not contain certificates");
        }
        Ok(KeyEntry {
            key,
            certificates: order_chain(certificates),
            alias: key_alias,
        })
    }

    fn select_key(
        keys: &[(Vec<u8>, Option<String>)],
        alias: Option<&str>,
    ) -> Result<(Vec<u8>, Option<String>)> {
        match alias {
            Some(want) => keys
                .iter()
                .find(|(_, a)| a.as_deref() == Some(want))
                .cloned()
                .ok_or_else(|| anyhow!("keystore does not contain key alias \"{want}\"")),
            None => match keys.len() {
                0 => bail!("keystore does not contain key entries"),
                1 => Ok(keys[0].clone()),
                _ => bail!(
                    "keystore contains multiple key entries; --ks-key-alias must select one"
                ),
            },
        }
    }

    /// Best-effort: leave the chain as found (leaf first is conventional).
    fn order_chain(certs: Vec<Certificate>) -> Vec<Certificate> {
        certs
    }

    fn content_info<'a>(ci: Tlv<'a>) -> Result<(String, &'a [u8])> {
        let mut c = der_lite::Cursor::new(ci.content);
        let oid = der_lite::oid_string(c.tlv()?)?;
        let explicit = c.tlv()?; // [0] EXPLICIT
        Ok((oid, explicit.content))
    }

    fn content_info_data<'a>(ci: Tlv<'a>) -> Result<&'a [u8]> {
        let (oid, content) = content_info(ci)?;
        if oid != OID_DATA {
            bail!("expected PKCS#12 data ContentInfo, got {oid}");
        }
        der_lite::octet_string_inner(content)
    }

    fn parse_safe_contents(
        safe_contents: &[u8],
        store_password: &str,
        key_password: Option<&str>,
        keys: &mut Vec<(Vec<u8>, Option<String>)>,
        certs: &mut Vec<Vec<u8>>,
    ) -> Result<()> {
        let seq = der_lite::sequence(safe_contents, 0)?;
        let mut c = der_lite::Cursor::new(seq.content);
        while let Some(bag) = c.try_tlv()? {
            let mut bc = der_lite::Cursor::new(bag.content);
            let bag_id = der_lite::oid_string(bc.tlv()?)?;
            let bag_value = bc.tlv()?; // [0] EXPLICIT
            let attrs = bc.try_tlv()?;
            let friendly = attrs.and_then(|a| friendly_name(a.content));
            match bag_id.as_str() {
                OID_KEY_BAG => {
                    keys.push((bag_value.content.to_vec(), friendly));
                }
                OID_PKCS8_SHROUDED_KEY_BAG => {
                    let pw = key_password.unwrap_or(store_password);
                    let key = decrypt_pkcs8_shrouded(bag_value.content, pw)?;
                    keys.push((key, friendly));
                }
                OID_CERT_BAG => {
                    if let Some(cert) = parse_cert_bag(bag_value.content)? {
                        certs.push(cert);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn friendly_name(attrs_content: &[u8]) -> Option<String> {
        let mut c = der_lite::Cursor::new(attrs_content);
        while let Ok(Some(attr)) = c.try_tlv() {
            let mut ac = der_lite::Cursor::new(attr.content);
            let oid = der_lite::oid_string(ac.tlv().ok()?).ok()?;
            if oid == OID_FRIENDLY_NAME {
                let set = ac.tlv().ok()?;
                let mut sc = der_lite::Cursor::new(set.content);
                let bmp = sc.tlv().ok()?;
                return Some(decode_bmp_string(bmp.content));
            }
        }
        None
    }

    fn decode_bmp_string(bytes: &[u8]) -> String {
        let mut s = String::new();
        for pair in bytes.chunks(2) {
            if pair.len() == 2 {
                let code = u16::from_be_bytes([pair[0], pair[1]]);
                if let Some(ch) = char::from_u32(code as u32) {
                    s.push(ch);
                }
            }
        }
        s
    }

    fn parse_cert_bag(content: &[u8]) -> Result<Option<Vec<u8>>> {
        // CertBag ::= SEQUENCE { certId OID, certValue [0] EXPLICIT OCTET STRING }
        let seq = der_lite::sequence(content, 0)?;
        let mut c = der_lite::Cursor::new(seq.content);
        let cert_id = der_lite::oid_string(c.tlv()?)?;
        if cert_id != OID_X509_CERTIFICATE {
            return Ok(None);
        }
        let explicit = c.tlv()?;
        let der = der_lite::octet_string_inner(explicit.content)?;
        Ok(Some(der.to_vec()))
    }

    /// Decrypts an `encryptedData` content (the cert bags in a debug keystore).
    fn decrypt_encrypted_data(content: &[u8], password: &str) -> Result<Vec<u8>> {
        // EncryptedData ::= SEQUENCE { version, EncryptedContentInfo }
        let seq = der_lite::sequence(content, 0)?;
        let mut c = der_lite::Cursor::new(seq.content);
        let _version = c.tlv()?;
        // EncryptedContentInfo ::= SEQUENCE { contentType OID, algo AlgId,
        //   [0] IMPLICIT encryptedContent OCTET STRING }
        let eci = c.tlv()?;
        let mut ec = der_lite::Cursor::new(eci.content);
        let _content_type = ec.tlv()?;
        let algo = ec.tlv()?;
        let encrypted = ec.tlv()?; // [0] implicit octet string content
        crate::pbe::decrypt(algo, encrypted.content, password)
    }

    /// Decrypts a pkcs8ShroudedKeyBag (`EncryptedPrivateKeyInfo`).
    fn decrypt_pkcs8_shrouded(content: &[u8], password: &str) -> Result<Vec<u8>> {
        // EncryptedPrivateKeyInfo ::= SEQUENCE { algo AlgId, encryptedData OCTET STRING }
        let seq = der_lite::sequence(content, 0)?;
        let mut c = der_lite::Cursor::new(seq.content);
        let algo = c.tlv()?;
        let encrypted = c.tlv()?;
        crate::pbe::decrypt(algo, der_lite::octet_string_inner_tlv(encrypted)?, password)
    }
}

// ── JKS ─────────────────────────────────────────────────────────────────

mod jks {
    use super::*;
    use crate::zip::{u16le, u32le};
    use sha1::{Digest, Sha1};

    const MAGIC: u32 = 0xfeed_feed;
    const PRIVATE_KEY_TAG: u32 = 1;
    const TRUSTED_CERT_TAG: u32 = 2;

    pub fn load(
        data: &[u8],
        store_password: &str,
        _key_password: Option<&str>,
        alias: Option<&str>,
    ) -> Result<KeyEntry> {
        if be32(data, 0) != MAGIC {
            bail!("not a JKS keystore");
        }
        let _version = be32(data, 4);
        let count = be32(data, 8) as usize;
        let mut pos = 12;
        let mut found: Option<(String, Vec<u8>, Vec<Vec<u8>>)> = None;
        for _ in 0..count {
            let tag = be32(data, pos);
            pos += 4;
            let (name, np) = read_utf(data, pos);
            pos = np;
            pos += 8; // creation date (long)
            match tag {
                PRIVATE_KEY_TAG => {
                    let key_len = be32(data, pos) as usize;
                    pos += 4;
                    let protected_key = &data[pos..pos + key_len];
                    pos += key_len;
                    let chain_count = be32(data, pos) as usize;
                    pos += 4;
                    let mut chain = Vec::new();
                    for _ in 0..chain_count {
                        let (_cert_type, ctp) = read_utf(data, pos);
                        pos = ctp;
                        let cert_len = be32(data, pos) as usize;
                        pos += 4;
                        chain.push(data[pos..pos + cert_len].to_vec());
                        pos += cert_len;
                    }
                    if alias.is_none() || alias == Some(name.as_str()) {
                        let key_der = decrypt_jks_key(protected_key, store_password)?;
                        found = Some((name, key_der, chain));
                    }
                }
                TRUSTED_CERT_TAG => {
                    let (_cert_type, ctp) = read_utf(data, pos);
                    pos = ctp;
                    let cert_len = be32(data, pos) as usize;
                    pos += 4 + cert_len;
                }
                _ => bail!("unrecognized JKS entry tag {tag}"),
            }
        }
        let (name, key_der, chain) = found.context("keystore does not contain a key entry")?;
        let key = PrivateKey::from_pkcs8_der(&key_der).context("JKS private key")?;
        let certificates = chain
            .iter()
            .map(|c| Certificate::from_der(c))
            .collect::<Result<Vec<_>>>()?;
        Ok(KeyEntry {
            key,
            certificates,
            alias: Some(name),
        })
    }

    fn be32(d: &[u8], o: usize) -> u32 {
        u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
    }

    fn read_utf(d: &[u8], o: usize) -> (String, usize) {
        let len = u16::from_be_bytes([d[o], d[o + 1]]) as usize;
        let s = String::from_utf8_lossy(&d[o + 2..o + 2 + len]).into_owned();
        (s, o + 2 + len)
    }

    /// JKS key protection: the Sun proprietary "keystream xor SHA-1" scheme.
    fn decrypt_jks_key(protected: &[u8], password: &str) -> Result<Vec<u8>> {
        // protected = AlgorithmId (SEQUENCE) wrapping the actual encrypted blob
        // in an EncryptedPrivateKeyInfo. The Sun JKS scheme is:
        //   encrypted = salt(20) || keystream-xored-plaintext || checksum(20)
        // The inner DER is EncryptedPrivateKeyInfo with the Sun OID
        // 1.3.6.1.4.1.42.2.17.1.1; the OCTET STRING holds the blob above.
        let _ = (u16le, u32le); // keep zip helpers referenced for parity
        let seq = crate::der_lite::sequence(protected, 0)?;
        let mut c = crate::der_lite::Cursor::new(seq.content);
        let _algo = c.tlv()?;
        let blob = crate::der_lite::octet_string_inner_tlv(c.tlv()?)?;
        if blob.len() < 40 {
            bail!("malformed JKS key blob");
        }
        let salt = &blob[..20];
        let encrypted = &blob[20..blob.len() - 20];
        let check = &blob[blob.len() - 20..];

        let pw_bytes = utf16be(password);
        let mut keystream = salt.to_vec();
        let mut plaintext = vec![0u8; encrypted.len()];
        let mut i = 0;
        while i < encrypted.len() {
            let mut h = Sha1::new();
            h.update(&pw_bytes);
            h.update(&keystream);
            let digest = h.finalize();
            keystream = digest.to_vec();
            let n = (encrypted.len() - i).min(20);
            for j in 0..n {
                plaintext[i + j] = encrypted[i + j] ^ digest[j];
            }
            i += n;
        }
        // Verify checksum.
        let mut h = Sha1::new();
        h.update(&pw_bytes);
        h.update(&plaintext);
        if h.finalize().as_slice() != check {
            bail!("JKS keystore password is incorrect");
        }
        Ok(plaintext)
    }

    fn utf16be(s: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(s.len() * 2);
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_be_bytes());
        }
        out
    }
}
