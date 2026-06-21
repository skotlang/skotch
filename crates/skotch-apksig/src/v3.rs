//! APK Signature Scheme v3 / v3.1 signer (`V3SchemeSigner.java`).

use crate::digest::ContentDigestAlgorithm;
use crate::lineage::SigningCertificateLineage;
use crate::sigblock::{
    length_prefixed, sequence_of_id_value_pairs, sequence_of_length_prefixed,
    PROOF_OF_ROTATION_ATTR_ID, ROTATION_MIN_SDK_VERSION_ATTR_ID, ROTATION_ON_DEV_RELEASE_ATTR_ID,
    V31_BLOCK_ID, V3_BLOCK_ID,
};
use crate::v2::{generate_signatures_over_data, SignerConfig};
use anyhow::Result;
use std::collections::BTreeMap;

/// Per-signer v3 parameters: an apk-signing-block signer config plus the v3
/// SDK range, optional lineage, and dev-release targeting.
pub struct V3SignerConfig<'a> {
    pub signer: SignerConfig<'a>,
    pub min_sdk_version: u32,
    pub max_sdk_version: u32,
    pub lineage: Option<&'a SigningCertificateLineage>,
    pub signer_targets_dev_release: bool,
}

/// Options governing which block id and stripping attributes are written.
pub struct V3BlockParams {
    /// `V3_BLOCK_ID` for v3.0, `V31_BLOCK_ID` for v3.1.
    pub block_id: u32,
    /// When set on the v3.0 block, writes the rotation-min-sdk stripping attr.
    pub rotation_min_sdk_version: Option<u32>,
    /// When true and writing the v3.1 block, writes the dev-release attr.
    pub rotation_targets_dev_release: bool,
}

impl V3BlockParams {
    pub fn v3() -> V3BlockParams {
        V3BlockParams {
            block_id: V3_BLOCK_ID,
            rotation_min_sdk_version: None,
            rotation_targets_dev_release: false,
        }
    }

    pub fn v31() -> V3BlockParams {
        V3BlockParams {
            block_id: V31_BLOCK_ID,
            rotation_min_sdk_version: None,
            rotation_targets_dev_release: false,
        }
    }
}

/// Generates the v3 (or v3.1) block bytes plus its block id.
pub fn generate_v3_block(
    signers: &[V3SignerConfig],
    content_digests: &BTreeMap<ContentDigestAlgorithm, Vec<u8>>,
    params: &V3BlockParams,
) -> Result<(Vec<u8>, u32)> {
    let mut signer_blocks: Vec<Vec<u8>> = Vec::with_capacity(signers.len());
    for signer in signers {
        signer_blocks.push(generate_signer_block(signer, content_digests, params)?);
    }
    let block = sequence_of_length_prefixed(&[sequence_of_length_prefixed(&signer_blocks)]);
    Ok((block, params.block_id))
}

fn generate_signer_block(
    cfg: &V3SignerConfig,
    content_digests: &BTreeMap<ContentDigestAlgorithm, Vec<u8>>,
    params: &V3BlockParams,
) -> Result<Vec<u8>> {
    let public_key = cfg.signer.certificates[0].spki_der.clone();

    let digests: Vec<(u32, Vec<u8>)> = cfg
        .signer
        .signature_algorithms
        .iter()
        .map(|alg| {
            (
                alg.id(),
                content_digests[&alg.content_digest_algorithm()].clone(),
            )
        })
        .collect();
    let certs: Vec<Vec<u8>> = cfg
        .signer
        .certificates
        .iter()
        .map(|c| c.der.clone())
        .collect();
    let additional_attributes = generate_additional_attributes(cfg, params);

    // signed data:
    //   length-prefixed digests
    //   length-prefixed certs
    //   uint32 minSdk
    //   uint32 maxSdk
    //   length-prefixed additional attributes
    let mut signed_data = Vec::new();
    signed_data.extend_from_slice(&length_prefixed(&sequence_of_id_value_pairs(&digests)));
    signed_data.extend_from_slice(&length_prefixed(&sequence_of_length_prefixed(&certs)));
    signed_data.extend_from_slice(&cfg.min_sdk_version.to_le_bytes());
    signed_data.extend_from_slice(&cfg.max_sdk_version.to_le_bytes());
    signed_data.extend_from_slice(&length_prefixed(&additional_attributes));

    let signatures = generate_signatures_over_data(&cfg.signer, &signed_data)?;

    // signer:
    //   length-prefixed signed data
    //   uint32 minSdk
    //   uint32 maxSdk
    //   length-prefixed signatures
    //   length-prefixed public key
    let mut signer = Vec::new();
    signer.extend_from_slice(&length_prefixed(&signed_data));
    signer.extend_from_slice(&cfg.min_sdk_version.to_le_bytes());
    signer.extend_from_slice(&cfg.max_sdk_version.to_le_bytes());
    signer.extend_from_slice(&length_prefixed(&sequence_of_id_value_pairs(&signatures)));
    signer.extend_from_slice(&length_prefixed(&public_key));
    Ok(signer)
}

fn generate_additional_attributes(cfg: &V3SignerConfig, params: &V3BlockParams) -> Vec<u8> {
    let mut attrs = Vec::new();
    if let Some(lineage) = cfg.lineage {
        attrs.extend_from_slice(&v3_lineage_attribute(lineage));
    }
    if (params.rotation_targets_dev_release || cfg.signer_targets_dev_release)
        && params.block_id == V31_BLOCK_ID
    {
        attrs.extend_from_slice(&dev_release_attribute());
    }
    if params.block_id == V3_BLOCK_ID {
        if let Some(min_sdk) = params.rotation_min_sdk_version {
            attrs.extend_from_slice(&rotation_min_sdk_attribute(min_sdk));
        }
    }
    attrs
}

/// `generateV3SignerAttribute`: PROOF_OF_ROTATION_ATTR_ID + encoded lineage.
fn v3_lineage_attribute(lineage: &SigningCertificateLineage) -> Vec<u8> {
    let encoded = lineage.encode_signing_certificate_lineage();
    let mut out = Vec::with_capacity(8 + encoded.len());
    out.extend_from_slice(&((4 + encoded.len()) as u32).to_le_bytes());
    out.extend_from_slice(&PROOF_OF_ROTATION_ATTR_ID.to_le_bytes());
    out.extend_from_slice(&encoded);
    out
}

/// `generateV3RotationMinSdkVersionStrippingProtectionAttribute`.
fn rotation_min_sdk_attribute(min_sdk: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&ROTATION_MIN_SDK_VERSION_ATTR_ID.to_le_bytes());
    out.extend_from_slice(&min_sdk.to_le_bytes());
    out
}

/// `generateV31RotationTargetsDevReleaseAttribute` (no value).
fn dev_release_attribute() -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&ROTATION_ON_DEV_RELEASE_ATTR_ID.to_le_bytes());
    out
}
