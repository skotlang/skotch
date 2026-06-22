//! v1 (JAR) signature verification, scoped to what `apksigner verify` needs:
//! recover the signer certificate from the PKCS#7 block, verify its signature
//! over the `.SF`, and check the `.SF` → MANIFEST.MF → entry digest chain.
//!
//! Only the apksigner-produced shape is fully verified (no signed attributes,
//! so the signature covers the `.SF` bytes directly). Third-party blocks with
//! signed attributes are detected and reported as unverifiable here.

use crate::base64;
use crate::crypto::{verify_signature, Certificate};
use crate::verify::SignerCert;
use crate::zip::{self, CdRecord, ZipSections};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// Result of v1 verification.
pub struct V1Result {
    pub certs: Vec<SignerCert>,
    /// APK signature scheme ids referenced by the `.SF`'s `X-Android-APK-Signed`
    /// attribute (the stripping-protection list).
    pub referenced_schemes: Vec<u32>,
}

/// Verifies the v1 signature, returning the signer certs + referenced schemes,
/// or `None` if the APK has no v1 signature.
pub fn verify_v1(apk: &[u8], sections: &ZipSections) -> Result<Option<V1Result>> {
    let records = zip::parse_central_directory(apk, sections)?;
    let lfh_section = &apk[..sections.cd_offset];

    let mut by_name: BTreeMap<String, &CdRecord> = BTreeMap::new();
    for r in &records {
        by_name.insert(r.name.clone(), r);
    }

    let manifest_cd = match by_name.get(crate::v1::MANIFEST_ENTRY_NAME) {
        Some(r) => *r,
        None => return Ok(None),
    };
    let manifest = entry_data(lfh_section, manifest_cd)?;

    // Locate signers: every META-INF/<name>.SF with a sibling signature block.
    let mut signer_certs = Vec::new();
    let mut referenced_schemes: Vec<u32> = Vec::new();
    let mut found_signer = false;
    for r in &records {
        let name = &r.name;
        let lower = name.to_lowercase();
        if !(lower.starts_with("meta-inf/") && lower.ends_with(".sf")) {
            continue;
        }
        found_signer = true;
        let base = &name[..name.len() - 3];
        let sf = entry_data(lfh_section, r)?;

        // Find the signature block (.RSA/.DSA/.EC).
        let block_cd = ["RSA", "DSA", "EC"]
            .iter()
            .find_map(|ext| by_name.get(&format!("{base}.{ext}")).copied());
        let block_cd = block_cd.with_context(|| format!("no signature block for {name}"))?;
        let block = entry_data(lfh_section, block_cd)?;

        let cert = verify_signature_block(&block, &sf)
            .with_context(|| format!("verifying signature block for {name}"))?;

        // Verify the .SF manifest digest and per-entry digests.
        verify_sf_against_manifest(&sf, &manifest)?;
        verify_manifest_entries(&manifest, &by_name, lfh_section)?;

        for id in referenced_schemes_from_sf(&sf) {
            if !referenced_schemes.contains(&id) {
                referenced_schemes.push(id);
            }
        }
        signer_certs.push(SignerCert {
            cert_der: cert,
            min_sdk_version: None,
            max_sdk_version: None,
        });
    }

    if !found_signer {
        return Ok(None);
    }
    Ok(Some(V1Result {
        certs: signer_certs,
        referenced_schemes,
    }))
}

/// Parses the `X-Android-APK-Signed` main attribute of a `.SF` into scheme ids.
fn referenced_schemes_from_sf(sf: &[u8]) -> Vec<u32> {
    crate::v1::parse_manifest_main_attributes(sf)
        .into_iter()
        .find(|(k, _)| k == "X-Android-APK-Signed")
        .map(|(_, v)| {
            v.split(',')
                .filter_map(|s| s.trim().parse::<u32>().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn entry_data(lfh_section: &[u8], cd: &CdRecord) -> Result<Vec<u8>> {
    let lfr = zip::parse_local_file_record(lfh_section, cd)?;
    lfr.uncompressed_data(lfh_section)
}

/// Parses a PKCS#7 SignedData block, verifies the signer signature over the
/// `.SF` bytes, and returns the signer certificate DER.
fn verify_signature_block(block: &[u8], sf: &[u8]) -> Result<Vec<u8>> {
    let content_info = der::read_sequence(block, 0)?;
    // ContentInfo: contentType OID, [0] EXPLICIT content.
    let (_oid, after_oid) = der::read_tlv(content_info.content, 0)?;
    let (explicit, _) = der::read_tlv(content_info.content, after_oid)?;
    let signed_data = der::read_sequence(explicit.content, 0)?;

    // SignedData: version, digestAlgorithms SET, encapContentInfo SEQ,
    // [0] certificates, [1]? crls, signerInfos SET.
    let sd = signed_data.content;
    let (_version, p1) = der::read_tlv(sd, 0)?;
    let (_digest_algs, p2) = der::read_tlv(sd, p1)?;
    let (_encap, p3) = der::read_tlv(sd, p2)?;
    // [0] IMPLICIT certificates (context-constructed tag 0xA0).
    let (certs_field, p4) = der::read_tlv(sd, p3)?;
    let mut next = p4;
    // Optional [1] crls.
    let (mut signer_infos_tlv, _) = der::read_tlv(sd, next)?;
    if signer_infos_tlv.tag == 0xA1 {
        next = der::tlv_end(sd, next)?;
        let (si, _) = der::read_tlv(sd, next)?;
        signer_infos_tlv = si;
    }

    // First certificate.
    let first_cert = der::read_tlv(certs_field.content, 0)?.0;
    let cert_der = der::tlv_bytes(certs_field.content, 0)?;
    let _ = first_cert;
    let cert = Certificate::from_der(cert_der)?;

    // First SignerInfo.
    let first_signer = der::read_tlv(signer_infos_tlv.content, 0)?.0;
    let si = first_signer.content;
    let (_version, q1) = der::read_tlv(si, 0)?;
    let (_sid, q2) = der::read_tlv(si, q1)?;
    let (_digest_alg, q3) = der::read_tlv(si, q2)?;
    let (maybe_signed_attrs, q4) = der::read_tlv(si, q3)?;
    if maybe_signed_attrs.tag == 0xA0 {
        bail!("PKCS#7 block uses signed attributes; not supported by this verifier");
    }
    // No signed attrs: maybe_signed_attrs is actually signatureAlgorithm.
    let (_sig_alg, q5) = (maybe_signed_attrs, q4);
    let (signature_tlv, _) = der::read_tlv(si, q5)?;
    let signature = signature_tlv.content;

    // Determine JCA signature algorithm from the cert's key + the digest.
    // apksigner blocks are SHA-256 for modern keys; try SHA-256 then SHA-1.
    let jca_candidates = jca_candidates_for_key(&cert);
    for jca in jca_candidates {
        if verify_signature(&cert.spki_der, jca, sf, signature)? {
            return Ok(cert.der);
        }
    }
    bail!("v1 signature did not verify");
}

fn jca_candidates_for_key(cert: &Certificate) -> &'static [&'static str] {
    use crate::crypto::KeyAlgorithm;
    match cert.key_algorithm {
        KeyAlgorithm::Rsa => &["SHA256withRSA", "SHA1withRSA", "SHA512withRSA"],
        KeyAlgorithm::Dsa => &["SHA256withDSA", "SHA1withDSA"],
        KeyAlgorithm::Ec => &["SHA256withECDSA", "SHA512withECDSA"],
    }
}

/// Checks the `.SF`'s `*-Digest-Manifest` main attribute against MANIFEST.MF.
fn verify_sf_against_manifest(sf: &[u8], manifest: &[u8]) -> Result<()> {
    let main = crate::v1::parse_manifest_main_attributes(sf);
    for (name, value) in &main {
        let alg = if name == "SHA-256-Digest-Manifest" {
            crate::crypto::DigestAlgorithm::Sha256
        } else if name == "SHA1-Digest-Manifest" {
            crate::crypto::DigestAlgorithm::Sha1
        } else {
            continue;
        };
        let expected = base64::decode(value).context("decoding .SF manifest digest")?;
        if alg.digest(manifest) != expected {
            bail!("MANIFEST.MF digest in .SF does not match");
        }
        return Ok(());
    }
    // No whole-manifest digest: not fatal (some signers only use per-entry).
    Ok(())
}

/// Checks each MANIFEST.MF section digest against the actual entry data.
fn verify_manifest_entries(
    manifest: &[u8],
    by_name: &BTreeMap<String, &CdRecord>,
    lfh_section: &[u8],
) -> Result<()> {
    for section in parse_manifest_sections(manifest) {
        let entry_name = match section.name {
            Some(n) => n,
            None => continue,
        };
        let cd = match by_name.get(&entry_name) {
            Some(r) => *r,
            None => bail!("MANIFEST.MF references missing entry {entry_name}"),
        };
        let data = entry_data(lfh_section, cd)?;
        for (attr, value) in &section.attrs {
            let alg = if attr == "SHA-256-Digest" {
                crate::crypto::DigestAlgorithm::Sha256
            } else if attr == "SHA1-Digest" {
                crate::crypto::DigestAlgorithm::Sha1
            } else {
                continue;
            };
            let expected = base64::decode(value).context("decoding entry digest")?;
            if alg.digest(&data) != expected {
                bail!("digest mismatch for {entry_name}");
            }
        }
    }
    Ok(())
}

struct ManifestSection {
    name: Option<String>,
    attrs: Vec<(String, String)>,
}

/// Splits a MANIFEST.MF into individual sections (skipping the main section).
fn parse_manifest_sections(manifest: &[u8]) -> Vec<ManifestSection> {
    let text = String::from_utf8_lossy(manifest);
    let normalized = text.replace("\r\n", "\n");
    let mut sections = Vec::new();
    let mut first = true;
    for block in normalized.split("\n\n") {
        if block.trim().is_empty() {
            continue;
        }
        if first {
            first = false;
            // Skip the main section.
            continue;
        }
        let attrs = parse_attrs(block);
        let name = attrs
            .iter()
            .find(|(k, _)| k == "Name")
            .map(|(_, v)| v.clone());
        let other: Vec<(String, String)> = attrs.into_iter().filter(|(k, _)| k != "Name").collect();
        sections.push(ManifestSection { name, attrs: other });
    }
    sections
}

fn parse_attrs(block: &str) -> Vec<(String, String)> {
    let mut attrs: Vec<(String, String)> = Vec::new();
    for line in block.split('\n') {
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(last) = attrs.last_mut() {
                last.1.push_str(rest);
            }
            continue;
        }
        if let Some(colon) = line.find(": ") {
            attrs.push((line[..colon].to_string(), line[colon + 2..].to_string()));
        }
    }
    attrs
}

/// A minimal DER TLV reader, just enough to walk PKCS#7 SignedData.
mod der {
    use anyhow::{bail, Result};

    pub struct Tlv<'a> {
        pub tag: u8,
        pub content: &'a [u8],
        /// Total bytes consumed (header + content).
        pub total_len: usize,
    }

    /// Reads the TLV starting at `offset` within `data`.
    pub fn read_tlv<'a>(data: &'a [u8], offset: usize) -> Result<(Tlv<'a>, usize)> {
        if offset + 2 > data.len() {
            bail!("truncated DER at {offset}");
        }
        let tag = data[offset];
        let len_byte = data[offset + 1];
        let (content_start, length) = if len_byte & 0x80 == 0 {
            (offset + 2, len_byte as usize)
        } else {
            let num = (len_byte & 0x7f) as usize;
            if num == 0 || num > 4 || offset + 2 + num > data.len() {
                bail!("invalid DER length at {offset}");
            }
            let mut len = 0usize;
            for i in 0..num {
                len = (len << 8) | data[offset + 2 + i] as usize;
            }
            (offset + 2 + num, len)
        };
        if content_start + length > data.len() {
            bail!("DER content exceeds buffer at {offset}");
        }
        let total = (content_start - offset) + length;
        Ok((
            Tlv {
                tag,
                content: &data[content_start..content_start + length],
                total_len: total,
            },
            offset + total,
        ))
    }

    /// Reads a SEQUENCE TLV (tag 0x30) at `offset`.
    pub fn read_sequence<'a>(data: &'a [u8], offset: usize) -> Result<Tlv<'a>> {
        let (tlv, _) = read_tlv(data, offset)?;
        if tlv.tag != 0x30 && tlv.tag != 0xA0 {
            bail!("expected SEQUENCE, got tag {:#x}", tlv.tag);
        }
        Ok(tlv)
    }

    /// Returns the raw bytes (header + content) of the TLV at `offset`.
    pub fn tlv_bytes(data: &[u8], offset: usize) -> Result<&[u8]> {
        let (tlv, _) = read_tlv(data, offset)?;
        Ok(&data[offset..offset + tlv.total_len])
    }

    /// Returns the offset just past the TLV at `offset`.
    pub fn tlv_end(data: &[u8], offset: usize) -> Result<usize> {
        let (tlv, _) = read_tlv(data, offset)?;
        Ok(offset + tlv.total_len)
    }
}
