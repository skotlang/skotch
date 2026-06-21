//! APK Signature Scheme v2 signer (`V2SchemeSigner.java`).

use crate::crypto::{Certificate, PrivateKey, SignatureAlgorithm};
use crate::digest::ContentDigestAlgorithm;
use crate::sigblock::{
    sequence_of_id_value_pairs, sequence_of_length_prefixed, STRIPPING_PROTECTION_ATTR_ID,
    V2_BLOCK_ID,
};
use anyhow::Result;
use std::collections::BTreeMap;

/// One signer's configuration for the block schemes.
pub struct SignerConfig<'a> {
    pub key: &'a PrivateKey,
    pub certificates: &'a [Certificate],
    pub signature_algorithms: Vec<SignatureAlgorithm>,
}

/// Generates the v2 block bytes plus its block id, given precomputed digests.
pub fn generate_v2_block(
    signers: &[SignerConfig],
    content_digests: &BTreeMap<ContentDigestAlgorithm, Vec<u8>>,
    v3_signing_enabled: bool,
) -> Result<(Vec<u8>, u32)> {
    let mut signer_blocks: Vec<Vec<u8>> = Vec::with_capacity(signers.len());
    for signer in signers {
        signer_blocks.push(generate_signer_block(
            signer,
            content_digests,
            v3_signing_enabled,
        )?);
    }
    let block = sequence_of_length_prefixed(&[sequence_of_length_prefixed(&signer_blocks)]);
    Ok((block, V2_BLOCK_ID))
}

fn generate_signer_block(
    signer: &SignerConfig,
    content_digests: &BTreeMap<ContentDigestAlgorithm, Vec<u8>>,
    v3_signing_enabled: bool,
) -> Result<Vec<u8>> {
    let public_key = signer.certificates[0].spki_der.clone();

    let digests: Vec<(u32, Vec<u8>)> = signer
        .signature_algorithms
        .iter()
        .map(|alg| {
            (
                alg.id(),
                content_digests[&alg.content_digest_algorithm()].clone(),
            )
        })
        .collect();
    let certs: Vec<Vec<u8>> = signer.certificates.iter().map(|c| c.der.clone()).collect();
    let additional_attributes = v2_additional_attributes(v3_signing_enabled);

    // signed data: digests | certificates | additional-attributes | [empty]
    let signed_data = sequence_of_length_prefixed(&[
        sequence_of_id_value_pairs(&digests),
        sequence_of_length_prefixed(&certs),
        additional_attributes,
        Vec::new(),
    ]);

    let signatures = generate_signatures_over_data(signer, &signed_data)?;

    // signer: signed-data | signatures | public-key
    Ok(sequence_of_length_prefixed(&[
        signed_data,
        sequence_of_id_value_pairs(&signatures),
        public_key,
    ]))
}

fn v2_additional_attributes(v3_signing_enabled: bool) -> Vec<u8> {
    if !v3_signing_enabled {
        return Vec::new();
    }
    // length-prefixed attribute: STRIPPING_PROTECTION_ATTR_ID -> v3 id (3).
    let mut attr = Vec::with_capacity(12);
    attr.extend_from_slice(&8u32.to_le_bytes());
    attr.extend_from_slice(&STRIPPING_PROTECTION_ATTR_ID.to_le_bytes());
    attr.extend_from_slice(&3u32.to_le_bytes());
    attr
}

/// `ApkSigningBlockUtils.generateSignaturesOverData`: sign `data` with each
/// configured algorithm. apksig also re-verifies each signature against the
/// certificate's public key; the verification path lives in `verify.rs` so we
/// don't duplicate it here.
pub fn generate_signatures_over_data(
    signer: &SignerConfig,
    data: &[u8],
) -> Result<Vec<(u32, Vec<u8>)>> {
    let mut out = Vec::with_capacity(signer.signature_algorithms.len());
    for alg in &signer.signature_algorithms {
        let sig = signer.key.sign(alg.jca_signature_algorithm(), data)?;
        out.push((alg.id(), sig));
    }
    Ok(out)
}
