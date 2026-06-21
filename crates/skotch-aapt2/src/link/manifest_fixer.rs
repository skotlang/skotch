//! Manifest normalization and attribute injection.
//!
//! Port of `link/ManifestFixer.{h,cpp}` (the structural validation
//! subset is being extended; injection and app-info extraction are
//! complete).

use crate::diag::Diagnostics;
use crate::res::utils::parse_sdk_version;
use crate::xml::{Element, NamespaceDecl, Node, SCHEMA_ANDROID};
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeSet;

/// Options mirroring `ManifestFixerOptions`.
#[derive(Debug, Clone, Default)]
pub struct ManifestFixerOptions {
    /// `--min-sdk-version`.
    pub min_sdk_version_default: Option<String>,
    /// `--target-sdk-version`.
    pub target_sdk_version_default: Option<String>,
    /// `--version-code`.
    pub version_code_default: Option<String>,
    /// `--version-code-major`.
    pub version_code_major_default: Option<String>,
    /// `--version-name`.
    pub version_name_default: Option<String>,
    /// `--revision-code`.
    pub revision_code_default: Option<String>,
    /// `--replace-version`.
    pub replace_version: bool,
    /// `--compile-sdk-version-code` (also sets platformBuildVersionCode).
    pub compile_sdk_version: Option<String>,
    /// `--compile-sdk-version-name` (also sets platformBuildVersionName).
    pub compile_sdk_version_codename: Option<String>,
    /// `--no-compile-sdk-metadata`.
    pub no_compile_sdk_metadata: bool,
    /// `--rename-manifest-package`.
    pub rename_manifest_package: Option<String>,
    /// `--rename-instrumentation-target-package`.
    pub rename_instrumentation_target_package: Option<String>,
    /// `--rename-overlay-target-package`.
    pub rename_overlay_target_package: Option<String>,
    /// `--rename-overlay-category`.
    pub rename_overlay_category: Option<String>,
    /// `--debug-mode`.
    pub debug_mode: bool,
    /// `--warn-manifest-validation`.
    pub warn_validation: bool,
    /// `--non-updatable-system`.
    pub non_updatable_system: bool,
    /// `--fingerprint-prefix` values.
    pub fingerprint_prefixes: Vec<String>,
}

/// Information extracted from the manifest. Mirrors `AppInfo`.
#[derive(Debug, Clone, Default)]
pub struct AppInfo {
    pub package: String,
    pub min_sdk_version: Option<i32>,
    pub version_code: Option<u32>,
    pub version_code_major: Option<u32>,
    pub revision_code: Option<u32>,
    pub split_name: Option<String>,
    pub split_name_dependencies: BTreeSet<String>,
}

/// Extracts package/version/sdk info. Mirrors
/// `ExtractAppInfoFromBinaryManifest` for source manifests.
pub fn extract_app_info(manifest: &Element) -> Result<AppInfo> {
    if manifest.name != "manifest" || !manifest.namespace_uri.is_empty() {
        bail!("root tag of AndroidManifest.xml must be <manifest>");
    }
    let package = manifest
        .attr_value("", "package")
        .ok_or_else(|| anyhow!("<manifest> must have a 'package' attribute"))?
        .to_string();

    let mut info = AppInfo {
        package,
        ..Default::default()
    };

    if let Some(value) = manifest.attr_value(SCHEMA_ANDROID, "versionCode") {
        info.version_code = value.parse().ok();
    }
    if let Some(value) = manifest.attr_value(SCHEMA_ANDROID, "versionCodeMajor") {
        info.version_code_major = value.parse().ok();
    }
    if let Some(value) = manifest.attr_value(SCHEMA_ANDROID, "revisionCode") {
        info.revision_code = value.parse().ok();
    }
    if let Some(value) = manifest.attr_value("", "split") {
        info.split_name = Some(value.to_string());
    }
    if let Some(uses_sdk) = manifest.find_child("", "uses-sdk") {
        if let Some(value) = uses_sdk.attr_value(SCHEMA_ANDROID, "minSdkVersion") {
            info.min_sdk_version = parse_sdk_version(value);
        }
    }
    if let Some(application) = manifest.find_child("", "application") {
        for uses_split in application.child_elements() {
            if uses_split.name == "uses-split" {
                if let Some(name) = uses_split.attr_value(SCHEMA_ANDROID, "name") {
                    info.split_name_dependencies.insert(name.to_string());
                }
            }
        }
    }
    Ok(info)
}

fn ensure_android_namespace(manifest: &mut Element) {
    let declared = manifest
        .namespace_decls
        .iter()
        .any(|decl| decl.uri == SCHEMA_ANDROID);
    if !declared {
        manifest.namespace_decls.push(NamespaceDecl {
            prefix: "android".to_string(),
            uri: SCHEMA_ANDROID.to_string(),
            line_number: manifest.line_number,
            column_number: 0,
        });
    }
}

/// Applies all fixes/injections to the manifest in place. Mirrors
/// `ManifestFixer::Consume`.
pub fn fix_manifest(
    manifest: &mut Element,
    options: &ManifestFixerOptions,
    diag: &Diagnostics,
) -> Result<()> {
    if manifest.name != "manifest" || !manifest.namespace_uri.is_empty() {
        bail!("root tag of AndroidManifest.xml must be <manifest>");
    }
    if manifest.attr_value("", "package").is_none() {
        bail!("<manifest> must have a 'package' attribute");
    }
    ensure_android_namespace(manifest);

    // ── uses-sdk injection ────────────────────────────────────────
    if options.min_sdk_version_default.is_some() || options.target_sdk_version_default.is_some() {
        if manifest.find_child("", "uses-sdk").is_none() {
            // Insert <uses-sdk> as the first child, like ManifestFixer.
            manifest
                .children
                .insert(0, Node::Element(Element::new("uses-sdk")));
        }
        let uses_sdk = manifest.find_child_mut("", "uses-sdk").unwrap();
        if let Some(min_sdk) = &options.min_sdk_version_default {
            if uses_sdk
                .attr_value(SCHEMA_ANDROID, "minSdkVersion")
                .is_none()
            {
                uses_sdk.set_attribute(SCHEMA_ANDROID, "minSdkVersion", min_sdk);
            }
        }
        if let Some(target_sdk) = &options.target_sdk_version_default {
            if uses_sdk
                .attr_value(SCHEMA_ANDROID, "targetSdkVersion")
                .is_none()
            {
                uses_sdk.set_attribute(SCHEMA_ANDROID, "targetSdkVersion", target_sdk);
            }
        }
    }

    // ── version injection ─────────────────────────────────────────
    let set_or_replace = |manifest: &mut Element, name: &str, value: &Option<String>| {
        if let Some(value) = value {
            let exists = manifest.attr_value(SCHEMA_ANDROID, name).is_some();
            if options.replace_version || !exists {
                manifest.set_attribute(SCHEMA_ANDROID, name, value);
            }
        }
    };
    set_or_replace(manifest, "versionCode", &options.version_code_default);
    set_or_replace(
        manifest,
        "versionCodeMajor",
        &options.version_code_major_default,
    );
    set_or_replace(manifest, "versionName", &options.version_name_default);
    set_or_replace(manifest, "revisionCode", &options.revision_code_default);

    // ── compile SDK metadata ──────────────────────────────────────
    if !options.no_compile_sdk_metadata {
        if let Some(code) = &options.compile_sdk_version {
            manifest.set_attribute(SCHEMA_ANDROID, "compileSdkVersion", code);
            manifest.set_attribute("", "platformBuildVersionCode", code);
        }
        if let Some(name) = &options.compile_sdk_version_codename {
            manifest.set_attribute(SCHEMA_ANDROID, "compileSdkVersionCodename", name);
            manifest.set_attribute("", "platformBuildVersionName", name);
        }
    }

    // ── package renames ───────────────────────────────────────────
    if let Some(new_package) = &options.rename_manifest_package {
        rename_manifest_package(manifest, new_package);
    }
    if let Some(target) = &options.rename_instrumentation_target_package {
        for child in manifest.child_elements_mut() {
            if child.name == "instrumentation" && child.namespace_uri.is_empty() {
                if child
                    .find_attribute(SCHEMA_ANDROID, "targetPackage")
                    .is_some()
                {
                    child.set_attribute(SCHEMA_ANDROID, "targetPackage", target);
                }
            }
        }
    }
    if let Some(target) = &options.rename_overlay_target_package {
        for child in manifest.child_elements_mut() {
            if child.name == "overlay" && child.namespace_uri.is_empty() {
                child.set_attribute(SCHEMA_ANDROID, "targetPackage", target);
            }
        }
    }
    if let Some(category) = &options.rename_overlay_category {
        for child in manifest.child_elements_mut() {
            if child.name == "overlay" && child.namespace_uri.is_empty() {
                child.set_attribute(SCHEMA_ANDROID, "category", category);
            }
        }
    }

    // ── debuggable / non-updatable ────────────────────────────────
    if options.debug_mode {
        if let Some(application) = manifest.find_child_mut("", "application") {
            application.set_attribute(SCHEMA_ANDROID, "debuggable", "true");
        }
    }
    if options.non_updatable_system {
        if manifest.attr_value(SCHEMA_ANDROID, "versionCode").is_none() {
            manifest.set_attribute("", "updatableSystem", "false");
        } else {
            diag.note("Ignoring --non-updatable-system because the manifest has a versionCode");
        }
    }

    // ── install constraints (--fingerprint-prefix) ────────────────
    if !options.fingerprint_prefixes.is_empty() {
        let mut constraints = Element::new("install-constraints");
        for prefix in &options.fingerprint_prefixes {
            let mut fingerprint = Element::new("fingerprint-prefix");
            fingerprint.set_attribute(SCHEMA_ANDROID, "value", prefix);
            constraints.children.push(Node::Element(fingerprint));
        }
        manifest.children.insert(0, Node::Element(constraints));
    }

    // Basic structural validation, mirroring the most important
    // ManifestFixer checks.
    let mut seen_application = false;
    for child in manifest.child_elements() {
        if child.namespace_uri.is_empty() && child.name == "application" {
            if seen_application {
                let message = "multiple <application> tags found";
                if options.warn_validation {
                    diag.warn(message.to_string());
                } else {
                    bail!("{message}");
                }
            }
            seen_application = true;
        }
    }

    Ok(())
}

/// Renames the manifest package, fixing up unqualified component names
/// (`.MyActivity` → `original.package.MyActivity`). Mirrors
/// `FullyQualifyClassName` usage in ManifestFixer.
fn rename_manifest_package(manifest: &mut Element, new_package: &str) {
    let original = manifest.attr_value("", "package").unwrap_or("").to_string();
    manifest.set_attribute("", "package", new_package);

    let class_attrs = ["name", "targetActivity", "parentActivityName"];
    fn fix_components(element: &mut Element, original: &str, class_attrs: &[&str]) {
        for attr in &mut element.attributes {
            if attr.namespace_uri == SCHEMA_ANDROID && class_attrs.contains(&attr.name.as_str()) {
                if let Some(qualified) = fully_qualify_class_name(original, &attr.value) {
                    attr.value = qualified;
                }
            }
        }
        for child in element.child_elements_mut() {
            fix_components(child, original, class_attrs);
        }
    }
    if let Some(application) = manifest.find_child_mut("", "application") {
        fix_components(application, &original, &class_attrs);
    }
}

/// `.Foo` → `package.Foo`; `Foo` (no dot) → `package.Foo`; already
/// qualified names pass through. Mirrors `util::FullyQualifyClassName`.
pub fn fully_qualify_class_name(package: &str, class_name: &str) -> Option<String> {
    if class_name.is_empty() {
        return None;
    }
    if let Some(rest) = class_name.strip_prefix('.') {
        return Some(format!("{package}.{rest}"));
    }
    if !class_name.contains('.') {
        return Some(format!("{package}.{class_name}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> Element {
        crate::xml::parse_source_xml("AndroidManifest.xml", xml)
            .unwrap()
            .root
            .unwrap()
    }

    const BASE: &str = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android"
        package="com.app"><application/></manifest>"#;

    #[test]
    fn extracts_app_info() {
        let manifest = parse(
            r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android"
                package="com.app" android:versionCode="42">
                 <uses-sdk android:minSdkVersion="21"/>
               </manifest>"#,
        );
        let info = extract_app_info(&manifest).unwrap();
        assert_eq!(info.package, "com.app");
        assert_eq!(info.version_code, Some(42));
        assert_eq!(info.min_sdk_version, Some(21));
    }

    #[test]
    fn injects_uses_sdk_and_versions() {
        let mut manifest = parse(BASE);
        let options = ManifestFixerOptions {
            min_sdk_version_default: Some("21".to_string()),
            target_sdk_version_default: Some("33".to_string()),
            version_code_default: Some("7".to_string()),
            version_name_default: Some("1.2".to_string()),
            ..Default::default()
        };
        let diag = Diagnostics::collecting();
        fix_manifest(&mut manifest, &options, &diag).unwrap();
        let uses_sdk = manifest.find_child("", "uses-sdk").unwrap();
        assert_eq!(
            uses_sdk.attr_value(SCHEMA_ANDROID, "minSdkVersion"),
            Some("21")
        );
        assert_eq!(
            uses_sdk.attr_value(SCHEMA_ANDROID, "targetSdkVersion"),
            Some("33")
        );
        assert_eq!(
            manifest.attr_value(SCHEMA_ANDROID, "versionCode"),
            Some("7")
        );
        assert_eq!(
            manifest.attr_value(SCHEMA_ANDROID, "versionName"),
            Some("1.2")
        );
    }

    #[test]
    fn existing_versions_kept_without_replace() {
        let mut manifest = parse(
            r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android"
                package="com.app" android:versionCode="10"/>"#,
        );
        let mut options = ManifestFixerOptions {
            version_code_default: Some("7".to_string()),
            ..Default::default()
        };
        let diag = Diagnostics::collecting();
        fix_manifest(&mut manifest, &options, &diag).unwrap();
        assert_eq!(
            manifest.attr_value(SCHEMA_ANDROID, "versionCode"),
            Some("10")
        );

        options.replace_version = true;
        fix_manifest(&mut manifest, &options, &diag).unwrap();
        assert_eq!(
            manifest.attr_value(SCHEMA_ANDROID, "versionCode"),
            Some("7")
        );
    }

    #[test]
    fn qualify_class_names() {
        assert_eq!(
            fully_qualify_class_name("com.app", ".Main"),
            Some("com.app.Main".to_string())
        );
        assert_eq!(
            fully_qualify_class_name("com.app", "Main"),
            Some("com.app.Main".to_string())
        );
        assert_eq!(fully_qualify_class_name("com.app", "other.pkg.Main"), None);
    }
}
