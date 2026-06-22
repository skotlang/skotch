//! Byte-identity tests against AOSP apksig's golden APKs.
//!
//! Each case signs one of the `golden-*-in.apk` inputs with the `rsa-2048`
//! key and asserts the output matches the committed `golden-*-out.apk` from
//! apksig's own test suite, exactly as apksigner would produce it.

use skotch_apksig::{ApkSigner, Certificate, PrivateKey, SignerConfig, SigningCertificateLineage};
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
        // apksig's golden tests name the signer after the key resource
        // ("rsa-2048"), which the v1 scheme sanitizes to "RSA-2048".
        name: "rsa-2048".to_string(),
        key,
        certificates: vec![cert],
        min_sdk_version: 0,
        deterministic_dsa: false,
    }
}

/// (v1, v2, v3) enablement for a golden suffix.
fn config_for(suffix: &str) -> (bool, bool, bool) {
    match suffix {
        "" => (true, true, true),
        "v1" => (true, false, false),
        "v2" => (false, true, false),
        "v3" => (false, false, true),
        "v1v2" => (true, true, false),
        "v2v3" => (false, true, true),
        "v1v2v3" => (true, true, true),
        other => panic!("unknown suffix {other}"),
    }
}

fn golden_name(input_stem: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        format!("{input_stem}-out.apk")
    } else {
        format!("{input_stem}-{suffix}-out.apk")
    }
}

fn run_case(input_stem: &str, suffix: &str) {
    let fx = fixtures();
    let input = std::fs::read(fx.join("in").join(format!("{input_stem}-in.apk"))).unwrap();
    let (v1, v2, v3) = config_for(suffix);

    // apksig's `assertGolden` (which the committed golden-*-out.apk match)
    // always signs with alignmentPreserved=true. The legacy-aligned goldens
    // predate the 16k native-library page-alignment default, so apksig pins
    // them to 4096 — we do the same.
    let mut signer = ApkSigner::new(vec![rsa2048_signer()])
        .v1_signing_enabled(v1)
        .v2_signing_enabled(v2)
        .v3_signing_enabled(v3)
        .v4_signing_enabled(false)
        .alignment_preserved(true);
    if input_stem == "golden-legacy-aligned" {
        signer = signer.lib_page_alignment(4096);
    }
    let result = signer.sign(&input).expect("sign");

    let golden_path = fx.join("golden").join(golden_name(input_stem, suffix));
    let golden = std::fs::read(&golden_path)
        .unwrap_or_else(|_| panic!("golden missing: {}", golden_path.display()));

    if result.apk != golden {
        if std::env::var("DUMP").is_ok() {
            let base = format!("/tmp/apksig-{input_stem}-{suffix}");
            std::fs::write(format!("{base}-produced.apk"), &result.apk).unwrap();
            std::fs::write(format!("{base}-golden.apk"), &golden).unwrap();
        }
        panic!(
            "mismatch for {input_stem} [{suffix}]: produced {} bytes, golden {} bytes; first diff at {:?}",
            result.apk.len(),
            golden.len(),
            first_diff(&result.apk, &golden),
        );
    }
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    (0..a.len().min(b.len())).find(|&i| a[i] != b[i])
}

macro_rules! golden_tests {
    ($($name:ident: $input:literal, $suffix:literal;)*) => {
        $(
            #[test]
            fn $name() {
                run_case($input, $suffix);
            }
        )*
    };
}

fn second_rsa2048_signer() -> SignerConfig {
    let dir = fixtures().join("keys");
    let key =
        PrivateKey::from_pkcs8_der(&std::fs::read(dir.join("rsa-2048_2.pk8")).unwrap()).unwrap();
    let cert =
        Certificate::from_pem_or_der(&std::fs::read(dir.join("rsa-2048_2.x509.pem")).unwrap())
            .unwrap();
    SignerConfig {
        name: "rsa-2048_2".to_string(),
        key,
        certificates: vec![cert],
        min_sdk_version: 0,
        deterministic_dsa: false,
    }
}

/// Lineage-based (key-rotation) golden cases: two signers + a lineage, v1/v2
/// signed by the old key and v3 by the new key with a proof-of-rotation.
fn run_lineage_case(input_stem: &str, suffix: &str, v1: bool, v2: bool) {
    let fx = fixtures();
    let input = std::fs::read(fx.join("in").join(format!("{input_stem}-in.apk"))).unwrap();
    let lineage = SigningCertificateLineage::from_bytes(
        &std::fs::read(fx.join("keys/rsa-2048-lineage-2-signers")).unwrap(),
    )
    .unwrap();

    let signer = ApkSigner::new(vec![rsa2048_signer(), second_rsa2048_signer()])
        .v1_signing_enabled(v1)
        .v2_signing_enabled(v2)
        .v3_signing_enabled(true)
        .v4_signing_enabled(false)
        .alignment_preserved(true)
        .rotation_min_sdk_version(28) // AndroidSdkVersion.P
        .signing_certificate_lineage(lineage);
    let result = signer.sign(&input).expect("sign");

    let golden_path = fx
        .join("golden")
        .join(format!("{input_stem}-{suffix}-out.apk"));
    let golden = std::fs::read(&golden_path)
        .unwrap_or_else(|_| panic!("golden missing: {}", golden_path.display()));
    if result.apk != golden {
        if std::env::var("DUMP").is_ok() {
            let base = format!("/tmp/apksig-{input_stem}-{suffix}");
            std::fs::write(format!("{base}-produced.apk"), &result.apk).unwrap();
            std::fs::write(format!("{base}-golden.apk"), &golden).unwrap();
        }
        panic!(
            "lineage mismatch for {input_stem} [{suffix}]: produced {} bytes, golden {} bytes; first diff at {:?}",
            result.apk.len(),
            golden.len(),
            first_diff(&result.apk, &golden),
        );
    }
}

/// Signs an input APK with a builder-configuring closure and compares against
/// a named golden output. Used for the `original.apk` family.
fn run_signing_golden(
    input_name: &str,
    golden_name: &str,
    configure: impl FnOnce(ApkSigner) -> ApkSigner,
) {
    let fx = fixtures();
    let input = std::fs::read(fx.join("in").join(input_name)).unwrap();
    let base = ApkSigner::new(vec![rsa2048_signer()])
        .v4_signing_enabled(false)
        .alignment_preserved(true);
    let result = configure(base).sign(&input).expect("sign");
    let golden = std::fs::read(fx.join("golden").join(golden_name))
        .unwrap_or_else(|_| panic!("golden missing: {golden_name}"));
    if result.apk != golden {
        if std::env::var("DUMP").is_ok() {
            std::fs::write(format!("/tmp/sg-{golden_name}-produced.apk"), &result.apk).unwrap();
            std::fs::write(format!("/tmp/sg-{golden_name}-golden.apk"), &golden).unwrap();
        }
        panic!(
            "signing-golden mismatch for {golden_name}: produced {} bytes, golden {} bytes; first diff at {:?}",
            result.apk.len(),
            golden.len(),
            first_diff(&result.apk, &golden),
        );
    }
}

#[test]
fn original_default() {
    run_signing_golden("original.apk", "golden-rsa-out.apk", |b| b);
}
#[test]
fn original_min_sdk_1() {
    run_signing_golden("original.apk", "golden-rsa-minSdkVersion-1-out.apk", |b| {
        b.min_sdk_version(1)
    });
}
#[test]
fn original_min_sdk_18() {
    run_signing_golden("original.apk", "golden-rsa-minSdkVersion-18-out.apk", |b| {
        b.min_sdk_version(18)
    });
}
#[test]
fn original_min_sdk_24() {
    run_signing_golden("original.apk", "golden-rsa-minSdkVersion-24-out.apk", |b| {
        b.min_sdk_version(24)
    });
}
#[test]
fn original_verity() {
    run_signing_golden("original.apk", "golden-rsa-verity-out.apk", |b| {
        b.v1_signing_enabled(true)
            .v2_signing_enabled(true)
            .v3_signing_enabled(true)
            .verity_enabled(true)
    });
}
#[test]
fn original_file_size_aligned() {
    run_signing_golden("original.apk", "golden-file-size-aligned.apk", |b| {
        b.align_file_size(true)
    });
}

#[test]
fn aligned_v3_lineage() {
    run_lineage_case("golden-aligned", "v3-lineage", false, false);
}
#[test]
fn aligned_v2v3_lineage() {
    run_lineage_case("golden-aligned", "v2v3-lineage", false, true);
}
#[test]
fn aligned_v1v2v3_lineage() {
    run_lineage_case("golden-aligned", "v1v2v3-lineage", true, true);
}

golden_tests! {
    aligned_default:        "golden-aligned", "";
    aligned_v1:             "golden-aligned", "v1";
    aligned_v2:             "golden-aligned", "v2";
    aligned_v3:             "golden-aligned", "v3";
    aligned_v1v2:           "golden-aligned", "v1v2";
    aligned_v2v3:           "golden-aligned", "v2v3";
    aligned_v1v2v3:         "golden-aligned", "v1v2v3";

    unaligned_default:      "golden-unaligned", "";
    unaligned_v1:           "golden-unaligned", "v1";
    unaligned_v2:           "golden-unaligned", "v2";
    unaligned_v3:           "golden-unaligned", "v3";
    unaligned_v1v2:         "golden-unaligned", "v1v2";
    unaligned_v2v3:         "golden-unaligned", "v2v3";
    unaligned_v1v2v3:       "golden-unaligned", "v1v2v3";

    legacy_default:         "golden-legacy-aligned", "";
    legacy_v1:              "golden-legacy-aligned", "v1";
    legacy_v2:              "golden-legacy-aligned", "v2";
    legacy_v3:              "golden-legacy-aligned", "v3";
    legacy_v1v2:            "golden-legacy-aligned", "v1v2";
    legacy_v2v3:            "golden-legacy-aligned", "v2v3";
    legacy_v1v2v3:          "golden-legacy-aligned", "v1v2v3";
}
