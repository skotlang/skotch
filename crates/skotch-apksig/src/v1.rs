//! v1 JAR signing: MANIFEST.MF / .SF emission and the PKCS#7 signature block
//! (`V1SchemeSigner.java`, `ManifestWriter.java`, `SignatureFileWriter.java`).
//!
//! Byte-for-byte fidelity here is what makes v1 golden APKs reproduce: the
//! 70-column line wrapping, CRLF terminators, alphabetical attribute ordering
//! and the 1024-byte `.SF` padding workaround all match the Java writers.

use crate::base64;
use crate::crypto::{Certificate, DigestAlgorithm, PrivateKey};
use crate::pkcs7;
use anyhow::{bail, Result};
use std::collections::BTreeMap;

pub const MANIFEST_ENTRY_NAME: &str = "META-INF/MANIFEST.MF";
pub const CREATED_BY_DEFAULT: &str = "1.0 (Android)";

const MAX_LINE_LENGTH: usize = 70;

/// A signer for the v1 scheme.
pub struct V1SignerConfig<'a> {
    /// Output basename (`META-INF/<name>.SF`).
    pub name: String,
    pub key: &'a PrivateKey,
    pub certificates: &'a [Certificate],
    pub digest_algorithm: DigestAlgorithm,
    pub deterministic_dsa: bool,
}

/// Result of v1 signing: ordered list of `(entry_name, contents)` to add.
pub struct V1Output {
    pub entries: Vec<(String, Vec<u8>)>,
}

/// `V1SchemeSigner.sign`: generate MANIFEST.MF and, per signer, .SF + block.
pub fn sign(
    signers: &[V1SignerConfig],
    jar_entry_digest_algorithm: DigestAlgorithm,
    jar_entry_digests: &BTreeMap<String, Vec<u8>>,
    apk_signing_scheme_ids: &[u32],
    source_manifest_main_attrs: Option<&[(String, String)]>,
    created_by: &str,
) -> Result<V1Output> {
    if signers.is_empty() {
        bail!("At least one signer config must be provided");
    }
    let manifest = generate_manifest_file(
        jar_entry_digest_algorithm,
        jar_entry_digests,
        source_manifest_main_attrs,
    );
    sign_manifest(
        signers,
        jar_entry_digest_algorithm,
        apk_signing_scheme_ids,
        created_by,
        &manifest,
    )
}

/// The generated MANIFEST.MF plus the per-entry section bytes (needed to
/// digest individual sections into the .SF file).
pub struct OutputManifest {
    pub contents: Vec<u8>,
    /// Map of entry name -> the exact bytes of that section in the manifest.
    pub individual_sections: BTreeMap<String, Vec<u8>>,
}

/// `V1SchemeSigner.generateManifestFile`.
pub fn generate_manifest_file(
    digest_algorithm: DigestAlgorithm,
    jar_entry_digests: &BTreeMap<String, Vec<u8>>,
    source_manifest_main_attrs: Option<&[(String, String)]>,
) -> OutputManifest {
    let mut out = Vec::new();

    // Main section: copy from source manifest if present, else default.
    let mut main_attrs: Vec<(String, String)> = match source_manifest_main_attrs {
        Some(attrs) => attrs.to_vec(),
        None => vec![("Manifest-Version".to_string(), "1.0".to_string())],
    };
    if source_manifest_main_attrs.is_none() {
        // already has Manifest-Version
    } else if !main_attrs.iter().any(|(k, _)| k == "Manifest-Version") {
        main_attrs.insert(0, ("Manifest-Version".to_string(), "1.0".to_string()));
    }
    write_main_section(&mut out, &main_attrs, "Manifest-Version");

    let entry_digest_attr = entry_digest_attribute_name(digest_algorithm);
    let mut sections = BTreeMap::new();
    for (entry_name, digest) in jar_entry_digests {
        let mut section = Vec::new();
        write_individual_section(
            &mut section,
            entry_name,
            &[(entry_digest_attr.to_string(), base64::encode(digest))],
        );
        out.extend_from_slice(&section);
        sections.insert(entry_name.clone(), section);
    }

    OutputManifest {
        contents: out,
        individual_sections: sections,
    }
}

/// `V1SchemeSigner.signManifest`.
pub fn sign_manifest(
    signers: &[V1SignerConfig],
    digest_algorithm: DigestAlgorithm,
    apk_signing_scheme_ids: &[u32],
    created_by: &str,
    manifest: &OutputManifest,
) -> Result<V1Output> {
    let mut entries = Vec::with_capacity(2 * signers.len() + 1);
    let sf_bytes = generate_signature_file(apk_signing_scheme_ids, digest_algorithm, created_by, manifest);
    for signer in signers {
        let block = generate_signature_block(signer, &sf_bytes)?;
        entries.push((format!("META-INF/{}.SF", signer.name), sf_bytes.clone()));
        let key_alg = signer.certificates[0].key_algorithm.jca_name();
        entries.push((format!("META-INF/{}.{}", signer.name, key_alg), block));
    }
    entries.push((MANIFEST_ENTRY_NAME.to_string(), manifest.contents.clone()));
    Ok(V1Output { entries })
}

/// `V1SchemeSigner.generateSignatureFile`.
fn generate_signature_file(
    apk_signing_scheme_ids: &[u32],
    digest_algorithm: DigestAlgorithm,
    created_by: &str,
    manifest: &OutputManifest,
) -> Vec<u8> {
    let mut main_attrs: Vec<(String, String)> = vec![
        ("Signature-Version".to_string(), "1.0".to_string()),
        ("Created-By".to_string(), created_by.to_string()),
    ];
    if !apk_signing_scheme_ids.is_empty() {
        let value = apk_signing_scheme_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        main_attrs.push(("X-Android-APK-Signed".to_string(), value));
    }
    let manifest_digest = digest_algorithm.digest(&manifest.contents);
    main_attrs.push((
        manifest_digest_attribute_name(digest_algorithm).to_string(),
        base64::encode(&manifest_digest),
    ));

    let mut out = Vec::new();
    write_main_section(&mut out, &main_attrs, "Signature-Version");

    let entry_digest_attr = entry_digest_attribute_name(digest_algorithm);
    for (section_name, section_bytes) in &manifest.individual_sections {
        let section_digest = digest_algorithm.digest(section_bytes);
        write_individual_section(
            &mut out,
            section_name,
            &[(entry_digest_attr.to_string(), base64::encode(&section_digest))],
        );
    }

    // Android <= 1.6 bug workaround: append a CRLF if size is a multiple of 1024.
    if !out.is_empty() && out.len() % 1024 == 0 {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// `V1SchemeSigner.generateSignatureBlock` -> PKCS#7 DER.
fn generate_signature_block(signer: &V1SignerConfig, sf_bytes: &[u8]) -> Result<Vec<u8>> {
    let jca_alg = jca_signature_algorithm(
        signer.certificates[0].key_algorithm,
        signer.digest_algorithm,
        signer.deterministic_dsa,
    )?;
    let signature = signer.key.sign(&jca_alg, sf_bytes)?;
    pkcs7::generate_pkcs7(
        &signature,
        signer.certificates,
        signer.digest_algorithm,
        signer.certificates[0].key_algorithm,
    )
}

/// JCA signature algorithm name for the PKCS#7 block
/// (`AlgorithmIdentifier.getSignerInfoSignatureAlgorithm`).
fn jca_signature_algorithm(
    key_alg: crate::crypto::KeyAlgorithm,
    digest: DigestAlgorithm,
    deterministic_dsa: bool,
) -> Result<String> {
    use crate::crypto::KeyAlgorithm;
    let prefix = match digest {
        DigestAlgorithm::Sha1 => "SHA1",
        DigestAlgorithm::Sha256 => "SHA256",
    };
    Ok(match key_alg {
        KeyAlgorithm::Rsa => format!("{prefix}withRSA"),
        KeyAlgorithm::Dsa => {
            if deterministic_dsa {
                format!("{prefix}withDetDSA")
            } else {
                format!("{prefix}withDSA")
            }
        }
        KeyAlgorithm::Ec => format!("{prefix}withECDSA"),
    })
}

// ── Manifest text writers (ManifestWriter / SignatureFileWriter) ──────────

/// `ManifestWriter.writeMainSection` / `SignatureFileWriter.writeMainSection`:
/// write `first_attr` first, then the remaining attributes sorted by name.
fn write_main_section(out: &mut Vec<u8>, attrs: &[(String, String)], first_attr: &str) {
    let first_value = attrs
        .iter()
        .find(|(k, _)| k == first_attr)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "1.0".to_string());
    write_attribute(out, first_attr, &first_value);
    if attrs.len() > 1 {
        let mut sorted: Vec<&(String, String)> = attrs.iter().filter(|(k, _)| k != first_attr).collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in sorted {
            write_attribute(out, k, v);
        }
    }
    out.extend_from_slice(b"\r\n");
}

/// `ManifestWriter.writeIndividualSection`: "Name: <name>" then sorted attrs.
fn write_individual_section(out: &mut Vec<u8>, name: &str, attrs: &[(String, String)]) {
    write_attribute(out, "Name", name);
    if !attrs.is_empty() {
        let mut sorted: Vec<&(String, String)> = attrs.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in sorted {
            write_attribute(out, k, v);
        }
    }
    out.extend_from_slice(b"\r\n");
}

/// `ManifestWriter.writeAttribute` / `writeLine`: `<name>: <value>` wrapped at
/// 70 columns with continuation lines beginning with a single space.
fn write_attribute(out: &mut Vec<u8>, name: &str, value: &str) {
    let line = format!("{name}: {value}");
    let bytes = line.as_bytes();
    let mut offset = 0;
    let mut first_line = true;
    let mut remaining = bytes.len();
    while remaining > 0 {
        let chunk_len = if first_line {
            remaining.min(MAX_LINE_LENGTH)
        } else {
            out.extend_from_slice(b"\r\n ");
            remaining.min(MAX_LINE_LENGTH - 1)
        };
        out.extend_from_slice(&bytes[offset..offset + chunk_len]);
        offset += chunk_len;
        remaining -= chunk_len;
        first_line = false;
    }
    out.extend_from_slice(b"\r\n");
}

fn entry_digest_attribute_name(d: DigestAlgorithm) -> &'static str {
    match d {
        DigestAlgorithm::Sha1 => "SHA1-Digest",
        DigestAlgorithm::Sha256 => "SHA-256-Digest",
    }
}

fn manifest_digest_attribute_name(d: DigestAlgorithm) -> &'static str {
    match d {
        DigestAlgorithm::Sha1 => "SHA1-Digest-Manifest",
        DigestAlgorithm::Sha256 => "SHA-256-Digest-Manifest",
    }
}

/// Sanitizes a signer name for use as a `META-INF/<name>.SF` basename
/// (`V1SchemeSigner.getSafeSignerName`): upper-cased, at most 8 characters,
/// with anything outside `A-Z0-9_-` replaced by `_`.
pub fn safe_signer_name(name: &str) -> String {
    assert!(!name.is_empty(), "empty signer name");
    name.to_uppercase()
        .chars()
        .take(8)
        .map(|c| {
            if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Whether a JAR entry must be listed in the signed manifest
/// (`isJarEntryDigestNeededInManifest`).
pub fn is_jar_entry_digest_needed_in_manifest(entry_name: &str) -> bool {
    if entry_name.ends_with('/') {
        return false;
    }
    if !entry_name.starts_with("META-INF/") {
        return true;
    }
    if entry_name[("META-INF/".len())..].contains('/') {
        return true;
    }
    let file_name = entry_name["META-INF/".len()..].to_lowercase();
    !(file_name == "manifest.mf"
        || file_name.ends_with(".sf")
        || file_name.ends_with(".rsa")
        || file_name.ends_with(".dsa")
        || file_name.ends_with(".ec")
        || file_name.starts_with("sig-"))
}

/// Parses the main-section attributes of an existing MANIFEST.MF, preserving
/// header case and (insertion) order. Continuation lines (leading space) are
/// joined. Parsing stops at the first blank line.
pub fn parse_manifest_main_attributes(bytes: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(bytes);
    let mut attrs: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for raw_line in text.split("\r\n").flat_map(|l| l.split('\n')) {
        if raw_line.is_empty() {
            break;
        }
        if let Some(rest) = raw_line.strip_prefix(' ') {
            if let Some((_, v)) = current.as_mut() {
                v.push_str(rest);
            }
            continue;
        }
        if let Some(done) = current.take() {
            attrs.push(done);
        }
        if let Some(colon) = raw_line.find(": ") {
            let name = raw_line[..colon].to_string();
            let value = raw_line[colon + 2..].to_string();
            current = Some((name, value));
        }
    }
    if let Some(done) = current.take() {
        attrs.push(done);
    }
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_wrapping_at_70() {
        let mut out = Vec::new();
        let long = "x".repeat(100);
        write_attribute(&mut out, "Name", &long);
        let text = String::from_utf8(out).unwrap();
        // First line: "Name: " + 64 x's = 70 chars, then CRLF + space + rest.
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines[0].len(), 70);
        assert!(lines[1].starts_with(' '));
        assert!(lines[1].len() <= 70);
    }

    #[test]
    fn manifest_digest_needed_predicate() {
        assert!(is_jar_entry_digest_needed_in_manifest("classes.dex"));
        assert!(!is_jar_entry_digest_needed_in_manifest("META-INF/MANIFEST.MF"));
        assert!(!is_jar_entry_digest_needed_in_manifest("META-INF/CERT.SF"));
        assert!(!is_jar_entry_digest_needed_in_manifest("META-INF/CERT.RSA"));
        assert!(is_jar_entry_digest_needed_in_manifest("META-INF/services/foo"));
        assert!(!is_jar_entry_digest_needed_in_manifest("res/"));
    }

    #[test]
    fn main_section_sorts_after_first() {
        let mut out = Vec::new();
        write_main_section(
            &mut out,
            &[
                ("Signature-Version".into(), "1.0".into()),
                ("X-Android-APK-Signed".into(), "2".into()),
                ("Created-By".into(), "1.0 (Android)".into()),
            ],
            "Signature-Version",
        );
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            "Signature-Version: 1.0\r\nCreated-By: 1.0 (Android)\r\nX-Android-APK-Signed: 2\r\n\r\n"
        );
    }
}
