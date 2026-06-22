//! Sign-then-verify round trips and verification of apksig's golden signed
//! APKs, exercising the [`ApkVerifier`] path the `apksigner verify` CLI uses.

use skotch_apksig::{ApkSigner, ApkVerifier, Certificate, PrivateKey, SignerConfig};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/apksigner")
        .canonicalize()
        .expect("fixtures dir")
}

fn rsa2048_signer() -> SignerConfig {
    let dir = fixtures().join("keys");
    let key =
        PrivateKey::from_pkcs8_der(&std::fs::read(dir.join("rsa-2048.pk8")).unwrap()).unwrap();
    let cert = Certificate::from_pem_or_der(&std::fs::read(dir.join("rsa-2048.x509.pem")).unwrap())
        .unwrap();
    SignerConfig {
        name: "rsa-2048".to_string(),
        key,
        certificates: vec![cert],
        min_sdk_version: 0,
        deterministic_dsa: false,
    }
}

fn input() -> Vec<u8> {
    std::fs::read(fixtures().join("in/golden-aligned-in.apk")).unwrap()
}

#[test]
fn sign_then_verify_v2_v3() {
    let result = ApkSigner::new(vec![rsa2048_signer()])
        .v1_signing_enabled(false)
        .v2_signing_enabled(true)
        .v3_signing_enabled(true)
        .v4_signing_enabled(false)
        .alignment_preserved(true)
        .sign(&input())
        .unwrap();

    let report = ApkVerifier::new().verify(&result.apk).unwrap();
    assert!(report.verified, "errors: {:?}", report.errors);
    assert!(report.verified_v2);
    assert!(report.verified_v3);
    assert!(!report.verified_v1);
    assert_eq!(report.signer_certs.len(), 1);
}

#[test]
fn sign_then_verify_v1_v2_v3() {
    let result = ApkSigner::new(vec![rsa2048_signer()])
        .v1_signing_enabled(true)
        .v2_signing_enabled(true)
        .v3_signing_enabled(true)
        .v4_signing_enabled(false)
        .alignment_preserved(true)
        .sign(&input())
        .unwrap();

    let report = ApkVerifier::new().verify(&result.apk).unwrap();
    assert!(report.verified, "errors: {:?}", report.errors);
    assert!(report.verified_v1);
    assert!(report.verified_v2);
    assert!(report.verified_v3);
}

#[test]
fn verify_golden_signed_apk() {
    // apksig's own golden output must verify cleanly through our verifier.
    let golden = std::fs::read(fixtures().join("golden/golden-aligned-v1v2v3-out.apk")).unwrap();
    let report = ApkVerifier::new().verify(&golden).unwrap();
    assert!(report.verified, "errors: {:?}", report.errors);
    assert!(report.verified_v1);
    assert!(report.verified_v2);
    assert!(report.verified_v3);
}

#[test]
fn tampered_apk_fails_verification() {
    let result = ApkSigner::new(vec![rsa2048_signer()])
        .v1_signing_enabled(false)
        .v2_signing_enabled(true)
        .v3_signing_enabled(false)
        .v4_signing_enabled(false)
        .alignment_preserved(true)
        .sign(&input())
        .unwrap();

    // Flip a byte in the first entry's data region (well before the CD).
    let mut tampered = result.apk.clone();
    tampered[64] ^= 0xff;
    let report = ApkVerifier::new().verify(&tampered).unwrap();
    assert!(!report.verified, "tampered APK should not verify");
}

#[test]
fn v4_signature_roundtrip() {
    let result = ApkSigner::new(vec![rsa2048_signer()])
        .v1_signing_enabled(false)
        .v2_signing_enabled(true)
        .v3_signing_enabled(true)
        .v4_signing_enabled(true)
        .alignment_preserved(true)
        .sign(&input())
        .unwrap();
    let idsig = result.v4_signature.expect("v4 signature produced");
    let ok = ApkVerifier::new().verify_v4(&result.apk, &idsig).unwrap();
    assert!(ok, "v4 signature should verify against the APK");
}
