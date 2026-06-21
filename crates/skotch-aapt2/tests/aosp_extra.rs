//! Additional validation extracted from the AOSP tree:
//!
//! - the feature-flag pipeline, mirroring `link/FlaggedResources_test.cpp`
//!   (which AOSP runs against an APK built by aapt2 itself at build time:
//!   the `resource-flagging-test-app-apk` genrule);
//! - the idmap2 overlay/target APKs (`cmds/idmap2/tests/data`);
//! - broad source-compilation coverage over every `res/` tree in
//!   aapt2's integration-tests (fonts, navigation, transitions, vectors,
//!   nine-patches, namespaced libs, symlinks).
//!
//! Tests skip silently when the AOSP checkout is absent.

use skotch_aapt2::apk::LoadedApk;
use skotch_aapt2::binary::arsc_flattener::{flatten_table, TableFlattenerOptions};
use skotch_aapt2::binary::arsc_parser::parse_table;
use skotch_aapt2::compile::{compile, parse_feature_flags_parameter, CompileOptions};
use skotch_aapt2::diag::Diagnostics;
use skotch_aapt2::link::{link, LinkOptions};
use skotch_aapt2::res::table::ResourceTable;
use std::io::Read as _;
use std::path::{Path, PathBuf};

const AAPT2_IT: &str = "/opt/src/github/skotlang/android/base/tools/aapt2/integration-tests";
const IDMAP2_DATA: &str = "/opt/src/github/skotlang/android/base/cmds/idmap2/tests/data";

fn aapt2_it() -> Option<PathBuf> {
    let path = PathBuf::from(AAPT2_IT);
    path.is_dir().then_some(path)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("skotch-aosp-extra-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn zip_entry(apk: &Path, name: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(apk).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry = archive.by_name(name).ok()?;
    let mut data = Vec::new();
    entry.read_to_end(&mut data).ok()?;
    Some(data)
}

/// Every entry name in the table, for absence/presence assertions.
fn entry_names(table: &ResourceTable) -> Vec<String> {
    let mut out = Vec::new();
    for package in &table.packages {
        for ty in &package.types {
            for entry in &ty.entries {
                out.push(format!("{}/{}", ty.named_type, entry.name));
            }
        }
    }
    out
}

/// Builds a minimal supplementary framework include declaring
/// `android:attr/featureFlag`. AOSP links the flagging test app against
/// the *current* android.jar (which defines it in attrs_manifest.xml);
/// the checked-in android-33.jar predates the attribute.
fn synthesize_feature_flag_framework(dir: &Path) -> PathBuf {
    use skotch_aapt2::res::config::ConfigDescription;
    use skotch_aapt2::res::table::{Visibility, VisibilityLevel};
    use skotch_aapt2::res::value::{format, Attribute, Value, ValueKind};
    use skotch_aapt2::res::{ResourceId, ResourceName, ResourceType};
    use std::io::Write as _;

    let mut table = ResourceTable::new();
    let name = ResourceName::new("android", ResourceType::Attr, "featureFlag");
    table
        .add_resource(
            skotch_aapt2::res::table::NewResource::with_name(name.clone())
                .config(ConfigDescription::default())
                .value(Value::new(ValueKind::Attribute(Attribute::new(
                    format::STRING,
                ))))
                .id(ResourceId(0x0101_1001))
                .visibility(Visibility {
                    level: VisibilityLevel::Public,
                    ..Default::default()
                }),
        )
        .unwrap();
    let arsc = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();

    let path = dir.join("framework-featureflag.apk");
    let file = std::fs::File::create(&path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    writer
        .start_file("resources.arsc", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(&arsc).unwrap();
    writer.finish().unwrap();
    path
}

// ───────────────────── feature flags end-to-end ─────────────────────

/// Mirrors the `resource-flagging-test-app-apk` genrule plus the
/// assertions in `FlaggedResources_test.cpp`: resources behind a
/// disabled read-only flag must be erased from the final APK.
#[test]
fn flagged_resources_disabled_resources_removed() {
    let Some(it) = aapt2_it() else { return };
    let fixture = it.join("FlaggedResourcesTest");
    let dir = temp_dir("flagged");

    // compile … --feature-flags test.package.falseFlag:ro=false,test.package.trueFlag:ro=true
    let diag = Diagnostics::collecting();
    let mut compile_options = CompileOptions::new();
    compile_options.res_dir = Some(fixture.join("res"));
    parse_feature_flags_parameter(
        "test.package.falseFlag:ro=false,test.package.trueFlag:ro=true",
        &mut compile_options.feature_flag_values,
    )
    .unwrap();
    let compiled = dir.join("compiled.zip");
    compile(&[], &compiled, &compile_options, &diag).unwrap_or_else(|e| {
        for message in diag.take() {
            eprintln!("{message}");
        }
        panic!("compile FlaggedResourcesTest: {e}");
    });

    // link … with the same flags.
    let apk_path = dir.join("resapp.apk");
    let mut link_options = LinkOptions {
        output_path: apk_path.clone(),
        manifest_path: fixture.join("AndroidManifest.xml"),
        ..Default::default()
    };
    let framework = it.join("CommandTests/android-33.jar");
    if framework.exists() {
        link_options.include_paths.push(framework);
    }
    link_options
        .include_paths
        .push(synthesize_feature_flag_framework(&dir));
    parse_feature_flags_parameter(
        "test.package.falseFlag:ro=false,test.package.trueFlag:ro=true",
        &mut link_options.feature_flag_values,
    )
    .unwrap();
    let link_diag = Diagnostics::collecting();
    link(&[compiled], &link_options, &link_diag).unwrap_or_else(|e| {
        for message in link_diag.take() {
            eprintln!("{message}");
        }
        panic!("link FlaggedResourcesTest: {e}");
    });

    // Port of DisabledResourcesRemovedFromTable / ...FromTableChunks:
    // bool4, str1, layout2, removedpng must not exist in the table.
    let reload_diag = Diagnostics::collecting();
    let apk = LoadedApk::load(&apk_path, &reload_diag).expect("reload resapp.apk");
    let names = entry_names(&apk.table);
    for gone in [
        "bool/bool4",
        "string/str1",
        "layout/layout2",
        "drawable/removedpng",
    ] {
        assert!(
            !names.contains(&gone.to_string()),
            "{gone} should have been removed; table has {names:?}"
        );
    }
    // Enabled / unflagged siblings survive.
    for kept in ["bool/bool1", "layout/layout1", "layout/layout3"] {
        assert!(
            names.contains(&kept.to_string()),
            "{kept} missing; table has {names:?}"
        );
    }

    // Port of DisabledStringRemovedFromPool: the disabled string's text
    // must not survive anywhere in resources.arsc.
    let arsc = zip_entry(&apk_path, "resources.arsc").expect("resources.arsc");
    let needle = b"DONTFIND";
    assert!(
        !arsc.windows(needle.len()).any(|w| w == needle),
        "disabled string text leaked into the string pool"
    );

    // Disabled files must not be packaged.
    let file = std::fs::File::open(&apk_path).unwrap();
    let archive = zip::ZipArchive::new(file).unwrap();
    let entries: Vec<&str> = archive.file_names().collect();
    assert!(
        !entries
            .iter()
            .any(|n| n.contains("removedpng") || n.contains("layout2")),
        "disabled files leaked into the APK: {entries:?}"
    );
}

// ───────────────────── idmap2 corpus ─────────────────────

#[test]
fn idmap2_apks_parse_and_round_trip() {
    let root = PathBuf::from(IDMAP2_DATA);
    if !root.is_dir() {
        return;
    }
    let apks = [
        "target/target.apk",
        "target/target-no-overlayable.apk",
        "overlay/overlay.apk",
        "overlay/overlay-shared.apk",
        "overlay/overlay-legacy.apk",
    ];
    let mut failures = Vec::new();
    for relative in apks {
        let path = root.join(relative);
        let Some(arsc) = zip_entry(&path, "resources.arsc") else {
            continue;
        };
        let table = match parse_table(&arsc) {
            Ok(table) => table,
            Err(e) => {
                failures.push(format!("{relative}: parse failed: {e}"));
                continue;
            }
        };
        match flatten_table(&table, &TableFlattenerOptions::default()) {
            Ok(bytes) => {
                if let Err(e) = parse_table(&bytes) {
                    failures.push(format!("{relative}: reparse failed: {e}"));
                }
            }
            Err(e) => failures.push(format!("{relative}: flatten failed: {e}")),
        }
        // The binary manifests must parse, too.
        if let Some(manifest) = zip_entry(&path, "AndroidManifest.xml") {
            if let Err(e) = skotch_aapt2::xml::axml::parse_binary_xml(&manifest) {
                failures.push(format!("{relative}: manifest parse failed: {e}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The idmap2 target app declares `<overlayable name="TestResources">`
/// with policy groups — richer than androidfw's fixture.
#[test]
fn idmap2_target_overlayable_round_trips() {
    let root = PathBuf::from(IDMAP2_DATA);
    let path = root.join("target/target.apk");
    let Some(arsc) = zip_entry(&path, "resources.arsc") else {
        return;
    };
    let table = parse_table(&arsc).expect("target.apk parses");
    assert!(
        !table.overlayables.is_empty(),
        "target.apk should declare overlayables"
    );

    // Overlayable declarations must survive our flattener.
    let flattened = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
    let reparsed = parse_table(&flattened).unwrap();
    let names = |t: &ResourceTable| -> Vec<String> {
        let mut v: Vec<String> = t.overlayables.iter().map(|o| o.name.clone()).collect();
        v.sort();
        v
    };
    assert_eq!(names(&table), names(&reparsed));
}

// ───────────────────── broad compile coverage ─────────────────────

/// Compiles every res/ tree shipped with aapt2's integration tests —
/// fonts, navigation graphs, transitions, vectors, nine-patches, raw
/// assets, namespaced libraries, symlinked trees — and validates every
/// produced container.
#[test]
fn compile_all_integration_res_trees() {
    let Some(it) = aapt2_it() else { return };
    let res_dirs = [
        "BasicTest/res",
        "AutoVersionTest/res",
        "CompileTest/res",
        "CompileTest/DirInput/res",
        "MergeOnlyTest/App/res",
        "MergeOnlyTest/LocalLib/res",
        "NamespaceTest/App/res",
        "NamespaceTest/LibOne/res",
        "NamespaceTest/LibTwo/res",
        "NamespaceTest/Split/res",
        "StaticLibTest/App/res",
        "StaticLibTest/LibOne/res",
        "StaticLibTest/LibTwo/res",
        "SymlinkTest/res",
    ];
    let mut failures = Vec::new();
    for relative in res_dirs {
        let res_dir = it.join(relative);
        if !res_dir.is_dir() {
            continue;
        }
        let dir = temp_dir(&relative.replace('/', "-"));
        let out = dir.join("compiled.zip");
        let diag = Diagnostics::collecting();
        let mut options = CompileOptions::new();
        options.res_dir = Some(res_dir);
        options.legacy_mode = true; // CompileTest has period-bearing names
        match compile(&[], &out, &options, &diag) {
            Ok(()) => {
                // Every artifact must be a structurally valid container.
                match skotch_aapt2::compile::read_artifacts(&out) {
                    Ok(artifacts) => {
                        for (name, data) in &artifacts {
                            if let Err(e) = skotch_aapt2::container::read_container(data) {
                                failures.push(format!("{relative}/{name}: bad container: {e}"));
                            }
                        }
                        if artifacts.is_empty() {
                            failures.push(format!("{relative}: produced no artifacts"));
                        }
                    }
                    Err(e) => failures.push(format!("{relative}: unreadable output: {e}")),
                }
            }
            Err(e) => {
                let messages: Vec<String> = diag.take().iter().map(|d| d.to_string()).collect();
                failures.push(format!(
                    "{relative}: compile failed: {e}\n  {}",
                    messages.join("\n  ")
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
