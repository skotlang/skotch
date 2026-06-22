//! Verification tests against apksig's own pre-signed APKs, replicating
//! `ApkVerifierTest`. These exercise the verifier across key types (RSA,
//! EC, DSA), digests (SHA-256/512, RSA-PSS), and schemes (v1/v2/v3/v3.1) that
//! the byte-identity signing tests can't reach (EC/DSA use random nonces).

use skotch_apksig::ApkVerifier;
use std::path::{Path, PathBuf};

fn signed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/apksigner/signed")
        .canonicalize()
        .expect("signed fixtures dir")
}

fn verify(name: &str) -> skotch_apksig::verify::VerificationResult {
    let apk = std::fs::read(signed_dir().join(name)).unwrap();
    ApkVerifier::new()
        .verify(&apk)
        .unwrap_or_else(|e| panic!("verify {name} errored: {e}"))
}

/// Positive: must verify, with the expected scheme flags.
fn assert_verifies(name: &str, v1: bool, v2: bool, v3: bool) {
    let r = verify(name);
    assert!(r.verified, "{name} should verify; errors: {:?}", r.errors);
    assert_eq!(r.verified_v1, v1, "{name} v1 flag");
    assert_eq!(r.verified_v2, v2, "{name} v2 flag");
    assert_eq!(r.verified_v3, v3, "{name} v3 flag");
}

/// Negative: must NOT verify.
fn assert_fails(name: &str) {
    let r = verify(name);
    assert!(!r.verified, "{name} should NOT verify but did");
}

// ── v1 ────────────────────────────────────────────────────────────────────

#[test]
fn v1_rsa_sha256_2048() {
    assert_verifies(
        "v1-only-with-rsa-pkcs1-sha256-1.2.840.113549.1.1.1-2048.apk",
        true,
        false,
        false,
    );
}

// ── v2 across key types ─────────────────────────────────────────────────────

#[test]
fn v2_rsa_sha256_2048() {
    assert_verifies("v2-only-with-rsa-pkcs1-sha256-2048.apk", false, true, false);
}
#[test]
fn v2_rsa_sha512_4096() {
    assert_verifies("v2-only-with-rsa-pkcs1-sha512-4096.apk", false, true, false);
}
#[test]
fn v2_rsa_pss_sha256_2048() {
    assert_verifies("v2-only-with-rsa-pss-sha256-2048.apk", false, true, false);
}
#[test]
fn v2_ecdsa_sha256_p256() {
    assert_verifies("v2-only-with-ecdsa-sha256-p256.apk", false, true, false);
}
#[test]
fn v2_ecdsa_sha512_p384() {
    assert_verifies("v2-only-with-ecdsa-sha512-p384.apk", false, true, false);
}
#[test]
fn v2_dsa_sha256_2048() {
    assert_verifies("v2-only-with-dsa-sha256-2048.apk", false, true, false);
}
#[test]
fn v2_two_signers() {
    assert_verifies("v2-only-two-signers.apk", false, true, false);
}

// ── v3 across key types ─────────────────────────────────────────────────────

#[test]
fn v3_rsa_sha256_2048() {
    assert_verifies("v3-only-with-rsa-pkcs1-sha256-2048.apk", false, false, true);
}
#[test]
fn v3_rsa_sha512_4096() {
    assert_verifies("v3-only-with-rsa-pkcs1-sha512-4096.apk", false, false, true);
}
#[test]
fn v3_ecdsa_sha256_p256() {
    assert_verifies("v3-only-with-ecdsa-sha256-p256.apk", false, false, true);
}
#[test]
fn v3_dsa_sha256_2048() {
    assert_verifies("v3-only-with-dsa-sha256-2048.apk", false, false, true);
}

// ── v3.1 ─────────────────────────────────────────────────────────────────────

#[test]
fn v31_rsa_tgt_33() {
    let r = verify("v31-rsa-2048_2-tgt-33-1-tgt-28.apk");
    assert!(r.verified, "v31 should verify; errors: {:?}", r.errors);
    assert!(r.verified_v31, "v3.1 scheme should be verified");
}
#[test]
fn v31_rsa_dev_release() {
    let r = verify("v31-rsa-2048_2-tgt-10000-dev-release.apk");
    assert!(
        r.verified,
        "v31 dev-release should verify; errors: {:?}",
        r.errors
    );
}

// ── v4 ──────────────────────────────────────────────────────────────────────

#[test]
fn v4_idsig_verifies() {
    let apk = std::fs::read(signed_dir().join("v31-rsa-2048_2-tgt-10000-dev-release.apk")).unwrap();
    let idsig =
        std::fs::read(signed_dir().join("v31-rsa-2048_2-tgt-10000-dev-release.apk.idsig")).unwrap();
    let ok = ApkVerifier::new()
        .verify_v4(&apk, &idsig)
        .expect("v4 verify errored");
    assert!(ok, "apksig's golden .idsig should verify against its APK");
}

// ── Negative ──────────────────────────────────────────────────────────────────

#[test]
fn neg_sig_does_not_verify() {
    assert_fails("v2-only-with-rsa-pkcs1-sha256-2048-sig-does-not-verify.apk");
}
#[test]
fn neg_digest_mismatch() {
    assert_fails("v2-only-with-rsa-pkcs1-sha512-4096-digest-mismatch.apk");
}
#[test]
fn neg_ecdsa_digest_mismatch() {
    assert_fails("v2-only-with-ecdsa-sha256-p256-digest-mismatch.apk");
}
#[test]
fn neg_v3_dsa_sig_does_not_verify() {
    assert_fails("v3-only-with-dsa-sha256-2048-sig-does-not-verify.apk");
}

// ── Security-critical: stripping protection + malformed blocks ──────────────

#[test]
fn neg_v2_stripped() {
    // v1 present, declares v2 required (X-Android-APK-Signed: 2), v2 block
    // stripped — must be rejected (stripping protection).
    assert_fails("v2-stripped.apk");
}
#[test]
fn neg_v3_stripped() {
    assert_fails("v3-stripped.apk");
}
#[test]
fn neg_cert_pubkey_mismatch_v2() {
    assert_fails("v2-only-cert-and-public-key-mismatch.apk");
}
#[test]
fn neg_cert_pubkey_mismatch_v3() {
    assert_fails("v3-only-cert-and-public-key-mismatch.apk");
}
#[test]
fn neg_sig_alg_mismatch() {
    assert_fails("v2-only-signatures-and-digests-block-mismatch.apk");
}
#[test]
fn neg_no_certs_in_sig() {
    assert_fails("v2-only-no-certs-in-sig.apk");
}
#[test]
fn neg_11_signers() {
    // Exceeds MAX_APK_SIGNERS (10) — must be rejected.
    assert_fails("v2-only-11-signers.apk");
}
#[test]
fn neg_second_signer_no_sig() {
    assert_fails("v2-only-two-signers-second-signer-no-sig.apk");
}
#[test]
fn neg_wrong_block_magic() {
    assert_fails("v2-only-wrong-apk-sig-block-magic.apk");
}
#[test]
fn neg_block_size_mismatch() {
    assert_fails("v2-only-apk-sig-block-size-mismatch.apk");
}
