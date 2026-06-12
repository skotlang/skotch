//! PKCS#7 / CMS SignedData for the v1 JAR signature block (the `.RSA`/`.DSA`/
//! `.EC` file). Mirrors `ApkSigningBlockUtils.generatePkcs7DerEncodedMessage`
//! and apksig's `Asn1DerEncoder` output exactly.
//!
//! The structure is an attached-but-content-less SignedData: there are no
//! signed attributes, so the signature is computed directly over the `.SF`
//! file bytes (done by the caller) and embedded here.

use crate::crypto::{Certificate, DigestAlgorithm, KeyAlgorithm};
use crate::derhelp::*;
use anyhow::Result;

// Digest OIDs (`OidConstants.java`).
const OID_DIGEST_SHA1: &str = "1.3.14.3.2.26";
const OID_DIGEST_SHA256: &str = "2.16.840.1.101.3.4.2.1";

// Signature OIDs.
const OID_SIG_RSA: &str = "1.2.840.113549.1.1.1";
const OID_SIG_DSA: &str = "1.2.840.10040.4.1";
const OID_SIG_SHA256_WITH_DSA: &str = "2.16.840.1.101.3.4.3.2";
const OID_SIG_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";

// PKCS#7 content type OIDs (`Pkcs7Constants.java`).
const OID_DATA: &str = "1.2.840.113549.1.7.1";
const OID_SIGNED_DATA: &str = "1.2.840.113549.1.7.2";

/// Builds the DER-encoded PKCS#7 ContentInfo wrapping a SignedData.
pub fn generate_pkcs7(
    signature: &[u8],
    certificates: &[Certificate],
    digest_algorithm: DigestAlgorithm,
    key_algorithm: KeyAlgorithm,
) -> Result<Vec<u8>> {
    let signing_cert = &certificates[0];

    let digest_alg_id = digest_algorithm_identifier(digest_algorithm);
    let signature_alg_id = signature_algorithm_identifier(key_algorithm, digest_algorithm);

    // SignerInfo:
    //   version INTEGER 1
    //   sid: IssuerAndSerialNumber { issuer Name, serial INTEGER }
    //   digestAlgorithm AlgorithmIdentifier
    //   (no signedAttrs)
    //   signatureAlgorithm AlgorithmIdentifier
    //   signature OCTET STRING
    let issuer_and_serial = sequence(&[
        signing_cert.issuer_der.clone(),
        integer_from_be_twos_complement(&signing_cert.serial_be),
    ]);
    let signer_info = sequence(&[
        integer_u32(1),
        issuer_and_serial,
        digest_alg_id.clone(),
        signature_alg_id,
        octet_string(signature),
    ]);

    // SignedData:
    //   version INTEGER 1
    //   digestAlgorithms SET OF AlgorithmIdentifier
    //   encapContentInfo SEQUENCE { contentType OID data }   (no content)
    //   certificates [0] IMPLICIT SET OF Certificate
    //   signerInfos SET OF SignerInfo
    let digest_algorithms = set_unordered(&[digest_alg_id]);
    let encap_content_info = sequence(&[oid(OID_DATA)]);
    let certs: Vec<Vec<u8>> = certificates.iter().map(|c| c.der.clone()).collect();
    let certificates_field = implicit_constructed(0, &concat(&certs));
    let signer_infos = set_unordered(&[signer_info]);

    let signed_data = sequence(&[
        integer_u32(1),
        digest_algorithms,
        encap_content_info,
        certificates_field,
        signer_infos,
    ]);

    // ContentInfo:
    //   contentType OID signedData
    //   content [0] EXPLICIT SignedData
    let content_info = sequence(&[oid(OID_SIGNED_DATA), explicit_context(0, &signed_data)]);
    Ok(content_info)
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

/// `AlgorithmIdentifier.getSignerInfoDigestAlgorithmOid`: OID + DER NULL.
fn digest_algorithm_identifier(d: DigestAlgorithm) -> Vec<u8> {
    let oid_str = match d {
        DigestAlgorithm::Sha1 => OID_DIGEST_SHA1,
        DigestAlgorithm::Sha256 => OID_DIGEST_SHA256,
    };
    sequence(&[oid(oid_str), null()])
}

/// `AlgorithmIdentifier.getSignerInfoSignatureAlgorithm`: OID + DER NULL.
fn signature_algorithm_identifier(key: KeyAlgorithm, digest: DigestAlgorithm) -> Vec<u8> {
    let oid_str = match key {
        KeyAlgorithm::Rsa => OID_SIG_RSA,
        KeyAlgorithm::Dsa => match digest {
            DigestAlgorithm::Sha1 => OID_SIG_DSA,
            DigestAlgorithm::Sha256 => OID_SIG_SHA256_WITH_DSA,
        },
        KeyAlgorithm::Ec => OID_SIG_EC_PUBLIC_KEY,
    };
    sequence(&[oid(oid_str), null()])
}
