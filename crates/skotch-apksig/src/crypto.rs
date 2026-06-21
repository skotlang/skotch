//! Key material, certificates, and signature algorithms.
//!
//! Wraps the RustCrypto primitives behind apksig-shaped types so the scheme
//! signers can stay close to the Java structure: a [`SignatureAlgorithm`]
//! enum mirroring `SignatureAlgorithm.java`, plus signing/verification that
//! select JCA-equivalent primitives by key type.

use crate::digest::ContentDigestAlgorithm;
use anyhow::{anyhow, bail, Context, Result};
use std::sync::Arc;

/// Asymmetric key algorithm family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlgorithm {
    Rsa,
    Dsa,
    Ec,
}

impl KeyAlgorithm {
    /// JCA-style uppercase name used in the v1 signature block filename
    /// (`META-INF/<signer>.RSA` / `.DSA` / `.EC`).
    pub fn jca_name(self) -> &'static str {
        match self {
            KeyAlgorithm::Rsa => "RSA",
            KeyAlgorithm::Dsa => "DSA",
            KeyAlgorithm::Ec => "EC",
        }
    }
}

/// A parsed private key plus its derived public-key encoding.
#[derive(Clone)]
pub struct PrivateKey {
    inner: Arc<PrivateKeyInner>,
}

enum PrivateKeyInner {
    Rsa(rsa::RsaPrivateKey),
    Dsa(dsa::SigningKey),
    EcP256(p256::ecdsa::SigningKey),
    EcP384(p384::ecdsa::SigningKey),
}

impl PrivateKey {
    pub fn algorithm(&self) -> KeyAlgorithm {
        match &*self.inner {
            PrivateKeyInner::Rsa(_) => KeyAlgorithm::Rsa,
            PrivateKeyInner::Dsa(_) => KeyAlgorithm::Dsa,
            PrivateKeyInner::EcP256(_) | PrivateKeyInner::EcP384(_) => KeyAlgorithm::Ec,
        }
    }

    /// Effective key strength in bits, as used by the v2/v3 digest selection
    /// (`RSAKey.getModulus().bitLength()`, EC order bit length).
    pub fn key_size_bits(&self) -> usize {
        match &*self.inner {
            PrivateKeyInner::Rsa(k) => {
                use rsa::traits::PublicKeyParts;
                k.n().bits()
            }
            PrivateKeyInner::Dsa(k) => dsa_p_bits(k),
            PrivateKeyInner::EcP256(_) => 256,
            PrivateKeyInner::EcP384(_) => 384,
        }
    }

    /// Parses an RSA/DSA/EC key from unencrypted PKCS#8 DER.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<PrivateKey> {
        use pkcs8::PrivateKeyInfo;
        let pki = PrivateKeyInfo::try_from(der)
            .map_err(|e| anyhow!("parsing PKCS#8 private key: {e}"))?;
        let oid = pki.algorithm.oid.to_string();
        let inner = match oid.as_str() {
            // rsaEncryption
            "1.2.840.113549.1.1.1" => {
                use rsa::pkcs8::DecodePrivateKey;
                PrivateKeyInner::Rsa(
                    rsa::RsaPrivateKey::from_pkcs8_der(der)
                        .map_err(|e| anyhow!("parsing RSA key: {e}"))?,
                )
            }
            // id-dsa
            "1.2.840.10040.4.1" => {
                use dsa::pkcs8::DecodePrivateKey;
                PrivateKeyInner::Dsa(
                    dsa::SigningKey::from_pkcs8_der(der)
                        .map_err(|e| anyhow!("parsing DSA key: {e}"))?,
                )
            }
            // id-ecPublicKey — discriminate by named curve.
            "1.2.840.10045.2.1" => parse_ec_pkcs8(der, pki)?,
            other => bail!("Unsupported private key algorithm OID: {other}"),
        };
        Ok(PrivateKey {
            inner: Arc::new(inner),
        })
    }

    /// Parses a private key from PKCS#8 PEM or DER, autodetecting the form.
    pub fn from_pkcs8_pem_or_der(data: &[u8]) -> Result<PrivateKey> {
        if data.starts_with(b"-----BEGIN") {
            let text = std::str::from_utf8(data).context("private key PEM is not UTF-8")?;
            let der = pem_to_der(text, "PRIVATE KEY")?;
            PrivateKey::from_pkcs8_der(&der)
        } else {
            PrivateKey::from_pkcs8_der(data)
        }
    }

    /// DER-encoded SubjectPublicKeyInfo for this key's public half.
    pub fn public_key_der(&self) -> Result<Vec<u8>> {
        let der = match &*self.inner {
            PrivateKeyInner::Rsa(k) => {
                use rsa::pkcs8::EncodePublicKey;
                k.to_public_key()
                    .to_public_key_der()
                    .map_err(|e| anyhow!("encoding RSA public key: {e}"))?
                    .as_bytes()
                    .to_vec()
            }
            PrivateKeyInner::Dsa(k) => {
                use dsa::pkcs8::EncodePublicKey;
                k.verifying_key()
                    .to_public_key_der()
                    .map_err(|e| anyhow!("encoding DSA public key: {e}"))?
                    .as_bytes()
                    .to_vec()
            }
            PrivateKeyInner::EcP256(k) => {
                use p256::pkcs8::EncodePublicKey;
                k.verifying_key()
                    .to_public_key_der()
                    .map_err(|e| anyhow!("encoding EC public key: {e}"))?
                    .as_bytes()
                    .to_vec()
            }
            PrivateKeyInner::EcP384(k) => {
                use p384::pkcs8::EncodePublicKey;
                k.verifying_key()
                    .to_public_key_der()
                    .map_err(|e| anyhow!("encoding EC public key: {e}"))?
                    .as_bytes()
                    .to_vec()
            }
        };
        Ok(der)
    }

    /// Signs `data` with the given JCA-style signature algorithm name.
    pub fn sign(&self, jca_alg: &str, data: &[u8]) -> Result<Vec<u8>> {
        match &*self.inner {
            PrivateKeyInner::Rsa(k) => rsa_sign(k, jca_alg, data),
            PrivateKeyInner::Dsa(k) => dsa_sign(k, jca_alg, data),
            PrivateKeyInner::EcP256(k) => {
                let sig = ecdsa_sign_p256(k, jca_alg, data)?;
                Ok(sig.as_bytes().to_vec())
            }
            PrivateKeyInner::EcP384(k) => ecdsa_sign_p384(k, jca_alg, data),
        }
    }
}

fn dsa_p_bits(k: &dsa::SigningKey) -> usize {
    use dsa::Components;
    let _ = <Components>::p; // keep the trait/method path explicit
    k.verifying_key().components().p().bits() as usize
}

fn parse_ec_pkcs8(der: &[u8], pki: pkcs8::PrivateKeyInfo) -> Result<PrivateKeyInner> {
    // The named curve lives in the AlgorithmIdentifier parameters as an OID.
    let params = pki
        .algorithm
        .parameters
        .ok_or_else(|| anyhow!("EC key missing curve parameters"))?;
    let curve_oid = params
        .decode_as::<der::asn1::ObjectIdentifier>()
        .map_err(|e| anyhow!("reading EC curve OID: {e}"))?
        .to_string();
    use pkcs8::DecodePrivateKey;
    match curve_oid.as_str() {
        // prime256v1 / secp256r1
        "1.2.840.10045.3.1.7" => Ok(PrivateKeyInner::EcP256(
            p256::ecdsa::SigningKey::from_pkcs8_der(der)
                .map_err(|e| anyhow!("parsing P-256 key: {e}"))?,
        )),
        // secp384r1
        "1.3.132.0.34" => Ok(PrivateKeyInner::EcP384(
            p384::ecdsa::SigningKey::from_pkcs8_der(der)
                .map_err(|e| anyhow!("parsing P-384 key: {e}"))?,
        )),
        // secp521r1 — not supported (P-521 ECDSA is unavailable in this build).
        "1.3.132.0.35" => bail!("P-521 (secp521r1) signing keys are not supported"),
        other => bail!("Unsupported EC curve OID: {other}"),
    }
}

fn rsa_sign(k: &rsa::RsaPrivateKey, jca_alg: &str, data: &[u8]) -> Result<Vec<u8>> {
    use rsa::signature::{SignatureEncoding, Signer};
    match jca_alg {
        "SHA256withRSA" => {
            let sk = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(k.clone());
            Ok(sk.sign(data).to_vec())
        }
        "SHA512withRSA" => {
            let sk = rsa::pkcs1v15::SigningKey::<sha2::Sha512>::new(k.clone());
            Ok(sk.sign(data).to_vec())
        }
        "SHA1withRSA" => {
            let sk = rsa::pkcs1v15::SigningKey::<sha1::Sha1>::new(k.clone());
            Ok(sk.sign(data).to_vec())
        }
        "SHA256withRSA/PSS" => {
            // Deterministic salt is not required by callers (we default to
            // PKCS1 v1.5 everywhere for OTA determinism), but support PSS for
            // completeness with a 32-byte salt.
            use rsa::pss::SigningKey;
            let sk = SigningKey::<sha2::Sha256>::new(k.clone());
            let mut rng = rand_compat::DummyRng::default();
            Ok(rsa::signature::RandomizedSigner::sign_with_rng(&sk, &mut rng, data).to_vec())
        }
        other => bail!("Unsupported RSA signature algorithm: {other}"),
    }
}

fn dsa_sign(k: &dsa::SigningKey, jca_alg: &str, data: &[u8]) -> Result<Vec<u8>> {
    use dsa::signature::hazmat::PrehashSigner;
    use sha2::Digest;
    let prehash = match jca_alg {
        "SHA256withDSA" | "SHA256withDetDSA" => sha2::Sha256::digest(data).to_vec(),
        "SHA1withDSA" => sha1::Sha1::digest(data).to_vec(),
        other => bail!("Unsupported DSA signature algorithm: {other}"),
    };
    // apksig's non-deterministic path uses the JCA RNG; deterministic DSA
    // (RFC 6979) is what we emit so signatures are reproducible in tests.
    let sig: dsa::Signature = k
        .sign_prehash(&prehash)
        .map_err(|e| anyhow!("DSA signing failed: {e}"))?;
    use der::Encode;
    sig.to_der()
        .map_err(|e| anyhow!("encoding DSA signature: {e}"))
}

fn ecdsa_sign_p256(
    k: &p256::ecdsa::SigningKey,
    jca_alg: &str,
    data: &[u8],
) -> Result<p256::ecdsa::DerSignature> {
    use p256::ecdsa::signature::Signer;
    match jca_alg {
        "SHA256withECDSA" => Ok(k.sign(data)),
        "SHA512withECDSA" => {
            // P-256 with SHA-512 prehash.
            use p256::ecdsa::signature::hazmat::PrehashSigner;
            use sha2::Digest;
            let h = sha2::Sha512::digest(data);
            let sig: p256::ecdsa::Signature = k
                .sign_prehash(&h)
                .map_err(|e| anyhow!("ECDSA signing failed: {e}"))?;
            Ok(sig.to_der())
        }
        other => bail!("Unsupported ECDSA signature algorithm: {other}"),
    }
}

fn ecdsa_sign_p384(k: &p384::ecdsa::SigningKey, jca_alg: &str, data: &[u8]) -> Result<Vec<u8>> {
    use p384::ecdsa::signature::Signer;
    let sig: p384::ecdsa::DerSignature = match jca_alg {
        "SHA256withECDSA" => {
            use p384::ecdsa::signature::hazmat::PrehashSigner;
            use sha2::Digest;
            let h = sha2::Sha256::digest(data);
            let s: p384::ecdsa::Signature = k
                .sign_prehash(&h)
                .map_err(|e| anyhow!("ECDSA signing failed: {e}"))?;
            s.to_der()
        }
        "SHA512withECDSA" => k.sign(data),
        other => bail!("Unsupported ECDSA signature algorithm: {other}"),
    };
    Ok(sig.as_bytes().to_vec())
}

/// A minimal "RNG" shim so PSS has something to draw from. PSS is not on any
/// golden path; this keeps the dependency surface small.
mod rand_compat {
    #[derive(Default)]
    pub struct DummyRng {
        state: u64,
    }
    impl rsa::rand_core::RngCore for DummyRng {
        fn next_u32(&mut self) -> u32 {
            self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (self.state >> 32) as u32
        }
        fn next_u64(&mut self) -> u64 {
            ((self.next_u32() as u64) << 32) | self.next_u32() as u64
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(4) {
                let v = self.next_u32().to_le_bytes();
                chunk.copy_from_slice(&v[..chunk.len()]);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }
    impl rsa::rand_core::CryptoRng for DummyRng {}
}

/// Verifies `signature` over `data` using the public key in `spki_der`, for
/// the JCA-style signature algorithm name. Returns `Ok(true)` on a valid
/// signature, `Ok(false)` on a well-formed but incorrect one.
pub fn verify_signature(
    spki_der: &[u8],
    jca_alg: &str,
    data: &[u8],
    signature: &[u8],
) -> Result<bool> {
    use spki::DecodePublicKey;
    if jca_alg.contains("RSA") {
        use rsa::RsaPublicKey;
        let key = RsaPublicKey::from_public_key_der(spki_der)
            .map_err(|e| anyhow!("parsing RSA public key: {e}"))?;
        return Ok(rsa_verify(&key, jca_alg, data, signature));
    }
    if jca_alg.contains("ECDSA") {
        return ec_verify(spki_der, jca_alg, data, signature);
    }
    if jca_alg.contains("DSA") {
        use dsa::VerifyingKey;
        let key = VerifyingKey::from_public_key_der(spki_der)
            .map_err(|e| anyhow!("parsing DSA public key: {e}"))?;
        return Ok(dsa_verify(&key, jca_alg, data, signature));
    }
    bail!("Unsupported signature algorithm for verification: {jca_alg}")
}

fn rsa_verify(key: &rsa::RsaPublicKey, jca_alg: &str, data: &[u8], sig: &[u8]) -> bool {
    use rsa::signature::Verifier;
    match jca_alg {
        "SHA256withRSA" => rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(key.clone())
            .verify(data, &rsa_sig(sig))
            .is_ok(),
        "SHA512withRSA" => rsa::pkcs1v15::VerifyingKey::<sha2::Sha512>::new(key.clone())
            .verify(data, &rsa_sig(sig))
            .is_ok(),
        "SHA1withRSA" => rsa::pkcs1v15::VerifyingKey::<sha1::Sha1>::new(key.clone())
            .verify(data, &rsa_sig(sig))
            .is_ok(),
        // RSASSA-PSS: MGF1 with the same hash, salt length = digest length.
        "SHA256withRSA/PSS" => match rsa::pss::Signature::try_from(sig) {
            Ok(s) => rsa::pss::VerifyingKey::<sha2::Sha256>::new(key.clone())
                .verify(data, &s)
                .is_ok(),
            Err(_) => false,
        },
        "SHA512withRSA/PSS" => match rsa::pss::Signature::try_from(sig) {
            Ok(s) => rsa::pss::VerifyingKey::<sha2::Sha512>::new(key.clone())
                .verify(data, &s)
                .is_ok(),
            Err(_) => false,
        },
        _ => false,
    }
}

fn rsa_sig(sig: &[u8]) -> rsa::pkcs1v15::Signature {
    rsa::pkcs1v15::Signature::try_from(sig).unwrap_or_else(|_| {
        // An unparseable signature can never verify; produce a dummy that fails.
        rsa::pkcs1v15::Signature::try_from([0u8; 1].as_ref()).unwrap()
    })
}

fn dsa_verify(key: &dsa::VerifyingKey, jca_alg: &str, data: &[u8], sig: &[u8]) -> bool {
    use der::Decode;
    use dsa::signature::hazmat::PrehashVerifier;
    use sha2::Digest as _;
    let signature = match dsa::Signature::from_der(sig) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let prehash = match jca_alg {
        "SHA256withDSA" | "SHA256withDetDSA" => sha2::Sha256::digest(data).to_vec(),
        "SHA1withDSA" => sha1::Sha1::digest(data).to_vec(),
        _ => return false,
    };
    key.verify_prehash(&prehash, &signature).is_ok()
}

fn ec_verify(spki_der: &[u8], jca_alg: &str, data: &[u8], sig: &[u8]) -> Result<bool> {
    use der::Decode;
    use spki::SubjectPublicKeyInfoRef;
    let spki =
        SubjectPublicKeyInfoRef::from_der(spki_der).map_err(|e| anyhow!("parsing EC SPKI: {e}"))?;
    let curve = spki
        .algorithm
        .parameters_oid()
        .map_err(|e| anyhow!("EC curve OID: {e}"))?
        .to_string();
    use sha2::Digest as _;
    let prehash = match jca_alg {
        "SHA256withECDSA" => sha2::Sha256::digest(data).to_vec(),
        "SHA512withECDSA" => sha2::Sha512::digest(data).to_vec(),
        _ => return Ok(false),
    };
    Ok(match curve.as_str() {
        "1.2.840.10045.3.1.7" => {
            use p256::ecdsa::signature::hazmat::PrehashVerifier;
            use p256::pkcs8::DecodePublicKey;
            match (
                p256::ecdsa::VerifyingKey::from_public_key_der(spki_der),
                p256::ecdsa::Signature::from_der(sig),
            ) {
                (Ok(k), Ok(s)) => k.verify_prehash(&prehash, &s).is_ok(),
                _ => false,
            }
        }
        "1.3.132.0.34" => {
            use p384::ecdsa::signature::hazmat::PrehashVerifier;
            use p384::pkcs8::DecodePublicKey;
            match (
                p384::ecdsa::VerifyingKey::from_public_key_der(spki_der),
                p384::ecdsa::Signature::from_der(sig),
            ) {
                (Ok(k), Ok(s)) => k.verify_prehash(&prehash, &s).is_ok(),
                _ => false,
            }
        }
        "1.3.132.0.35" => bail!("P-521 (secp521r1) verification is not supported"),
        other => bail!("Unsupported EC curve for verification: {other}"),
    })
}

/// An X.509 certificate, kept as raw DER plus the fields signing needs.
#[derive(Clone)]
pub struct Certificate {
    /// DER encoding (exactly as stored on disk / in the APK).
    pub der: Vec<u8>,
    /// DER encoding of the issuer Name (the `Name` TLV).
    pub issuer_der: Vec<u8>,
    /// DER encoding of the subject Name.
    pub subject_der: Vec<u8>,
    /// Serial number as a big-endian two's-complement integer body.
    pub serial_be: Vec<u8>,
    /// SubjectPublicKeyInfo DER.
    pub spki_der: Vec<u8>,
    /// Public key algorithm.
    pub key_algorithm: KeyAlgorithm,
}

impl Certificate {
    pub fn from_der(der: &[u8]) -> Result<Certificate> {
        use der::{Decode, Encode};
        use x509_cert::Certificate as X509;
        let cert = X509::from_der(der).context("parsing X.509 certificate")?;
        let tbs = &cert.tbs_certificate;
        let issuer_der = tbs.issuer.to_der().context("encoding issuer")?;
        let subject_der = tbs.subject.to_der().context("encoding subject")?;
        let serial_be = tbs.serial_number.as_bytes().to_vec();
        let spki_der = tbs
            .subject_public_key_info
            .to_der()
            .context("encoding SPKI")?;
        let key_oid = tbs.subject_public_key_info.algorithm.oid.to_string();
        let key_algorithm = match key_oid.as_str() {
            "1.2.840.113549.1.1.1" => KeyAlgorithm::Rsa,
            "1.2.840.10040.4.1" => KeyAlgorithm::Dsa,
            "1.2.840.10045.2.1" => KeyAlgorithm::Ec,
            other => bail!("Unsupported certificate public key algorithm: {other}"),
        };
        Ok(Certificate {
            der: der.to_vec(),
            issuer_der,
            subject_der,
            serial_be,
            spki_der,
            key_algorithm,
        })
    }

    /// Parses a certificate from PEM or DER, autodetecting the encoding
    /// (`--cert` accepts either, like apksigner).
    pub fn from_pem_or_der(data: &[u8]) -> Result<Certificate> {
        if data.starts_with(b"-----BEGIN") || data.windows(11).any(|w| w == b"-----BEGIN ") {
            let text = std::str::from_utf8(data).context("certificate PEM is not UTF-8")?;
            let der = pem_to_der(text, "CERTIFICATE")?;
            Certificate::from_der(&der)
        } else {
            Certificate::from_der(data)
        }
    }

    /// RFC 2253 subject distinguished name for `--print-certs` output.
    pub fn subject_rfc2253(&self) -> String {
        use der::Decode;
        use x509_cert::name::Name;
        match Name::from_der(&self.subject_der) {
            Ok(name) => name.to_string(),
            Err(_) => String::new(),
        }
    }
}

/// Extracts the DER body of the first PEM block with the given label.
fn pem_to_der(pem: &str, label: &str) -> Result<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = pem.find(&begin).context("PEM begin marker not found")? + begin.len();
    let stop = pem[start..]
        .find(&end)
        .context("PEM end marker not found")?
        + start;
    let b64: String = pem[start..stop]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    crate::base64::decode(&b64).context("decoding base64 in PEM")
}

/// APK Signing Block signature algorithms (`SignatureAlgorithm.java`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureAlgorithm {
    RsaPssWithSha256,
    RsaPssWithSha512,
    RsaPkcs1V15WithSha256,
    RsaPkcs1V15WithSha512,
    EcdsaWithSha256,
    EcdsaWithSha512,
    DsaWithSha256,
    DetDsaWithSha256,
    VerityRsaPkcs1V15WithSha256,
    VerityEcdsaWithSha256,
    VerityDsaWithSha256,
}

impl SignatureAlgorithm {
    pub fn id(self) -> u32 {
        match self {
            SignatureAlgorithm::RsaPssWithSha256 => 0x0101,
            SignatureAlgorithm::RsaPssWithSha512 => 0x0102,
            SignatureAlgorithm::RsaPkcs1V15WithSha256 => 0x0103,
            SignatureAlgorithm::RsaPkcs1V15WithSha512 => 0x0104,
            SignatureAlgorithm::EcdsaWithSha256 => 0x0201,
            SignatureAlgorithm::EcdsaWithSha512 => 0x0202,
            SignatureAlgorithm::DsaWithSha256 => 0x0301,
            SignatureAlgorithm::DetDsaWithSha256 => 0x0301,
            SignatureAlgorithm::VerityRsaPkcs1V15WithSha256 => 0x0421,
            SignatureAlgorithm::VerityEcdsaWithSha256 => 0x0423,
            SignatureAlgorithm::VerityDsaWithSha256 => 0x0425,
        }
    }

    pub fn from_id(id: u32) -> Option<SignatureAlgorithm> {
        Some(match id {
            0x0101 => SignatureAlgorithm::RsaPssWithSha256,
            0x0102 => SignatureAlgorithm::RsaPssWithSha512,
            0x0103 => SignatureAlgorithm::RsaPkcs1V15WithSha256,
            0x0104 => SignatureAlgorithm::RsaPkcs1V15WithSha512,
            0x0201 => SignatureAlgorithm::EcdsaWithSha256,
            0x0202 => SignatureAlgorithm::EcdsaWithSha512,
            0x0301 => SignatureAlgorithm::DsaWithSha256,
            0x0421 => SignatureAlgorithm::VerityRsaPkcs1V15WithSha256,
            0x0423 => SignatureAlgorithm::VerityEcdsaWithSha256,
            0x0425 => SignatureAlgorithm::VerityDsaWithSha256,
            _ => return None,
        })
    }

    pub fn content_digest_algorithm(self) -> ContentDigestAlgorithm {
        match self {
            SignatureAlgorithm::RsaPssWithSha256
            | SignatureAlgorithm::RsaPkcs1V15WithSha256
            | SignatureAlgorithm::EcdsaWithSha256
            | SignatureAlgorithm::DsaWithSha256
            | SignatureAlgorithm::DetDsaWithSha256 => ContentDigestAlgorithm::ChunkedSha256,
            SignatureAlgorithm::RsaPssWithSha512
            | SignatureAlgorithm::RsaPkcs1V15WithSha512
            | SignatureAlgorithm::EcdsaWithSha512 => ContentDigestAlgorithm::ChunkedSha512,
            SignatureAlgorithm::VerityRsaPkcs1V15WithSha256
            | SignatureAlgorithm::VerityEcdsaWithSha256
            | SignatureAlgorithm::VerityDsaWithSha256 => {
                ContentDigestAlgorithm::VerityChunkedSha256
            }
        }
    }

    /// JCA signature algorithm name passed to [`PrivateKey::sign`].
    pub fn jca_signature_algorithm(self) -> &'static str {
        match self {
            SignatureAlgorithm::RsaPssWithSha256 => "SHA256withRSA/PSS",
            SignatureAlgorithm::RsaPssWithSha512 => "SHA512withRSA/PSS",
            SignatureAlgorithm::RsaPkcs1V15WithSha256 => "SHA256withRSA",
            SignatureAlgorithm::RsaPkcs1V15WithSha512 => "SHA512withRSA",
            SignatureAlgorithm::EcdsaWithSha256 => "SHA256withECDSA",
            SignatureAlgorithm::EcdsaWithSha512 => "SHA512withECDSA",
            SignatureAlgorithm::DsaWithSha256 => "SHA256withDSA",
            SignatureAlgorithm::DetDsaWithSha256 => "SHA256withDetDSA",
            SignatureAlgorithm::VerityRsaPkcs1V15WithSha256 => "SHA256withRSA",
            SignatureAlgorithm::VerityEcdsaWithSha256 => "SHA256withECDSA",
            SignatureAlgorithm::VerityDsaWithSha256 => "SHA256withDSA",
        }
    }

    pub fn min_sdk_version(self) -> u32 {
        match self {
            SignatureAlgorithm::VerityRsaPkcs1V15WithSha256
            | SignatureAlgorithm::VerityEcdsaWithSha256
            | SignatureAlgorithm::VerityDsaWithSha256 => crate::sdk::P,
            _ => crate::sdk::N,
        }
    }
}

/// `V2/V3SchemeSigner.getSuggestedSignatureAlgorithms`.
pub fn suggested_signature_algorithms(
    key: &PrivateKey,
    verity_enabled: bool,
    deterministic_dsa: bool,
) -> Result<Vec<SignatureAlgorithm>> {
    let mut algs = Vec::new();
    match key.algorithm() {
        KeyAlgorithm::Rsa => {
            if key.key_size_bits() <= 3072 {
                algs.push(SignatureAlgorithm::RsaPkcs1V15WithSha256);
                if verity_enabled {
                    algs.push(SignatureAlgorithm::VerityRsaPkcs1V15WithSha256);
                }
            } else {
                algs.push(SignatureAlgorithm::RsaPkcs1V15WithSha512);
            }
        }
        KeyAlgorithm::Dsa => {
            algs.push(if deterministic_dsa {
                SignatureAlgorithm::DetDsaWithSha256
            } else {
                SignatureAlgorithm::DsaWithSha256
            });
            if verity_enabled {
                algs.push(SignatureAlgorithm::VerityDsaWithSha256);
            }
        }
        KeyAlgorithm::Ec => {
            if key.key_size_bits() <= 256 {
                algs.push(SignatureAlgorithm::EcdsaWithSha256);
                if verity_enabled {
                    algs.push(SignatureAlgorithm::VerityEcdsaWithSha256);
                }
            } else {
                algs.push(SignatureAlgorithm::EcdsaWithSha512);
            }
        }
    }
    Ok(algs)
}

/// v1 JAR digest algorithm (`V1SchemeSigner.getSuggestedSignatureDigestAlgorithm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlgorithm {
    Sha1,
    Sha256,
}

impl DigestAlgorithm {
    pub fn jca_digest(self) -> &'static str {
        match self {
            DigestAlgorithm::Sha1 => "SHA-1",
            DigestAlgorithm::Sha256 => "SHA-256",
        }
    }

    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        use sha1::Digest as _;
        match self {
            DigestAlgorithm::Sha1 => sha1::Sha1::digest(data).to_vec(),
            DigestAlgorithm::Sha256 => {
                use sha2::Digest;
                sha2::Sha256::digest(data).to_vec()
            }
        }
    }
}

pub fn suggested_v1_digest_algorithm(key: &PrivateKey, min_sdk: u32) -> Result<DigestAlgorithm> {
    match key.algorithm() {
        KeyAlgorithm::Rsa => Ok(if min_sdk < crate::sdk::JELLY_BEAN_MR2 {
            DigestAlgorithm::Sha1
        } else {
            DigestAlgorithm::Sha256
        }),
        KeyAlgorithm::Dsa => Ok(if min_sdk < crate::sdk::LOLLIPOP {
            DigestAlgorithm::Sha1
        } else {
            DigestAlgorithm::Sha256
        }),
        KeyAlgorithm::Ec => {
            if min_sdk < crate::sdk::JELLY_BEAN_MR2 {
                bail!("ECDSA signatures only supported for minSdkVersion 18 and higher");
            }
            Ok(DigestAlgorithm::Sha256)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_algorithm_ids_roundtrip() {
        for id in [0x0101, 0x0103, 0x0104, 0x0201, 0x0202, 0x0301, 0x0421] {
            assert_eq!(SignatureAlgorithm::from_id(id).unwrap().id(), id);
        }
    }
}
