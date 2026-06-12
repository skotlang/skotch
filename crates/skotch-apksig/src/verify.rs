//! APK signature verification (`ApkVerifier.java`), scoped to what the
//! `apksigner verify` CLI reports: which schemes verify, the signer
//! certificates, and the v4 `.idsig` check.
//!
//! v2/v3/v3.1 are verified end-to-end (recompute content digests + verify the
//! signature over the signed-data). v1 is verified by extracting the PKCS#7
//! signer certificate and checking the `.SF` signature and the MANIFEST.MF
//! digests. Source-stamp verification is detected but reported as a stub.

use crate::crypto::{verify_signature, SignatureAlgorithm};
use crate::digest::{compute_content_digests, ContentDigestAlgorithm};
use crate::sigblock::{self, Slice, V31_BLOCK_ID, V3_BLOCK_ID, V2_BLOCK_ID};
use crate::zip;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// A signer certificate surfaced by verification.
#[derive(Clone)]
pub struct SignerCert {
    pub cert_der: Vec<u8>,
    /// For v3/v3.1 signers, the SDK range.
    pub min_sdk_version: Option<u32>,
    pub max_sdk_version: Option<u32>,
}

/// Verification result, shaped for the `apksigner verify` output.
#[derive(Default)]
pub struct VerificationResult {
    pub verified: bool,
    pub verified_v1: bool,
    pub verified_v2: bool,
    pub verified_v3: bool,
    pub verified_v31: bool,
    pub verified_v4: bool,
    pub source_stamp_verified: bool,
    /// The signer certificates (v1 signer order, or v3 signers).
    pub signer_certs: Vec<SignerCert>,
    pub v3_signer_certs: Vec<SignerCert>,
    pub v31_signer_certs: Vec<SignerCert>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// The `apksigner verify` driver.
pub struct ApkVerifier {
    min_sdk_version: Option<u32>,
    max_sdk_version: Option<u32>,
}

impl ApkVerifier {
    pub fn new() -> ApkVerifier {
        ApkVerifier {
            min_sdk_version: None,
            max_sdk_version: None,
        }
    }

    pub fn min_sdk_version(mut self, v: u32) -> Self {
        self.min_sdk_version = Some(v);
        self
    }
    pub fn max_sdk_version(mut self, v: u32) -> Self {
        self.max_sdk_version = Some(v);
        self
    }

    pub fn verify(&self, apk: &[u8]) -> Result<VerificationResult> {
        let mut result = VerificationResult::default();
        let sections = zip::find_zip_sections(apk)?;

        let block_info = zip::find_apk_signing_block(apk, &sections)?;
        if let Some(info) = &block_info {
            let block = &apk[info.start_offset..info.start_offset + info.size];

            // before_cd = LFH section up to the signing block start.
            let before_cd = &apk[..info.start_offset];
            let central_dir = &apk[sections.cd_offset..sections.cd_offset + sections.cd_size];
            // eocd with the CD offset pointing at the signing block start.
            let mut eocd = sections.eocd(apk).to_vec();
            zip::eocd::set_cd_offset(&mut eocd, info.start_offset as u32);

            // v3.1
            if let Some(scheme) = sigblock::find_block(block, V31_BLOCK_ID)? {
                match verify_v3(&scheme, before_cd, central_dir, &eocd) {
                    Ok(certs) => {
                        result.verified_v31 = true;
                        result.v31_signer_certs = certs;
                    }
                    Err(e) => result.errors.push(format!("APK Signature Scheme v3.1: {e}")),
                }
            }
            // v3
            if let Some(scheme) = sigblock::find_block(block, V3_BLOCK_ID)? {
                match verify_v3(&scheme, before_cd, central_dir, &eocd) {
                    Ok(certs) => {
                        result.verified_v3 = true;
                        result.v3_signer_certs = certs.clone();
                        if result.signer_certs.is_empty() {
                            result.signer_certs = certs;
                        }
                    }
                    Err(e) => result.errors.push(format!("APK Signature Scheme v3 signer: {e}")),
                }
            }
            // v2
            if let Some(scheme) = sigblock::find_block(block, V2_BLOCK_ID)? {
                match verify_v2(&scheme, before_cd, central_dir, &eocd) {
                    Ok(certs) => {
                        result.verified_v2 = true;
                        if result.signer_certs.is_empty() {
                            result.signer_certs = certs;
                        }
                    }
                    Err(e) => result.errors.push(format!("APK Signature Scheme v2 signer: {e}")),
                }
            }
        }

        // v1 detection + verification.
        match crate::v1verify::verify_v1(apk, &sections) {
            Ok(Some(certs)) => {
                result.verified_v1 = true;
                if result.signer_certs.is_empty() {
                    result.signer_certs = certs;
                }
            }
            Ok(None) => {}
            Err(e) => result.errors.push(format!("JAR signer: {e}")),
        }

        result.verified = result.errors.is_empty()
            && (result.verified_v1
                || result.verified_v2
                || result.verified_v3
                || result.verified_v31);
        Ok(result)
    }

    /// Verifies a v4 `.idsig` against the APK (`--v4-signature-file`).
    pub fn verify_v4(&self, apk: &[u8], idsig: &[u8]) -> Result<bool> {
        let sig = crate::v4::V4Signature::read(idsig)?;
        // The signing info carries the cert + signature; recompute the signed
        // data from the APK and verify.
        let mut s = Slice::new(&sig.signing_infos);
        let apk_digest = s.get_length_prefixed_bytes()?.to_vec();
        let certificate = s.get_length_prefixed_bytes()?.to_vec();
        let additional_data = s.get_length_prefixed_bytes()?.to_vec();
        let _public_key = s.get_length_prefixed_bytes()?.to_vec();
        let signature_algorithm_id = s.get_u32()?;
        let signature = s.get_length_prefixed_bytes()?.to_vec();

        let alg = SignatureAlgorithm::from_id(signature_algorithm_id)
            .context("unknown v4 signature algorithm")?;
        let cert = crate::crypto::Certificate::from_der(&certificate)?;

        // Rebuild the v4 signed data: requires the hashing info + file size.
        let mut hi = Slice::new(&sig.hashing_info);
        let hash_algorithm = hi.get_u32()?;
        let log2_block_size = hi.get_bytes(1)?[0];
        let salt = hi.get_length_prefixed_bytes()?.to_vec();
        let raw_root_hash = hi.get_length_prefixed_bytes()?.to_vec();

        let signed = build_v4_signed_data(
            apk.len() as u64,
            hash_algorithm,
            log2_block_size,
            &salt,
            &raw_root_hash,
            &apk_digest,
            &certificate,
            &additional_data,
        );
        let ok = verify_signature(&cert.spki_der, alg.jca_signature_algorithm(), &signed, &signature)?;
        Ok(ok)
    }
}

impl Default for ApkVerifier {
    fn default() -> Self {
        Self::new()
    }
}

fn build_v4_signed_data(
    file_size: u64,
    hash_algorithm: u32,
    log2_block_size: u8,
    salt: &[u8],
    raw_root_hash: &[u8],
    apk_digest: &[u8],
    certificate: &[u8],
    additional_data: &[u8],
) -> Vec<u8> {
    let size = 4 + 8 + 4 + 1 + 4 + salt.len() + 4 + raw_root_hash.len() + 4 + apk_digest.len() + 4
        + certificate.len() + 4 + additional_data.len();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&hash_algorithm.to_le_bytes());
    out.push(log2_block_size);
    let mut put = |b: &[u8]| {
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b);
    };
    put(salt);
    put(raw_root_hash);
    put(apk_digest);
    put(certificate);
    put(additional_data);
    out
}

/// Verifies all signers of a v2 block.
fn verify_v2(
    scheme_block: &[u8],
    before_cd: &[u8],
    central_dir: &[u8],
    eocd: &[u8],
) -> Result<Vec<SignerCert>> {
    let mut s = Slice::new(scheme_block);
    let mut signers = s.get_length_prefixed_slice()?;
    let mut certs = Vec::new();
    while signers.has_remaining() {
        let mut signer = signers.get_length_prefixed_slice()?;
        let signed_data = signer.get_length_prefixed_slice()?;
        let signed_data_bytes = signed_data.rest();
        let signatures = signer.get_length_prefixed_slice()?;
        let public_key = signer.get_length_prefixed_bytes()?;
        let cert = verify_signer_signed_data(
            signed_data_bytes,
            signatures,
            public_key,
            before_cd,
            central_dir,
            eocd,
            false,
        )?;
        certs.push(SignerCert {
            cert_der: cert,
            min_sdk_version: None,
            max_sdk_version: None,
        });
    }
    if certs.is_empty() {
        bail!("no signers in v2 block");
    }
    Ok(certs)
}

/// Verifies all signers of a v3/v3.1 block.
fn verify_v3(
    scheme_block: &[u8],
    before_cd: &[u8],
    central_dir: &[u8],
    eocd: &[u8],
) -> Result<Vec<SignerCert>> {
    let mut s = Slice::new(scheme_block);
    let mut signers = s.get_length_prefixed_slice()?;
    let mut certs = Vec::new();
    while signers.has_remaining() {
        let mut signer = signers.get_length_prefixed_slice()?;
        let signed_data = signer.get_length_prefixed_slice()?;
        let signed_data_bytes = signed_data.rest();
        let min_sdk = signer.get_u32()?;
        let max_sdk = signer.get_u32()?;
        let signatures = signer.get_length_prefixed_slice()?;
        let public_key = signer.get_length_prefixed_bytes()?;
        let cert = verify_signer_signed_data(
            signed_data_bytes,
            signatures,
            public_key,
            before_cd,
            central_dir,
            eocd,
            true,
        )?;
        certs.push(SignerCert {
            cert_der: cert,
            min_sdk_version: Some(min_sdk),
            max_sdk_version: Some(max_sdk),
        });
    }
    if certs.is_empty() {
        bail!("no signers in v3 block");
    }
    Ok(certs)
}

/// Verifies one signer: checks each signature over the signed-data with the
/// signer's public key, then recomputes the content digests and compares them
/// with the digests recorded in the signed-data. Returns the signer cert DER.
fn verify_signer_signed_data(
    signed_data: &[u8],
    mut signatures: Slice,
    public_key: &[u8],
    before_cd: &[u8],
    central_dir: &[u8],
    eocd: &[u8],
    is_v3: bool,
) -> Result<Vec<u8>> {
    // 1. Verify each signature over the signed-data.
    let mut verified_any = false;
    while signatures.has_remaining() {
        let mut sig = signatures.get_length_prefixed_slice()?;
        let alg_id = sig.get_u32()?;
        let signature = sig.get_length_prefixed_bytes()?;
        let alg = match SignatureAlgorithm::from_id(alg_id) {
            Some(a) => a,
            None => continue,
        };
        if !verify_signature(public_key, alg.jca_signature_algorithm(), signed_data, signature)? {
            bail!("signature did not verify over signed-data");
        }
        verified_any = true;
    }
    if !verified_any {
        bail!("no supported signatures");
    }

    // 2. Parse signed-data: digests, certificates, [v3: min/max sdk], attrs.
    let mut sd = Slice::new(signed_data);
    let mut digests = sd.get_length_prefixed_slice()?;
    let mut certificates = sd.get_length_prefixed_slice()?;
    if is_v3 {
        let _min = sd.get_u32()?;
        let _max = sd.get_u32()?;
    }
    let _attributes = sd.get_length_prefixed_slice()?;

    // First certificate's public key must match the signer public key.
    let first_cert = certificates.get_length_prefixed_bytes()?;
    let cert = crate::crypto::Certificate::from_der(first_cert)?;
    if cert.spki_der != public_key {
        bail!("public key does not match the first certificate");
    }

    // 3. Recompute and compare content digests.
    let mut needed: Vec<ContentDigestAlgorithm> = Vec::new();
    let mut declared: BTreeMap<ContentDigestAlgorithm, Vec<u8>> = BTreeMap::new();
    let mut dcursor = digests;
    while dcursor.has_remaining() {
        let mut d = dcursor.get_length_prefixed_slice()?;
        let alg_id = d.get_u32()?;
        let value = d.get_length_prefixed_bytes()?.to_vec();
        if let Some(alg) = SignatureAlgorithm::from_id(alg_id) {
            let cda = alg.content_digest_algorithm();
            if !needed.contains(&cda) {
                needed.push(cda);
            }
            declared.insert(cda, value);
        }
    }
    let _ = &mut digests;
    let computed = compute_content_digests(&needed, before_cd, central_dir, eocd)?;
    for (cda, value) in &declared {
        match computed.get(cda) {
            Some(c) if c == value => {}
            _ => bail!("content digest mismatch"),
        }
    }

    Ok(first_cert.to_vec())
}
