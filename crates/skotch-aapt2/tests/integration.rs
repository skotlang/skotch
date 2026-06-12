//! End-to-end tests over the AOSP aapt2 integration-test fixtures:
//! compile the real `res/` trees, link against the real framework
//! `android-33.jar`, and verify the produced APKs by reading them back
//! with the crate's own parsers.
//!
//! Fixture root: `android/base/tools/aapt2/integration-tests` in the
//! skotlang checkout. Tests skip silently when it is absent so the
//! suite stays green on machines without the AOSP tree.

use skotch_aapt2::apk::LoadedApk;
use skotch_aapt2::compile::{compile, CompileOptions};
use skotch_aapt2::diag::Diagnostics;
use skotch_aapt2::link::{link, LinkOptions, OutputFormat};
use skotch_aapt2::res::{ResourceName, ResourceType};
use std::path::{Path, PathBuf};

fn fixtures_root() -> Option<PathBuf> {
    let path = PathBuf::from("/opt/src/github/skotlang/android/base/tools/aapt2/integration-tests");
    path.is_dir().then_some(path)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("skotch-aapt2-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn compile_res_dir(res_dir: &Path, out: &Path) -> anyhow::Result<()> {
    let diag = Diagnostics::collecting();
    let mut options = CompileOptions::new();
    options.res_dir = Some(res_dir.to_path_buf());
    options.legacy_mode = true; // CompileTest has values.all.xml (period in name)
    compile(&[], out, &options, &diag)
}

fn link_apk(
    manifest: &Path,
    compiled: &Path,
    framework: &Path,
    output: &Path,
    java_dir: Option<&Path>,
    proto: bool,
) -> anyhow::Result<skotch_aapt2::link::LinkOutputs> {
    let diag = Diagnostics::collecting();
    let mut options = LinkOptions {
        output_path: output.to_path_buf(),
        manifest_path: manifest.to_path_buf(),
        ..Default::default()
    };
    if framework.exists() {
        options.include_paths.push(framework.to_path_buf());
    }
    options.generate_java_class_path = java_dir.map(Path::to_path_buf);
    options.output_format = if proto { OutputFormat::Proto } else { OutputFormat::Binary };
    options.manifest_fixer_options.min_sdk_version_default = Some("21".to_string());
    options.manifest_fixer_options.target_sdk_version_default = Some("33".to_string());
    options.verbose = false;
    let result = link(&[compiled.to_path_buf()], &options, &diag);
    if result.is_err() {
        for message in diag.take() {
            eprintln!("{message}");
        }
    }
    result
}

#[test]
fn basic_test_compile_link_and_verify() {
    let Some(root) = fixtures_root() else { return };
    let dir = temp_dir("basic");
    let compiled = dir.join("compiled.zip");
    compile_res_dir(&root.join("BasicTest/res"), &compiled).expect("compile BasicTest");

    let apk_path = dir.join("out.apk");
    let java_dir = dir.join("java");
    link_apk(
        &root.join("BasicTest/AndroidManifest.xml"),
        &compiled,
        &root.join("CommandTests/android-33.jar"),
        &apk_path,
        Some(&java_dir),
        false,
    )
    .expect("link BasicTest");

    // Read the APK back with our own parsers.
    let diag = Diagnostics::collecting();
    let apk = LoadedApk::load(&apk_path, &diag).expect("reload APK");
    assert_eq!(apk.format, skotch_aapt2::apk::ApkFormat::Binary);
    let package_name = apk.package_name().expect("package name");
    assert_eq!(package_name, "com.android.aapt.basic");

    // BasicTest defines attr/format_conflict via two styleables.
    let attr_name =
        ResourceName::new(package_name.clone(), ResourceType::Attr, "format_conflict");
    let entry = apk
        .table
        .find_resource(&attr_name)
        .expect("attr/format_conflict in linked table");
    assert!(entry.entry.id.is_some());

    // The manifest must parse as binary XML with injected SDK levels.
    let manifest = apk.manifest().expect("binary manifest parses");
    let uses_sdk = manifest.find_child("", "uses-sdk").expect("uses-sdk injected");
    assert!(uses_sdk
        .attributes
        .iter()
        .any(|a| a.name == "minSdkVersion"));

    // R.java was generated with the assigned IDs.
    let r_java = std::fs::read_to_string(java_dir.join("com/android/aapt/basic/R.java"))
        .expect("R.java written");
    assert!(r_java.contains("class attr"), "{r_java}");
    assert!(r_java.contains("format_conflict=0x7f"), "{r_java}");
    assert!(r_java.contains("class styleable"), "{r_java}");
}

#[test]
fn compile_test_res_dir_with_pngs() {
    let Some(root) = fixtures_root() else { return };
    let dir = temp_dir("compile");
    let compiled = dir.join("compiled.zip");
    compile_res_dir(&root.join("CompileTest/res"), &compiled).expect("compile CompileTest");

    // Every artifact must be a valid container.
    let artifacts = skotch_aapt2::compile::read_artifacts(&compiled).expect("read flat zip");
    assert!(!artifacts.is_empty());
    let mut seen_nine_patch = false;
    for (name, data) in &artifacts {
        let entries =
            skotch_aapt2::container::read_container(data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!entries.is_empty(), "{name} has no entries");
        seen_nine_patch |= name.contains(".9.png");
    }
    assert!(seen_nine_patch, "9-patch was compiled");

    // The 9-patch artifact must contain an npTc chunk.
    let (_, nine_patch) = artifacts
        .iter()
        .find(|(name, _)| name.ends_with(".9.png.flat"))
        .expect("nine-patch artifact");
    let entries = skotch_aapt2::container::read_container(nine_patch).unwrap();
    match &entries[0] {
        skotch_aapt2::container::ContainerEntry::ResFile { data, .. } => {
            assert!(
                data.windows(4).any(|w| w == b"npTc"),
                "compiled 9-patch carries the npTc chunk"
            );
        }
        other => panic!("unexpected entry {other:?}"),
    }
}

#[test]
fn auto_version_test_links() {
    let Some(root) = fixtures_root() else { return };
    let dir = temp_dir("autoversion");
    let compiled = dir.join("compiled.zip");
    compile_res_dir(&root.join("AutoVersionTest/res"), &compiled).expect("compile");
    let apk_path = dir.join("out.apk");
    link_apk(
        &root.join("AutoVersionTest/AndroidManifest.xml"),
        &compiled,
        &root.join("CommandTests/android-33.jar"),
        &apk_path,
        None,
        false,
    )
    .expect("link AutoVersionTest");

    let diag = Diagnostics::collecting();
    let apk = LoadedApk::load(&apk_path, &diag).expect("reload");
    // The layout must be in the table and present in the zip.
    let package = apk.package_name().unwrap();
    let layout = apk
        .table
        .find_resource(&ResourceName::new(package, ResourceType::Layout, "layout"))
        .expect("layout/layout");
    let mut found_file = false;
    for config_value in &layout.entry.values {
        if let Some(value) = &config_value.value {
            if let skotch_aapt2::res::value::ValueKind::Item(
                skotch_aapt2::res::value::Item::FileReference(file),
            ) = &value.kind
            {
                found_file = true;
                assert!(
                    apk.entry(&file.path).is_some(),
                    "{} missing from zip",
                    file.path
                );
                // And it must parse as binary XML.
                let data = apk.entry(&file.path).unwrap();
                skotch_aapt2::xml::axml::parse_binary_xml(data).expect("layout parses");
            }
        }
    }
    assert!(found_file);
}

#[test]
fn proto_format_link_and_convert_round_trip() {
    let Some(root) = fixtures_root() else { return };
    let dir = temp_dir("proto");
    let compiled = dir.join("compiled.zip");
    compile_res_dir(&root.join("BasicTest/res"), &compiled).expect("compile");

    // Link with --proto-format.
    let proto_apk_path = dir.join("proto.apk");
    link_apk(
        &root.join("BasicTest/AndroidManifest.xml"),
        &compiled,
        &root.join("CommandTests/android-33.jar"),
        &proto_apk_path,
        None,
        true,
    )
    .expect("link proto");

    let diag = Diagnostics::collecting();
    let proto_apk = LoadedApk::load(&proto_apk_path, &diag).expect("load proto APK");
    assert_eq!(proto_apk.format, skotch_aapt2::apk::ApkFormat::Proto);

    // Convert proto → binary (what bundletool/aapt2 convert does).
    let binary_apk_path = dir.join("binary.apk");
    skotch_aapt2::convert::convert_apk(
        &proto_apk,
        &binary_apk_path,
        OutputFormat::Binary,
        false,
        false,
        false,
        &diag,
    )
    .expect("convert to binary");

    let binary_apk = LoadedApk::load(&binary_apk_path, &diag).expect("load converted APK");
    assert_eq!(binary_apk.format, skotch_aapt2::apk::ApkFormat::Binary);
    let package = binary_apk.package_name().unwrap();
    assert!(binary_apk
        .table
        .find_resource(&ResourceName::new(package, ResourceType::Attr, "format_conflict"))
        .is_some());
}

#[test]
fn framework_table_parses() {
    let Some(root) = fixtures_root() else { return };
    let jar = root.join("CommandTests/android-33.jar");
    if !jar.exists() {
        return;
    }
    let diag = Diagnostics::collecting();
    let apk = LoadedApk::load(&jar, &diag).expect("load android-33.jar");
    let android = apk.table.find_package("android").expect("android package");
    assert!(android.types.len() > 20, "{} types", android.types.len());
    let ok = apk
        .table
        .find_resource(&ResourceName::new("android", ResourceType::String, "ok"))
        .expect("android:string/ok");
    assert_eq!(ok.entry.id.map(|id| id.package_id()), Some(0x01));
}
