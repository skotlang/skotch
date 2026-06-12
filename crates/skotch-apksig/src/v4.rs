//! APK Signature Scheme v4 (`.idsig`) signer and parser
//! (`V4SchemeSigner.java` + `V4Signature.java`).
//!
//! v4 is an fs-verity Merkle-tree signature stored in a sidecar `.idsig`
//! file; it never changes the APK bytes. It signs a digest of the v2/v3
//! content-digest plus the verity root hash.

use crate::crypto::{Certificate, PrivateKey, SignatureAlgorithm};
use crate::digest::{self, ContentDigestAlgorithm};
use crate::sigblock::{self, V2_BLOCK_ID, V31_BLOCK_ID, V3_BLOCK_ID};
use crate::zip;
use anyhow::{bail, Context, Result};

pub const CURRENT_VERSION: u32 = 2;
pub const HASHING_ALGORITHM_SHA256: u32 = 1;
pub const LOG2_BLOCK_SIZE_4096: u8 = 12;

/// fs-verity hashing parameters (`V4Signature.HashingInfo`).
pub struct HashingInfo {
    pub hash_algorithm: u32,
    pub log2_block_size: u8,
    pub salt: Vec<u8>,
    pub raw_root_hash: Vec<u8>,
}

impl HashingInfo {
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.hash_algorithm.to_le_bytes());
        out.push(self.log2_block_size);
        write_len_bytes(&mut out, &self.salt);
        write_len_bytes(&mut out, &self.raw_root_hash);
        out
    }
}

/// Per-signer authentication block (`V4Signature.SigningInfo`).
pub struct SigningInfo {
    pub apk_digest: Vec<u8>,
    pub certificate: Vec<u8>,
    pub additional_data: Vec<u8>,
    pub public_key: Vec<u8>,
    pub signature_algorithm_id: i32,
    pub signature: Vec<u8>,
}

impl SigningInfo {
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_len_bytes(&mut out, &self.apk_digest);
        write_len_bytes(&mut out, &self.certificate);
        write_len_bytes(&mut out, &self.additional_data);
        write_len_bytes(&mut out, &self.public_key);
        out.extend_from_slice(&self.signature_algorithm_id.to_le_bytes());
        write_len_bytes(&mut out, &self.signature);
        out
    }
}

fn write_len_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// `V4Signature.getSignedData`: the bytes that the v4 signature covers.
fn v4_signed_data(file_size: u64, hashing: &HashingInfo, info: &SigningInfo) -> Vec<u8> {
    let size = 4
        + 8
        + 4
        + 1
        + 4
        + hashing.salt.len()
        + 4
        + hashing.raw_root_hash.len()
        + 4
        + info.apk_digest.len()
        + 4
        + info.certificate.len()
        + 4
        + info.additional_data.len();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&hashing.hash_algorithm.to_le_bytes());
    out.push(hashing.log2_block_size);
    write_len_bytes(&mut out, &hashing.salt);
    write_len_bytes(&mut out, &hashing.raw_root_hash);
    write_len_bytes(&mut out, &info.apk_digest);
    write_len_bytes(&mut out, &info.certificate);
    write_len_bytes(&mut out, &info.additional_data);
    out
}

/// The complete serialized `.idsig` file (V4Signature + hash tree).
pub fn generate_v4_signature(
    apk: &[u8],
    key: &PrivateKey,
    certificate: &Certificate,
    signature_algorithm: SignatureAlgorithm,
) -> Result<Vec<u8>> {
    let file_size = apk.len() as u64;

    // Best v2/v3 content digest for the apkDigest field.
    let apk_digest = best_apk_digest(apk).context("computing v4 apk digest")?;

    // Verity tree + root hash over the whole file (salt = none).
    let tree = digest::verity_tree(&[apk], None);
    let root_hash = digest::verity_root_from_tree(&tree, None);

    let hashing = HashingInfo {
        hash_algorithm: HASHING_ALGORITHM_SHA256,
        log2_block_size: LOG2_BLOCK_SIZE_4096,
        salt: Vec::new(),
        raw_root_hash: root_hash,
    };

    let public_key = key.public_key_der()?;
    let signing_info_no_sig = SigningInfo {
        apk_digest: apk_digest.clone(),
        certificate: certificate.der.clone(),
        additional_data: Vec::new(),
        public_key: public_key.clone(),
        signature_algorithm_id: -1,
        signature: Vec::new(),
    };
    let signed = v4_signed_data(file_size, &hashing, &signing_info_no_sig);
    let signature = key.sign(signature_algorithm.jca_signature_algorithm(), &signed)?;

    let signing_info = SigningInfo {
        apk_digest,
        certificate: certificate.der.clone(),
        additional_data: Vec::new(),
        public_key,
        signature_algorithm_id: signature_algorithm.id() as i32,
        signature,
    };

    // SigningInfos = single SigningInfo (no v4.1 block here).
    let signing_infos = signing_info.to_bytes();

    // V4Signature.writeTo: version | hashingInfo | signingInfos, then the tree.
    let mut out = Vec::new();
    out.extend_from_slice(&CURRENT_VERSION.to_le_bytes());
    write_len_bytes(&mut out, &hashing.to_bytes());
    write_len_bytes(&mut out, &signing_infos);
    write_len_bytes(&mut out, &tree);
    Ok(out)
}

/// Pulls the strongest supported v2/v3 content digest out of the APK's
/// signing block (`V4SchemeSigner.getApkDigests` + `getBestV3/V2Digest`).
fn best_apk_digest(apk: &[u8]) -> Result<Vec<u8>> {
    let sections = zip::find_zip_sections(apk)?;
    let block_info = zip::find_apk_signing_block(apk, &sections)?
        .context("APK has no signing block, cannot derive v4 digest")?;
    let block = &apk[block_info.start_offset..block_info.start_offset + block_info.size];

    // Prefer v3.1, then v3, then v2; within a block, prefer the strongest
    // supported content digest (CHUNKED_SHA512 > VERITY_SHA256 > SHA256).
    for block_id in [V31_BLOCK_ID, V3_BLOCK_ID, V2_BLOCK_ID] {
        if let Some(scheme_block) = sigblock::find_block(block, block_id)? {
            if let Some(digest) = best_digest_in_block(&scheme_block, block_id != V2_BLOCK_ID)? {
                return Ok(digest);
            }
        }
    }
    bail!("Failed to obtain v2/v3 digest from APK signing block")
}

/// Returns the best supported content digest from a v2/v3 scheme block's
/// first signer.
fn best_digest_in_block(scheme_block: &[u8], is_v3: bool) -> Result<Option<Vec<u8>>> {
    let mut s = sigblock::Slice::new(scheme_block);
    let mut signers = s.get_length_prefixed_slice()?;
    if !signers.has_remaining() {
        return Ok(None);
    }
    let mut signer = signers.get_length_prefixed_slice()?;
    let mut signed_data = signer.get_length_prefixed_slice()?;
    let mut digests = signed_data.get_length_prefixed_slice()?;

    let mut best: Option<(i32, Vec<u8>)> = None;
    while digests.has_remaining() {
        let mut digest = digests.get_length_prefixed_slice()?;
        let sig_alg_id = digest.get_u32()?;
        let value = digest.get_length_prefixed_bytes()?.to_vec();
        let alg = match SignatureAlgorithm::from_id(sig_alg_id) {
            Some(a) => a,
            None => continue,
        };
        let cda = alg.content_digest_algorithm();
        let order = digest_sort_order(cda, is_v3);
        if order < 0 {
            continue;
        }
        if best.as_ref().map(|(o, _)| order > *o).unwrap_or(true) {
            best = Some((order, value));
        }
    }
    Ok(best.map(|(_, v)| v))
}

/// `V4SchemeSigner.digestAlgorithmSortingOrder` gated by `isSupported`.
fn digest_sort_order(cda: ContentDigestAlgorithm, for_v3: bool) -> i32 {
    match cda {
        ContentDigestAlgorithm::ChunkedSha256 => 0,
        ContentDigestAlgorithm::VerityChunkedSha256 if for_v3 => 1,
        ContentDigestAlgorithm::ChunkedSha512 => 2,
        _ => -1,
    }
}

/// Parsed `.idsig` for verification.
pub struct V4Signature {
    pub version: u32,
    pub hashing_info: Vec<u8>,
    pub signing_infos: Vec<u8>,
}

impl V4Signature {
    /// Reads the version/hashingInfo/signingInfos header (ignores the tree).
    pub fn read(data: &[u8]) -> Result<V4Signature> {
        let mut s = sigblock::Slice::new(data);
        let version = s.get_u32()?;
        if version != CURRENT_VERSION {
            bail!("Invalid signature version.");
        }
        let hashing_info = s.get_length_prefixed_bytes()?.to_vec();
        let signing_infos = s.get_length_prefixed_bytes()?.to_vec();
        Ok(V4Signature {
            version,
            hashing_info,
            signing_infos,
        })
    }
}
