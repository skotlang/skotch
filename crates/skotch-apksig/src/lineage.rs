//! Signing certificate lineage / proof-of-rotation
//! (`SigningCertificateLineage.java` + `V3SigningCertificateLineage.java`).
//!
//! A lineage is an ordered chain of signing certificates where each node is
//! signed by its predecessor, enabling APK signing-key rotation (v3). It is
//! serialized both standalone (the `apksigner rotate`/`lineage` file format,
//! MAGIC `0x3eff39d1`) and embedded in the v3 proof-of-rotation attribute.

use crate::crypto::{suggested_signature_algorithms, Certificate, PrivateKey, SignatureAlgorithm};
use crate::sigblock::{length_prefixed, sequence_of_length_prefixed, Slice};
use anyhow::{anyhow, bail, Context, Result};

pub const MAGIC: u32 = 0x3eff_39d1;
pub const CURRENT_VERSION: u32 = 1;

// Capability flag bits (`SigningCertificateLineage`).
pub const PAST_CERT_INSTALLED_DATA: u32 = 1;
pub const PAST_CERT_SHARED_USER_ID: u32 = 2;
pub const PAST_CERT_PERMISSION: u32 = 4;
pub const PAST_CERT_ROLLBACK: u32 = 8;
pub const PAST_CERT_AUTH: u32 = 16;

/// Capabilities a previous signer retains after rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignerCapabilities {
    pub flags: u32,
}

impl SignerCapabilities {
    /// `SignerCapabilities.Builder().build()` default: all bits except
    /// rollback (`PAST_CERT_INSTALLED_DATA | PERMISSION | SHARED_USER_ID | AUTH`).
    pub fn default_flags() -> SignerCapabilities {
        SignerCapabilities {
            flags: PAST_CERT_INSTALLED_DATA
                | PAST_CERT_PERMISSION
                | PAST_CERT_SHARED_USER_ID
                | PAST_CERT_AUTH,
        }
    }

    pub fn from_flags(flags: u32) -> SignerCapabilities {
        SignerCapabilities { flags }
    }

    pub fn has_installed_data(&self) -> bool {
        self.flags & PAST_CERT_INSTALLED_DATA != 0
    }
    pub fn has_shared_uid(&self) -> bool {
        self.flags & PAST_CERT_SHARED_USER_ID != 0
    }
    pub fn has_permission(&self) -> bool {
        self.flags & PAST_CERT_PERMISSION != 0
    }
    pub fn has_rollback(&self) -> bool {
        self.flags & PAST_CERT_ROLLBACK != 0
    }
    pub fn has_auth(&self) -> bool {
        self.flags & PAST_CERT_AUTH != 0
    }

    pub fn set(&mut self, bit: u32, value: bool) {
        if value {
            self.flags |= bit;
        } else {
            self.flags &= !bit;
        }
    }
}

/// One node in the lineage (`SigningCertificateNode`).
#[derive(Debug, Clone)]
pub struct SigningCertificateNode {
    /// DER of this node's signing certificate.
    pub signing_cert: Vec<u8>,
    /// Algorithm the parent used to bless this node (`None` for the root).
    pub parent_sig_algorithm: Option<SignatureAlgorithm>,
    /// Algorithm this node uses to bless its child (`None` for the leaf).
    pub sig_algorithm: Option<SignatureAlgorithm>,
    /// Signature from the parent over this node's signed data (empty for root).
    pub signature: Vec<u8>,
    /// Capability flags for this signer.
    pub flags: u32,
}

/// A full signing certificate lineage.
#[derive(Debug, Clone)]
pub struct SigningCertificateLineage {
    pub min_sdk_version: u32,
    pub nodes: Vec<SigningCertificateNode>,
}

impl SigningCertificateLineage {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Encodes the lineage to the standalone file format (MAGIC + version +
    /// length-prefixed node list) — `SigningCertificateLineage.write`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let encoded = self.encode_signing_certificate_lineage();
        let mut out = Vec::with_capacity(12 + encoded.len());
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&CURRENT_VERSION.to_le_bytes());
        out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        out.extend_from_slice(&encoded);
        out
    }

    /// Parses the standalone lineage file format.
    pub fn from_bytes(data: &[u8]) -> Result<SigningCertificateLineage> {
        let mut s = Slice::new(data);
        if s.remaining() < 8 {
            bail!("Improper SigningCertificateLineage format: insufficient data for header.");
        }
        if s.get_u32()? != MAGIC {
            bail!("Improper SigningCertificateLineage format: MAGIC header mismatch.");
        }
        let version = s.get_u32()?;
        if version != CURRENT_VERSION {
            bail!("Improper SigningCertificateLineage format: unrecognized version.");
        }
        let node_bytes = s.get_length_prefixed_slice()?;
        let nodes = read_nodes(node_bytes)?;
        let min_sdk_version = calculate_min_sdk_version(&nodes);
        Ok(SigningCertificateLineage {
            min_sdk_version,
            nodes,
        })
    }

    /// Encodes just the node list with a leading version code
    /// (`V3SigningCertificateLineage.encodeSigningCertificateLineage`). This
    /// is the form embedded in the v3 proof-of-rotation attribute.
    pub fn encode_signing_certificate_lineage(&self) -> Vec<u8> {
        let nodes: Vec<Vec<u8>> = self.nodes.iter().map(encode_node).collect();
        let encoded_nodes = sequence_of_length_prefixed(&nodes);
        let mut out = Vec::with_capacity(4 + encoded_nodes.len());
        out.extend_from_slice(&CURRENT_VERSION.to_le_bytes());
        out.extend_from_slice(&encoded_nodes);
        out
    }

    /// The DER of the most recent (leaf) signing certificate.
    pub fn current_cert_der(&self) -> Option<&[u8]> {
        self.nodes.last().map(|n| n.signing_cert.as_slice())
    }

    /// Creates a new two-node lineage by signing `child` with `parent`
    /// (`apksigner rotate` with no input lineage).
    pub fn create(
        parent_key: &PrivateKey,
        parent_cert: &Certificate,
        child_cert: &Certificate,
        child_capabilities: SignerCapabilities,
        min_sdk_version: u32,
    ) -> Result<SigningCertificateLineage> {
        let root = SigningCertificateNode {
            signing_cert: parent_cert.der.clone(),
            parent_sig_algorithm: None,
            sig_algorithm: None,
            signature: Vec::new(),
            flags: SignerCapabilities::default_flags().flags,
        };
        let lineage = SigningCertificateLineage {
            min_sdk_version,
            nodes: vec![root],
        };
        lineage.spawn_descendant(parent_key, parent_cert, child_cert, child_capabilities)
    }

    /// Adds a new signing certificate, signed by the current leaf
    /// (`spawnDescendant`).
    pub fn spawn_descendant(
        &self,
        parent_key: &PrivateKey,
        parent_cert: &Certificate,
        child_cert: &Certificate,
        child_capabilities: SignerCapabilities,
    ) -> Result<SigningCertificateLineage> {
        if self.nodes.is_empty() {
            bail!("Cannot spawn descendant signing certificate on an empty SigningCertificateLineage: no parent node");
        }
        let current = self.nodes.last().unwrap();
        if current.signing_cert != parent_cert.der {
            bail!("SignerConfig Certificate containing private key to sign the new SigningCertificateLineage record does not match the existing most recent record");
        }

        // Signed data = child cert + parent's signature algorithm id, encoded
        // by `encodeSignedData` then stripped of its outer length prefix.
        let signature_algorithm = lineage_signature_algorithm(parent_key, self.min_sdk_version)?;
        let prefixed_signed_data =
            encode_signed_data(&child_cert.der, signature_algorithm.id() as i32);
        let signed_data = prefixed_signed_data[4..].to_vec();

        let signature =
            parent_key.sign(signature_algorithm.jca_signature_algorithm(), &signed_data)?;

        let mut nodes = self.nodes.clone();
        nodes.last_mut().unwrap().sig_algorithm = Some(signature_algorithm);
        nodes.push(SigningCertificateNode {
            signing_cert: child_cert.der.clone(),
            parent_sig_algorithm: Some(signature_algorithm),
            sig_algorithm: None,
            signature,
            flags: child_capabilities.flags,
        });
        Ok(SigningCertificateLineage {
            min_sdk_version: self.min_sdk_version,
            nodes,
        })
    }

    /// Updates a signer's capabilities by certificate match (`lineage` cmd /
    /// `updateSignerCapabilities`). Returns whether anything changed.
    pub fn update_capabilities(&mut self, cert_der: &[u8], caps: SignerCapabilities) -> bool {
        for node in &mut self.nodes {
            if node.signing_cert == cert_der {
                if node.flags != caps.flags {
                    node.flags = caps.flags;
                    return true;
                }
                return false;
            }
        }
        false
    }
}

fn lineage_signature_algorithm(key: &PrivateKey, _min_sdk: u32) -> Result<SignatureAlgorithm> {
    let algs = suggested_signature_algorithms(key, false, false)?;
    algs.into_iter()
        .next()
        .ok_or_else(|| anyhow!("no signature algorithm available for lineage key"))
}

/// `V3SigningCertificateLineage.encodeSignedData`.
fn encode_signed_data(cert_der: &[u8], flags: i32) -> Vec<u8> {
    let prefixed_cert = length_prefixed(cert_der);
    let mut payload = Vec::with_capacity(prefixed_cert.len() + 4);
    payload.extend_from_slice(&prefixed_cert);
    payload.extend_from_slice(&(flags as u32).to_le_bytes());
    length_prefixed(&payload)
}

/// `V3SigningCertificateLineage.encodeSigningCertificateNode`.
fn encode_node(node: &SigningCertificateNode) -> Vec<u8> {
    let parent_sig_id = node.parent_sig_algorithm.map(|a| a.id()).unwrap_or(0);
    let sig_id = node.sig_algorithm.map(|a| a.id()).unwrap_or(0);
    let prefixed_signed_data = encode_signed_data(&node.signing_cert, parent_sig_id as i32);
    let prefixed_signature = length_prefixed(&node.signature);
    let mut out = Vec::with_capacity(prefixed_signed_data.len() + 8 + prefixed_signature.len());
    out.extend_from_slice(&prefixed_signed_data);
    out.extend_from_slice(&node.flags.to_le_bytes());
    out.extend_from_slice(&sig_id.to_le_bytes());
    out.extend_from_slice(&prefixed_signature);
    out
}

/// `V3SigningCertificateLineage.readSigningCertificateLineage` (without the
/// signature re-verification, which the caller can request separately).
fn read_nodes(bytes: Slice) -> Result<Vec<SigningCertificateNode>> {
    let mut bytes = bytes;
    let mut nodes = Vec::new();
    let version = bytes.get_u32()?;
    if version != CURRENT_VERSION {
        bail!("Encoded SigningCertificateLineage has an unrecognized version");
    }
    let mut last_sig_alg_id = 0u32;
    while bytes.has_remaining() {
        let mut node = bytes.get_length_prefixed_slice()?;
        let signed_data = node.get_length_prefixed_slice()?;
        let flags = node.get_u32()?;
        let sig_algorithm_id = node.get_u32()?;
        let signature = node.get_length_prefixed_bytes()?.to_vec();

        let mut sd = signed_data;
        let encoded_cert = sd.get_length_prefixed_bytes()?.to_vec();
        let signed_sig_algorithm = sd.get_u32()?;

        let parent_sig = SignatureAlgorithm::from_id(last_sig_alg_id);
        let _ = signed_sig_algorithm;
        nodes.push(SigningCertificateNode {
            signing_cert: encoded_cert,
            parent_sig_algorithm: if nodes.is_empty() { None } else { parent_sig },
            sig_algorithm: SignatureAlgorithm::from_id(sig_algorithm_id),
            signature,
            flags,
        });
        last_sig_alg_id = sig_algorithm_id;
    }
    Ok(nodes)
}

/// `SigningCertificateLineage.calculateMinSdkVersion`.
fn calculate_min_sdk_version(nodes: &[SigningCertificateNode]) -> u32 {
    let mut min = crate::sdk::P;
    for node in nodes {
        if let Some(alg) = node.sig_algorithm {
            min = min.max(alg.min_sdk_version());
        }
    }
    min
}

/// Parses a lineage embedded in a v3 signer's signed-data additional
/// attributes (`readFromV3AttributeValue`/`readFromSignedData`). `value` is
/// the proof-of-rotation attribute value (the encoded node list).
pub fn read_from_v3_attribute_value(value: &[u8]) -> Result<SigningCertificateLineage> {
    let nodes = read_nodes(Slice::new(value)).context("parsing lineage attribute")?;
    let min_sdk_version = calculate_min_sdk_version(&nodes);
    Ok(SigningCertificateLineage {
        min_sdk_version,
        nodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_defaults() {
        let caps = SignerCapabilities::default_flags();
        assert!(caps.has_installed_data());
        assert!(caps.has_permission());
        assert!(caps.has_shared_uid());
        assert!(caps.has_auth());
        assert!(!caps.has_rollback());
    }
}
