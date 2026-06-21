//! `aapt2 dump badging` / `dump permissions`.
//!
//! Faithful port of `dump/DumpManifest.cpp`: the `ManifestExtractor`,
//! its per-tag element extractors, resource resolution against the
//! APK's own table with config matching, implied permissions/features,
//! component discovery, and the exact print formatting validated by the
//! `DumpTest` goldens.

use super::printer::Printer;
use crate::apk::{ApkFormat, LoadedApk};
use crate::diag::Diagnostics;
use crate::res::config::ConfigDescription;
use crate::res::table::{ResourceEntry, ResourceTable};
use crate::res::value::{res_value_type, Item, Reference, Value, ValueKind, DATA_NULL_EMPTY};
use crate::xml::{Element as XmlElement, XmlAttribute};
use std::collections::{BTreeMap, BTreeSet};

// Attribute resource constants for the platform (android.R.attr).
const LABEL_ATTR: u32 = 0x01010001;
const ICON_ATTR: u32 = 0x01010002;
const NAME_ATTR: u32 = 0x01010003;
const PERMISSION_ATTR: u32 = 0x01010006;
const EXPORTED_ATTR: u32 = 0x01010010;
const GRANT_URI_PERMISSIONS_ATTR: u32 = 0x0101001b;
const PRIORITY_ATTR: u32 = 0x0101001c;
const RESOURCE_ATTR: u32 = 0x01010025;
const DEBUGGABLE_ATTR: u32 = 0x0101000f;
const TARGET_PACKAGE_ATTR: u32 = 0x01010021;
const VALUE_ATTR: u32 = 0x01010024;
const VERSION_CODE_ATTR: u32 = 0x0101021b;
const VERSION_NAME_ATTR: u32 = 0x0101021c;
const SCREEN_ORIENTATION_ATTR: u32 = 0x0101001e;
const MIN_SDK_VERSION_ATTR: u32 = 0x0101020c;
const MAX_SDK_VERSION_ATTR: u32 = 0x01010271;
const REQ_TOUCH_SCREEN_ATTR: u32 = 0x01010227;
const REQ_KEYBOARD_TYPE_ATTR: u32 = 0x01010228;
const REQ_HARD_KEYBOARD_ATTR: u32 = 0x01010229;
const REQ_NAVIGATION_ATTR: u32 = 0x0101022a;
const REQ_FIVE_WAY_NAV_ATTR: u32 = 0x01010232;
const TARGET_SDK_VERSION_ATTR: u32 = 0x01010270;
const TEST_ONLY_ATTR: u32 = 0x01010272;
const ANY_DENSITY_ATTR: u32 = 0x0101026c;
const GL_ES_VERSION_ATTR: u32 = 0x01010281;
const SMALL_SCREEN_ATTR: u32 = 0x01010284;
const NORMAL_SCREEN_ATTR: u32 = 0x01010285;
const LARGE_SCREEN_ATTR: u32 = 0x01010286;
const XLARGE_SCREEN_ATTR: u32 = 0x010102bf;
const REQUIRED_ATTR: u32 = 0x0101028e;
const INSTALL_LOCATION_ATTR: u32 = 0x010102b7;
const SCREEN_SIZE_ATTR: u32 = 0x010102ca;
const SCREEN_DENSITY_ATTR: u32 = 0x010102cb;
const REQUIRES_SMALLEST_WIDTH_DP_ATTR: u32 = 0x01010364;
const COMPATIBLE_WIDTH_LIMIT_DP_ATTR: u32 = 0x01010365;
const LARGEST_WIDTH_LIMIT_DP_ATTR: u32 = 0x01010366;
const PUBLIC_KEY_ATTR: u32 = 0x010103a6;
const CATEGORY_ATTR: u32 = 0x010103e8;
const BANNER_ATTR: u32 = 0x10103f2;
const ISGAME_ATTR: u32 = 0x10103f4;
const VERSION_ATTR: u32 = 0x01010519;
const CERT_DIGEST_ATTR: u32 = 0x01010548;
const REQUIRED_FEATURE_ATTR: u32 = 0x01010554;
const REQUIRED_NOT_FEATURE_ATTR: u32 = 0x01010555;
const IS_STATIC_ATTR: u32 = 0x0101055a;
const REQUIRED_SYSTEM_PROPERTY_NAME_ATTR: u32 = 0x01010565;
const REQUIRED_SYSTEM_PROPERTY_VALUE_ATTR: u32 = 0x01010566;
const COMPILE_SDK_VERSION_ATTR: u32 = 0x01010572;
const COMPILE_SDK_VERSION_CODENAME_ATTR: u32 = 0x01010573;
const VERSION_MAJOR_ATTR: u32 = 0x01010577;
const PACKAGE_TYPE_ATTR: u32 = 0x01010587;
const USES_PERMISSION_FLAGS_ATTR: u32 = 0x01010644;

const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";
const NEVER_FOR_LOCATION: i32 = 0x00010000;

// SDK level constants (SdkConstants.h).
const SDK_DONUT: i32 = 4;
const SDK_GINGERBREAD: i32 = 9;
const SDK_JELLY_BEAN: i32 = 16;
const SDK_LOLLIPOP: i32 = 21;
const SDK_CUR_DEVELOPMENT: i32 = 10000;

#[derive(Debug, Clone, Copy, Default)]
pub struct DumpManifestOptions {
    pub include_meta_data: bool,
    pub only_permissions: bool,
}

/// Port of `android::ResTable::normalizeForOutput`.
fn normalize_for_output(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

/// Creates the default configuration used to retrieve resources
/// (`ManifestExtractor::DefaultConfig`).
fn default_config() -> ConfigDescription {
    let mut config = ConfigDescription::default();
    config.orientation = ConfigDescription::ORIENTATION_PORT;
    config.density = ConfigDescription::DENSITY_MEDIUM;
    config.sdk_version = SDK_CUR_DEVELOPMENT as u16; // Very high.
    config.screen_width_dp = 320;
    config.screen_height_dp = 480;
    config.smallest_screen_width_dp = 320;
    config.screen_layout |= ConfigDescription::SCREENSIZE_NORMAL;
    config
}

// ───────────────────── attribute lookup + resolution ─────────────────────

/// `FindAttribute(el, resource_id)`.
fn find_attr_by_id<'e>(el: &'e XmlElement, res_id: u32) -> Option<&'e XmlAttribute> {
    el.attributes.iter().find(|a| {
        a.compiled_attribute
            .as_ref()
            .and_then(|c| c.id)
            .is_some_and(|id| id.0 == res_id)
    })
}

/// `FindAttribute(el, package, name)`.
fn find_attr<'e>(el: &'e XmlElement, ns: &str, name: &str) -> Option<&'e XmlAttribute> {
    el.find_attribute(ns, name)
}

/// Port of `ManifestExtractor::Element::BestConfigValue`.
fn best_config_value<'t>(
    entry: &'t ResourceEntry,
    wanted: &ConfigDescription,
) -> Option<&'t Value> {
    let mut best: Option<&crate::res::table::ResourceConfigValue> = None;
    for value in &entry.values {
        if !value.config.matches(wanted) {
            continue;
        }
        if let Some(best_value) = best {
            if !value
                .config
                .is_better_than(&best_value.config, Some(wanted))
                && value.config.compare(&best_value.config) != std::cmp::Ordering::Equal
            {
                continue;
            }
        }
        best = Some(value);
    }
    best.and_then(|b| b.value.as_ref())
}

/// Port of `FindValueById`.
fn find_value_by_id<'t>(
    table: &'t ResourceTable,
    res_id: u32,
    config: &ConfigDescription,
) -> Option<&'t Value> {
    for package in &table.packages {
        for ty in &package.types {
            for entry in &ty.entries {
                if entry.id.is_some_and(|id| id.0 == res_id) {
                    if let Some(value) = best_config_value(entry, config) {
                        return Some(value);
                    }
                }
            }
        }
    }
    None
}

/// Port of `ResolveReference` (max 40 hops).
fn resolve_reference<'t>(
    table: &'t ResourceTable,
    reference: &Reference,
    config: &ConfigDescription,
) -> Option<&'t Value> {
    let mut current = reference.id;
    let mut iterations = 0;
    while let Some(id) = current {
        if iterations >= 40 {
            break;
        }
        iterations += 1;
        match find_value_by_id(table, id.0, config) {
            Some(value) => match &value.kind {
                ValueKind::Item(Item::Reference(next)) => current = next.id,
                _ => return Some(value),
            },
            None => return None,
        }
    }
    None
}

/// True when the compiled value is the undefined-null marker, which the
/// C++ DOM represents as an empty `Reference` (so it never resolves).
fn is_undefined_null(item: &Item) -> bool {
    matches!(item, Item::BinaryPrimitive(rv)
        if rv.data_type == res_value_type::TYPE_NULL && rv.data != DATA_NULL_EMPTY)
}

fn item_string(item: &Item) -> Option<String> {
    match item {
        Item::String { value, .. } => Some(value.clone()),
        Item::RawString(value) => Some(value.clone()),
        Item::StyledString { value, .. } => Some(value.clone()),
        Item::FileReference(f) => Some(f.path.clone()),
        _ => None,
    }
}

/// Port of `GetAttributeString` (resolving references against the
/// APK's resource table for the given config).
fn get_attr_string(
    apk: &LoadedApk,
    attr: Option<&XmlAttribute>,
    config: &ConfigDescription,
) -> Option<String> {
    let attr = attr?;
    if let Some(compiled) = &attr.compiled_value {
        let resolved: Option<String> = match compiled {
            Item::Reference(reference) => resolve_reference(&apk.table, reference, config)
                .and_then(|v| v.as_item())
                .and_then(item_string),
            item if is_undefined_null(item) => None,
            item => item_string(item),
        };
        if let Some(s) = resolved {
            return Some(s);
        }
    }
    if !attr.value.is_empty() {
        return Some(attr.value.clone());
    }
    None
}

fn get_attr_string_default(
    apk: &LoadedApk,
    attr: Option<&XmlAttribute>,
    def: &str,
    config: &ConfigDescription,
) -> String {
    get_attr_string(apk, attr, config).unwrap_or_else(|| def.to_string())
}

/// Port of `GetAttributeInteger`.
fn get_attr_integer(
    apk: &LoadedApk,
    attr: Option<&XmlAttribute>,
    config: &ConfigDescription,
) -> Option<i32> {
    let attr = attr?;
    let compiled = attr.compiled_value.as_ref()?;
    let item: &Item = match compiled {
        Item::Reference(reference) => {
            match resolve_reference(&apk.table, reference, config).and_then(|v| v.as_item()) {
                Some(item) => item,
                None => return None,
            }
        }
        item if is_undefined_null(item) => return None,
        item => item,
    };
    match item {
        Item::BinaryPrimitive(rv) => Some(rv.data as i32),
        _ => None,
    }
}

fn get_attr_integer_default(
    apk: &LoadedApk,
    attr: Option<&XmlAttribute>,
    def: i32,
    config: &ConfigDescription,
) -> i32 {
    get_attr_integer(apk, attr, config).unwrap_or(def)
}

// ───────────────────────── feature groups ─────────────────────────

#[derive(Debug, Clone, Copy)]
struct Feature {
    required: bool,
    version: i32,
}

#[derive(Debug, Clone, Default)]
struct FeatureGroupData {
    label: String,
    open_gles_version: i32,
    features: BTreeMap<String, Feature>,
}

impl FeatureGroupData {
    /// Port of `FeatureGroup::AddFeature` (with prerequisite expansion).
    fn add_feature(&mut self, name: &str, required: bool, version: i32) {
        self.features
            .insert(name.to_string(), Feature { required, version });
        if required {
            match name {
                "android.hardware.camera.autofocus" | "android.hardware.camera.flash" => {
                    self.add_feature("android.hardware.camera", true, -1);
                }
                "android.hardware.location.gps" | "android.hardware.location.network" => {
                    self.add_feature("android.hardware.location", true, -1);
                }
                "android.hardware.faketouch.multitouch" => {
                    self.add_feature("android.hardware.faketouch", true, -1);
                }
                "android.hardware.faketouch.multitouch.distinct"
                | "android.hardware.faketouch.multitouch.jazzhands" => {
                    self.add_feature("android.hardware.faketouch.multitouch", true, -1);
                    self.add_feature("android.hardware.faketouch", true, -1);
                }
                "android.hardware.touchscreen.multitouch" => {
                    self.add_feature("android.hardware.touchscreen", true, -1);
                }
                "android.hardware.touchscreen.multitouch.distinct"
                | "android.hardware.touchscreen.multitouch.jazzhands" => {
                    self.add_feature("android.hardware.touchscreen.multitouch", true, -1);
                    self.add_feature("android.hardware.touchscreen", true, -1);
                }
                "android.hardware.opengles.aep" => {
                    const OPENGL_ES_VERSION_31: i32 = 0x00030001;
                    if OPENGL_ES_VERSION_31 > self.open_gles_version {
                        self.open_gles_version = OPENGL_ES_VERSION_31;
                    }
                }
                _ => {}
            }
        }
    }

    /// Port of `FeatureGroup::Merge` (existing features win).
    fn merge(&mut self, other: &FeatureGroupData) {
        self.open_gles_version = self.open_gles_version.max(other.open_gles_version);
        for (name, feature) in &other.features {
            self.features.entry(name.clone()).or_insert(*feature);
        }
    }

    /// Port of `FeatureGroup::PrintGroup`.
    fn print_group(&self, printer: &mut Printer) {
        printer.print(format!("feature-group: label='{}'\n", self.label));
        if self.open_gles_version > 0 {
            printer.print(format!("  uses-gl-es: '0x{:x}'\n", self.open_gles_version));
        }
        for (name, feature) in &self.features {
            printer.print(format!(
                "  uses-feature{}: name='{}'",
                if feature.required {
                    ""
                } else {
                    "-not-required"
                },
                name
            ));
            if feature.version > 0 {
                printer.print(format!(" version='{}'", feature.version));
            }
            printer.print("\n");
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ImpliedFeature {
    reasons: BTreeSet<String>,
    implied_from_sdk_23: bool,
}

#[derive(Debug, Clone, Default)]
struct CommonFeatureGroup {
    group: FeatureGroupData,
    implied: BTreeMap<String, ImpliedFeature>,
}

impl CommonFeatureGroup {
    fn has_feature(&self, name: &str) -> bool {
        self.group.features.contains_key(name) || self.implied.contains_key(name)
    }

    fn add_implied_feature(&mut self, name: &str, reason: &str, sdk23: bool) {
        let entry = self
            .implied
            .entry(name.to_string())
            .or_insert_with(|| ImpliedFeature {
                implied_from_sdk_23: sdk23,
                ..Default::default()
            });
        // A non-sdk-23 implied feature takes precedence.
        if entry.implied_from_sdk_23 && !sdk23 {
            entry.implied_from_sdk_23 = false;
        }
        entry.reasons.insert(reason.to_string());
    }

    /// Port of `CommonFeatureGroup::addImpliedFeaturesForPermission`.
    fn add_implied_features_for_permission(&mut self, target_sdk: i32, name: &str, sdk23: bool) {
        match name {
            "android.permission.CAMERA" => {
                self.add_implied_feature(
                    "android.hardware.camera",
                    &format!("requested {name} permission"),
                    sdk23,
                );
            }
            "android.permission.ACCESS_FINE_LOCATION" => {
                if target_sdk < SDK_LOLLIPOP {
                    self.add_implied_feature(
                        "android.hardware.location.gps",
                        &format!("requested {name} permission"),
                        sdk23,
                    );
                    self.add_implied_feature(
                        "android.hardware.location.gps",
                        &format!("targetSdkVersion < {SDK_LOLLIPOP}"),
                        sdk23,
                    );
                }
                self.add_implied_feature(
                    "android.hardware.location",
                    &format!("requested {name} permission"),
                    sdk23,
                );
            }
            "android.permission.ACCESS_COARSE_LOCATION" => {
                if target_sdk < SDK_LOLLIPOP {
                    self.add_implied_feature(
                        "android.hardware.location.network",
                        &format!("requested {name} permission"),
                        sdk23,
                    );
                    self.add_implied_feature(
                        "android.hardware.location.network",
                        &format!("targetSdkVersion < {SDK_LOLLIPOP}"),
                        sdk23,
                    );
                }
                self.add_implied_feature(
                    "android.hardware.location",
                    &format!("requested {name} permission"),
                    sdk23,
                );
            }
            "android.permission.ACCESS_MOCK_LOCATION"
            | "android.permission.ACCESS_LOCATION_EXTRA_COMMANDS"
            | "android.permission.INSTALL_LOCATION_PROVIDER" => {
                self.add_implied_feature(
                    "android.hardware.location",
                    &format!("requested {name} permission"),
                    sdk23,
                );
            }
            "android.permission.BLUETOOTH" | "android.permission.BLUETOOTH_ADMIN" => {
                if target_sdk > SDK_DONUT {
                    self.add_implied_feature(
                        "android.hardware.bluetooth",
                        &format!("requested {name} permission"),
                        sdk23,
                    );
                    self.add_implied_feature(
                        "android.hardware.bluetooth",
                        &format!("targetSdkVersion > {SDK_DONUT}"),
                        sdk23,
                    );
                }
            }
            "android.permission.RECORD_AUDIO" => {
                self.add_implied_feature(
                    "android.hardware.microphone",
                    &format!("requested {name} permission"),
                    sdk23,
                );
            }
            "android.permission.ACCESS_WIFI_STATE"
            | "android.permission.CHANGE_WIFI_STATE"
            | "android.permission.CHANGE_WIFI_MULTICAST_STATE" => {
                self.add_implied_feature(
                    "android.hardware.wifi",
                    &format!("requested {name} permission"),
                    sdk23,
                );
            }
            "android.permission.CALL_PHONE"
            | "android.permission.CALL_PRIVILEGED"
            | "android.permission.MODIFY_PHONE_STATE"
            | "android.permission.PROCESS_OUTGOING_CALLS"
            | "android.permission.READ_SMS"
            | "android.permission.RECEIVE_SMS"
            | "android.permission.RECEIVE_MMS"
            | "android.permission.RECEIVE_WAP_PUSH"
            | "android.permission.SEND_SMS"
            | "android.permission.WRITE_APN_SETTINGS"
            | "android.permission.WRITE_SMS" => {
                self.add_implied_feature(
                    "android.hardware.telephony",
                    "requested a telephony permission",
                    sdk23,
                );
            }
            _ => {}
        }
    }

    /// Port of `CommonFeatureGroup::PrintGroup`.
    fn print_group(&self, printer: &mut Printer) {
        self.group.print_group(printer);
        for (name, feature) in &self.implied {
            if self.group.features.contains_key(name) {
                continue;
            }
            let sdk23 = if feature.implied_from_sdk_23 {
                "-sdk-23"
            } else {
                ""
            };
            printer.print(format!("  uses-feature{sdk23}: name='{name}'\n"));
            printer.print(format!(
                "  uses-implied-feature{sdk23}: name='{name}' reason='"
            ));
            let total = feature.reasons.len();
            for (count, reason) in feature.reasons.iter().enumerate() {
                printer.print(reason);
                if count + 2 < total {
                    printer.print(", ");
                } else if count + 1 < total {
                    printer.print(", and ");
                }
            }
            printer.print("'\n");
        }
    }
}

// ───────────────────────── element data ─────────────────────────

#[derive(Debug, Default)]
struct ManifestData {
    only_package_name: bool,
    package: String,
    version_code: i32,
    version_name: String,
    split: Option<String>,
    platform_version_name: Option<String>,
    platform_version_code: Option<String>,
    platform_version_name_int: Option<i32>,
    platform_version_code_int: Option<i32>,
    compilesdk_version: Option<i32>,
    compilesdk_version_codename: Option<String>,
    install_location: Option<i32>,
}

#[derive(Debug, Default)]
struct ApplicationData {
    label: String,
    icon: String,
    banner: String,
    is_game: i32,
    debuggable: i32,
    test_only: i32,
    has_multi_arch: bool,
    locale_labels: BTreeMap<String, String>,
    density_icons: BTreeMap<u16, String>,
}

#[derive(Debug, Default)]
struct UsesSdkData {
    min_sdk: Option<i32>,
    min_sdk_name: Option<String>,
    max_sdk: Option<i32>,
    target_sdk: Option<i32>,
    target_sdk_name: Option<String>,
}

#[derive(Debug, Default)]
struct UsesConfigurationData {
    req_touch_screen: i32,
    req_keyboard_type: i32,
    req_hard_keyboard: i32,
    req_navigation: i32,
    req_five_way_nav: i32,
}

#[derive(Debug, Clone)]
struct SupportsScreenData {
    small_screen: i32,
    normal_screen: i32,
    large_screen: i32,
    xlarge_screen: i32,
    any_density: i32,
    requires_smallest_width_dp: i32,
    compatible_width_limit_dp: i32,
    largest_width_limit_dp: i32,
}

impl Default for SupportsScreenData {
    fn default() -> Self {
        SupportsScreenData {
            small_screen: 1,
            normal_screen: 1,
            large_screen: 1,
            xlarge_screen: 1,
            any_density: 1,
            requires_smallest_width_dp: 0,
            compatible_width_limit_dp: 0,
            largest_width_limit_dp: 0,
        }
    }
}

impl SupportsScreenData {
    fn is_small_screen_supported(&self, target_sdk: i32) -> bool {
        if self.small_screen > 0 {
            return target_sdk >= SDK_DONUT;
        }
        self.small_screen != 0
    }

    fn is_large_screen_supported(&self, target_sdk: i32) -> bool {
        if self.large_screen > 0 {
            return target_sdk >= SDK_DONUT;
        }
        self.large_screen != 0
    }

    fn is_xlarge_screen_supported(&self, target_sdk: i32) -> bool {
        if self.xlarge_screen > 0 {
            return target_sdk >= SDK_GINGERBREAD;
        }
        self.xlarge_screen != 0
    }

    fn is_any_density_supported(&self, target_sdk: i32) -> bool {
        if self.any_density > 0 {
            return target_sdk >= SDK_DONUT
                || self.requires_smallest_width_dp > 0
                || self.compatible_width_limit_dp > 0;
        }
        self.any_density != 0
    }

    fn print_screens(&self, printer: &mut Printer, target_sdk: i32) {
        printer.print("supports-screens:");
        if self.is_small_screen_supported(target_sdk) {
            printer.print(" 'small'");
        }
        if self.normal_screen != 0 {
            printer.print(" 'normal'");
        }
        if self.is_large_screen_supported(target_sdk) {
            printer.print(" 'large'");
        }
        if self.is_xlarge_screen_supported(target_sdk) {
            printer.print(" 'xlarge'");
        }
        printer.print("\n");
        printer.print(format!(
            "supports-any-density: '{}'\n",
            if self.is_any_density_supported(target_sdk) {
                "true"
            } else {
                "false"
            }
        ));
        if self.requires_smallest_width_dp > 0 {
            printer.print(format!(
                "requires-smallest-width:'{}'\n",
                self.requires_smallest_width_dp
            ));
        }
        if self.compatible_width_limit_dp > 0 {
            printer.print(format!(
                "compatible-width-limit:'{}'\n",
                self.compatible_width_limit_dp
            ));
        }
        if self.largest_width_limit_dp > 0 {
            printer.print(format!(
                "largest-width-limit:'{}'\n",
                self.largest_width_limit_dp
            ));
        }
    }
}

#[derive(Debug, Default)]
struct UsesPermissionData {
    implied: bool,
    name: String,
    required_features: Vec<String>,
    required_not_features: Vec<String>,
    required: i32,
    max_sdk_version: i32,
    uses_permission_flags: i32,
    implied_reason: String,
}

impl UsesPermissionData {
    /// Port of `UsesPermission::Print`.
    fn print(&self, printer: &mut Printer) {
        if !self.name.is_empty() {
            printer.print(format!("uses-permission: name='{}'", self.name));
            if self.max_sdk_version >= 0 {
                printer.print(format!(" maxSdkVersion='{}'", self.max_sdk_version));
            }
            if (self.uses_permission_flags & NEVER_FOR_LOCATION) != 0 {
                printer.print(" usesPermissionFlags='neverForLocation'");
            }
            printer.print("\n");
            for required_feature in &self.required_features {
                printer.print(format!("  required-feature='{required_feature}'\n"));
            }
            for required_not_feature in &self.required_not_features {
                printer.print(format!("  required-not-feature='{required_not_feature}'\n"));
            }
            if self.required == 0 {
                printer.print(format!("optional-permission: name='{}'", self.name));
                if self.max_sdk_version >= 0 {
                    printer.print(format!(" maxSdkVersion='{}'", self.max_sdk_version));
                }
                if (self.uses_permission_flags & NEVER_FOR_LOCATION) != 0 {
                    printer.print(" usesPermissionFlags='neverForLocation'");
                }
                printer.print("\n");
            }
        }
        if self.implied {
            printer.print(format!("uses-implied-permission: name='{}'", self.name));
            if self.max_sdk_version >= 0 {
                printer.print(format!(" maxSdkVersion='{}'", self.max_sdk_version));
            }
            if (self.uses_permission_flags & NEVER_FOR_LOCATION) != 0 {
                printer.print(" usesPermissionFlags='neverForLocation'");
            }
            printer.print(format!(" reason='{}'\n", self.implied_reason));
        }
    }
}

#[derive(Debug, Default)]
struct UsesPermissionSdk23Data {
    name: Option<String>,
    max_sdk_version: Option<i32>,
}

#[derive(Debug, Default)]
struct ActivityData {
    name: String,
    icon: String,
    label: String,
    banner: String,
    has_component: bool,
    has_launcher_category: bool,
    has_leanback_launcher_category: bool,
    has_main_action: bool,
}

#[derive(Debug, Default)]
struct MetaDataData {
    name: String,
    value: String,
    value_int: Option<i32>,
    resource: String,
    resource_int: Option<i32>,
}

#[derive(Debug, Default)]
struct OverlayData {
    target_package: Option<String>,
    priority: i32,
    is_static: bool,
    required_property_name: Option<String>,
    required_property_value: Option<String>,
}

#[derive(Debug, Default)]
struct UsesPackageData {
    package_type: Option<String>,
    name: Option<String>,
    version: i32,
    version_major: i32,
    cert_digests: Vec<String>,
}

#[derive(Debug, Default)]
struct PropertyData {
    name: String,
    value: String,
    value_int: Option<i32>,
    resource: String,
    resource_int: Option<i32>,
}

#[derive(Debug)]
enum Data {
    None,
    Manifest(ManifestData),
    Application(ApplicationData),
    UsesSdk(UsesSdkData),
    UsesConfiguration(UsesConfigurationData),
    SupportsScreen(SupportsScreenData),
    FeatureGroup(FeatureGroupData),
    UsesFeature,
    UsesPermission(UsesPermissionData),
    UsesPermissionSdk23(UsesPermissionSdk23Data),
    RequiredFeature,
    RequiredNotFeature,
    Permission {
        name: String,
    },
    Activity(ActivityData),
    IntentFilter,
    Category {
        component: String,
    },
    Action {
        component: String,
    },
    Provider {
        has_required_saf_attributes: bool,
    },
    Receiver {
        permission: Option<String>,
        has_component: bool,
    },
    Service {
        permission: Option<String>,
        has_component: bool,
    },
    UsesLibrary {
        name: String,
        required: i32,
    },
    StaticLibrary {
        name: String,
        version: i32,
        version_major: i32,
    },
    UsesStaticLibrary {
        name: String,
        version: i32,
        version_major: i32,
        cert_digests: Vec<String>,
    },
    SdkLibrary {
        name: String,
        version_major: i32,
    },
    UsesSdkLibrary {
        name: String,
        version_major: i32,
        cert_digests: Vec<String>,
    },
    UsesNativeLibrary {
        name: String,
        required: i32,
    },
    MetaData(MetaDataData),
    SupportsInput {
        inputs: Vec<String>,
    },
    InputType,
    InstallConstraints {
        fingerprint_prefixes: Vec<String>,
    },
    OriginalPackage {
        name: Option<String>,
    },
    Overlay(OverlayData),
    PackageVerifier {
        name: Option<String>,
        public_key: Option<String>,
    },
    UsesPackage(UsesPackageData),
    AdditionalCertificate,
    Screen {
        size: Option<i32>,
        density: Option<i32>,
    },
    CompatibleScreens,
    SupportsGlTexture {
        name: Option<String>,
    },
    Property(PropertyData),
}

struct El {
    data: Data,
    children: Vec<usize>,
}

// (child, parent) tag pairs that mark an element as "featured"
// (`kValidChildParentTags`).
const VALID_CHILD_PARENT_TAGS: &[(&str, &str)] = &[
    ("action", "intent-filter"),
    ("activity", "application"),
    ("additional-certificate", "uses-package"),
    ("additional-certificate", "uses-static-library"),
    ("application", "manifest"),
    ("category", "intent-filter"),
    ("compatible-screens", "manifest"),
    ("feature-group", "manifest"),
    ("input-type", "supports-input"),
    ("intent-filter", "activity"),
    ("intent-filter", "activity-alias"),
    ("intent-filter", "service"),
    ("intent-filter", "receiver"),
    ("intent-filter", "provider"),
    ("manifest", ""),
    ("meta-data", "activity"),
    ("meta-data", "activity-alias"),
    ("meta-data", "application"),
    ("meta-data", "service"),
    ("meta-data", "receiver"),
    ("meta-data", "provider"),
    ("original-package", "manifest"),
    ("overlay", "manifest"),
    ("package-verifier", "manifest"),
    ("permission", "manifest"),
    ("property", "activity"),
    ("property", "activity-alias"),
    ("property", "application"),
    ("property", "service"),
    ("property", "receiver"),
    ("property", "provider"),
    ("provider", "application"),
    ("receiver", "application"),
    ("required-feature", "uses-permission"),
    ("required-not-feature", "uses-permission"),
    ("screen", "compatible-screens"),
    ("service", "application"),
    ("sdk-library", "application"),
    ("static-library", "application"),
    ("supports-gl-texture", "manifest"),
    ("supports-input", "manifest"),
    ("supports-screens", "manifest"),
    ("uses-configuration", "manifest"),
    ("uses-feature", "feature-group"),
    ("uses-feature", "manifest"),
    ("uses-library", "application"),
    ("uses-native-library", "application"),
    ("uses-package", "application"),
    ("uses-permission", "manifest"),
    ("uses-permission-sdk-23", "manifest"),
    ("uses-sdk", "manifest"),
    ("uses-sdk-library", "application"),
    ("uses-static-library", "application"),
];

/// Components printed as `provides-component:` lines, in fixed order.
const PRINTABLE_COMPONENTS: &[&str] = &[
    "app-widget",
    "device-admin",
    "ime",
    "wallpaper",
    "accessibility",
    "print-service",
    "payment",
    "search",
    "document-provider",
    "launcher",
    "notification-listener",
    "dream",
    "camera",
    "camera-secure",
];

#[derive(Debug, Default)]
struct Components {
    discovered: BTreeSet<String>,
    other_activities: bool,
    other_receivers: bool,
    other_services: bool,
}

impl Components {
    fn print(&self, printer: &mut Printer) {
        for component in PRINTABLE_COMPONENTS {
            if self.discovered.contains(*component) {
                printer.print(format!("provides-component:'{component}'\n"));
            }
        }
        if self.discovered.contains("main") {
            printer.print("main\n");
        }
        if self.other_activities {
            printer.print("other-activities\n");
        }
        if self.other_receivers {
            printer.print("other-receivers\n");
        }
        if self.other_services {
            printer.print("other-services\n");
        }
    }
}

#[derive(Debug, Default)]
struct Architectures {
    architectures: BTreeSet<String>,
    alt_architectures: BTreeSet<String>,
}

impl Architectures {
    fn print(&self, printer: &mut Printer) {
        if !self.architectures.is_empty() {
            printer.print("native-code:");
            for arch in &self.architectures {
                printer.print(format!(" '{arch}'"));
            }
            printer.print("\n");
        }
        if !self.alt_architectures.is_empty() {
            printer.print("alt-native-code:");
            for arch in &self.alt_architectures {
                printer.print(format!(" '{arch}'"));
            }
            printer.print("\n");
        }
    }
}

/// Port of `android::util::ValidLibraryPathLastSlash`: returns the byte
/// index of the last slash when `path` is a valid `lib/<abi>/lib*.so`
/// member, else `None`.
fn valid_library_path_last_slash(
    path: &str,
    suppress_64bit: bool,
    debuggable: bool,
) -> Option<usize> {
    const APK_LIB: &str = "lib/";
    const LIB_PREFIX: &str = "/lib";
    const LIB_SUFFIX: &str = ".so";
    let min_length = APK_LIB.len() + 2 + LIB_PREFIX.len() + 1 + LIB_SUFFIX.len();
    if path.len() < min_length {
        return None;
    }
    let last_slash = path.rfind('/')?;
    // Skip directories.
    if last_slash + 1 >= path.len() {
        return None;
    }
    // Make sure the filename is safe.
    if !path[last_slash + 1..].bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'+' | b',' | b'-' | b'.' | b'/' | b'=' | b'_')
    }) {
        return None;
    }
    if !path.starts_with(APK_LIB) {
        return None;
    }
    // No subdirectories: the first '/' after "lib/" must be the last slash.
    if path[APK_LIB.len()..].find('/').map(|i| i + APK_LIB.len()) != Some(last_slash) {
        return None;
    }
    if !debuggable && (!path.ends_with(LIB_SUFFIX) || !path[last_slash..].starts_with(LIB_PREFIX)) {
        return None;
    }
    let abi = &path[APK_LIB.len()..last_slash];
    if suppress_64bit && (abi == "arm64-v8a" || abi == "x86_64") {
        return None;
    }
    Some(last_slash)
}

// ───────────────────────── the extractor ─────────────────────────

pub struct ManifestExtractor<'a> {
    apk: &'a LoadedApk,
    options: DumpManifestOptions,
    locales: BTreeMap<String, ConfigDescription>,
    densities: BTreeMap<u16, ConfigDescription>,
    parent_stack: Vec<usize>,
    target_sdk: i32,
    common: CommonFeatureGroup,
    arena: Vec<El>,
    root: Option<usize>,
    implied_permissions: Vec<UsesPermissionData>,
    feature_group_indices: Vec<usize>,
    components: Components,
    architectures: Architectures,
    supports_screen: SupportsScreenData,
}

impl<'a> ManifestExtractor<'a> {
    pub fn new(apk: &'a LoadedApk, options: DumpManifestOptions) -> Self {
        ManifestExtractor {
            apk,
            options,
            locales: BTreeMap::new(),
            densities: BTreeMap::new(),
            parent_stack: Vec::new(),
            target_sdk: 0,
            common: CommonFeatureGroup::default(),
            arena: Vec::new(),
            root: None,
            implied_permissions: Vec::new(),
            feature_group_indices: Vec::new(),
            components: Components::default(),
            architectures: Architectures::default(),
            supports_screen: SupportsScreenData::default(),
        }
    }

    fn raise_target_sdk(&mut self, min_target: i32) {
        if min_target > self.target_sdk {
            self.target_sdk = min_target;
        }
    }

    /// Loads an XML file from the APK (binary or proto form).
    fn load_xml(&self, path: &str) -> Option<XmlElement> {
        let data = self.apk.entry(path)?;
        match self.apk.format {
            ApkFormat::Proto => crate::xml::decode_pb_xml(data).ok(),
            _ => crate::xml::axml::parse_binary_xml(data).ok(),
        }
    }

    /// Port of `ManifestExtractor::Extract`.
    pub fn extract(&mut self, diag: &Diagnostics) -> bool {
        let Some(manifest) = &self.apk.manifest else {
            diag.error("failed to find AndroidManifest.xml");
            return false;
        };
        if manifest.name != "manifest" {
            diag.error("manifest does not start with <manifest> tag");
            return false;
        }

        if self.options.only_permissions {
            let root = self.inflate(manifest, "");
            self.root = Some(root);
            if let Data::Manifest(data) = &mut self.arena[root].data {
                data.only_package_name = true;
            } else {
                return false;
            }
            let children: Vec<usize> = manifest
                .child_elements()
                .filter(|child| {
                    child.name == "uses-permission"
                        || child.name == "uses-permission-sdk-23"
                        || child.name == "permission"
                })
                .map(|child| self.visit(child, "manifest"))
                .collect();
            self.arena[root].children.extend(children);
            return true;
        }

        // Collect information about the resource configurations.
        for package in &self.apk.table.packages {
            for ty in &package.types {
                for entry in &ty.entries {
                    for value in &entry.values {
                        let locale_str = value.config.get_bcp47_locale(false);
                        self.locales.entry(locale_str.clone()).or_insert_with(|| {
                            let mut config = default_config();
                            if !locale_str.is_empty() {
                                config.set_bcp47_locale(&locale_str);
                            }
                            config
                        });

                        let density = if value.config.density == 0 {
                            160
                        } else {
                            value.config.density
                        };
                        self.densities.entry(density).or_insert_with(|| {
                            let mut config = default_config();
                            config.density = density;
                            config
                        });
                    }
                }
            }
        }

        // Extract badging information.
        let root = self.visit(manifest, "");
        self.root = Some(root);

        // Filter out all <uses-sdk> tags besides the very last one.
        let uses_sdk_children: Vec<usize> = self.arena[root]
            .children
            .iter()
            .copied()
            .filter(|&c| matches!(self.arena[c].data, Data::UsesSdk(_)))
            .collect();
        if uses_sdk_children.len() >= 2 {
            let to_remove: BTreeSet<usize> = uses_sdk_children[..uses_sdk_children.len() - 1]
                .iter()
                .copied()
                .collect();
            self.arena[root].children.retain(|c| !to_remove.contains(c));
        }

        // Implied permissions.
        let find_permission = |s: &Self, name: &str| -> Option<usize> {
            s.find_element(
                root,
                &|idx| matches!(&s.arena[idx].data, Data::UsesPermission(p) if p.name == name),
            )
        };

        // Pre-1.6 implicitly granted permission compatibility logic.
        let mut insert_write_external = false;
        let write_external_permission =
            find_permission(self, "android.permission.WRITE_EXTERNAL_STORAGE");
        if self.target_sdk < SDK_DONUT {
            if write_external_permission.is_none() {
                self.add_implied_permission(
                    "android.permission.WRITE_EXTERNAL_STORAGE",
                    "targetSdkVersion < 4",
                    -1,
                );
                insert_write_external = true;
            }
            if find_permission(self, "android.permission.READ_PHONE_STATE").is_none() {
                self.add_implied_permission(
                    "android.permission.READ_PHONE_STATE",
                    "targetSdkVersion < 4",
                    -1,
                );
            }
        }

        // Apps requesting WRITE_EXTERNAL_STORAGE always take
        // READ_EXTERNAL_STORAGE as well.
        let read_external = find_permission(self, "android.permission.READ_EXTERNAL_STORAGE");
        if read_external.is_none() && (insert_write_external || write_external_permission.is_some())
        {
            let max_sdk = write_external_permission
                .map(|idx| match &self.arena[idx].data {
                    Data::UsesPermission(p) => p.max_sdk_version,
                    _ => -1,
                })
                .unwrap_or(-1);
            self.add_implied_permission(
                "android.permission.READ_EXTERNAL_STORAGE",
                "requested WRITE_EXTERNAL_STORAGE",
                max_sdk,
            );
        }

        // Pre-JellyBean call log permission compatibility.
        if self.target_sdk < SDK_JELLY_BEAN {
            if find_permission(self, "android.permission.READ_CALL_LOG").is_none()
                && find_permission(self, "android.permission.READ_CONTACTS").is_some()
            {
                self.add_implied_permission(
                    "android.permission.READ_CALL_LOG",
                    "targetSdkVersion < 16 and requested READ_CONTACTS",
                    -1,
                );
            }
            if find_permission(self, "android.permission.WRITE_CALL_LOG").is_none()
                && find_permission(self, "android.permission.WRITE_CONTACTS").is_some()
            {
                self.add_implied_permission(
                    "android.permission.WRITE_CALL_LOG",
                    "targetSdkVersion < 16 and requested WRITE_CONTACTS",
                    -1,
                );
            }
        }

        // Touchscreen → implied faketouch.
        if !self.common.has_feature("android.hardware.touchscreen") {
            self.common.add_implied_feature(
                "android.hardware.faketouch",
                "default feature for all apps",
                false,
            );
        }

        // Only print the common feature group if no feature group is defined.
        let mut feature_groups = Vec::new();
        self.for_each_child(root, &mut |idx| {
            if matches!(self.arena[idx].data, Data::FeatureGroup(_)) {
                feature_groups.push(idx);
            }
        });
        if !feature_groups.is_empty() {
            // Merge the common feature group into each feature group.
            let common = self.common.group.clone();
            for &idx in &feature_groups {
                if let Data::FeatureGroup(group) = &mut self.arena[idx].data {
                    group.merge(&common);
                }
            }
        }
        self.feature_group_indices = feature_groups;

        // Collect the component types of the application.
        let mut discovered = Vec::new();
        self.for_each_child(root, &mut |idx| match &self.arena[idx].data {
            Data::Action { component } if !component.is_empty() => {
                discovered.push(component.clone());
            }
            Data::Category { component } if !component.is_empty() => {
                discovered.push(component.clone());
            }
            _ => {}
        });
        self.components.discovered.extend(discovered);

        // Check for the payment component.
        let mut services = Vec::new();
        self.for_each_child(root, &mut |idx| {
            if matches!(self.arena[idx].data, Data::Service { .. }) {
                services.push(idx);
            }
        });
        for service in services {
            let host_apdu_action = self
                .find_element(service, &|idx| {
                    matches!(&self.arena[idx].data, Data::Action { component } if component == "host-apdu")
                })
                .is_some();
            let offhost_apdu_action = self
                .find_element(service, &|idx| {
                    matches!(&self.arena[idx].data, Data::Action { component } if component == "offhost-apdu")
                })
                .is_some();
            let mut resources_to_check = Vec::new();
            self.for_each_child(service, &mut |idx| {
                if let Data::MetaData(meta) = &self.arena[idx].data {
                    if (meta.name == "android.nfc.cardemulation.host_apdu_service"
                        && host_apdu_action)
                        || (meta.name == "android.nfc.cardemulation.off_host_apdu_service"
                            && offhost_apdu_action)
                    {
                        if !meta.resource.is_empty() {
                            resources_to_check.push(meta.resource.clone());
                        }
                    }
                }
            });
            'outer: for resource in resources_to_check {
                let Some(xml_root) = self.load_xml(&resource) else {
                    continue;
                };
                if (host_apdu_action && xml_root.name == "host-apdu-service")
                    || (offhost_apdu_action && xml_root.name == "offhost-apdu-service")
                {
                    for child in xml_root.child_elements() {
                        if child.name == "aid-group" {
                            if let Some(category) = find_attr_by_id(child, CATEGORY_ATTR) {
                                if category.value == "payment" {
                                    self.components.discovered.insert("payment".to_string());
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Presence of activities, receivers, and services with no
        // special components.
        self.components.other_activities = self
            .find_element(
                root,
                &|idx| matches!(&self.arena[idx].data, Data::Activity(a) if !a.has_component),
            )
            .is_some();
        self.components.other_receivers = self
            .find_element(root, &|idx| {
                matches!(&self.arena[idx].data, Data::Receiver { has_component, .. } if !has_component)
            })
            .is_some();
        self.components.other_services = self
            .find_element(root, &|idx| {
                matches!(&self.arena[idx].data, Data::Service { has_component, .. } if !has_component)
            })
            .is_some();

        // Gather the supported screens.
        if let Some(idx) = self.find_element(root, &|idx| {
            matches!(self.arena[idx].data, Data::SupportsScreen(_))
        }) {
            if let Data::SupportsScreen(data) = &self.arena[idx].data {
                self.supports_screen = data.clone();
            }
        }

        // Gather the supported architectures of the app.
        let has_renderscript_bitcode = self.apk.entries().any(|(path, _)| path.ends_with(".bc"));
        let mut architectures_from_apk: BTreeSet<String> = BTreeSet::new();
        for (path, _) in self.apk.entries() {
            if let Some(last_slash) =
                valid_library_path_last_slash(path, has_renderscript_bitcode, false)
            {
                architectures_from_apk.insert(path[4..last_slash].to_string());
            }
        }

        // Determine if the application has multiArch support.
        let has_multi_arch = self
            .find_element(root, &|idx| {
                matches!(&self.arena[idx].data, Data::Application(app) if app.has_multi_arch)
            })
            .is_some();

        let mut output_alt_native_code = false;
        if has_multi_arch {
            // Report the 64-bit version only; the rest go to the alt list.
            let arch = if architectures_from_apk.contains("x86_64") {
                Some("x86_64".to_string())
            } else if architectures_from_apk.contains("arm64-v8a") {
                Some("arm64-v8a".to_string())
            } else {
                None
            };
            if let Some(arch) = arch {
                architectures_from_apk.remove(&arch);
                self.architectures.architectures.insert(arch);
                output_alt_native_code = true;
            }
        }
        for arch in architectures_from_apk {
            if output_alt_native_code {
                self.architectures.alt_architectures.insert(arch);
            } else {
                self.architectures.architectures.insert(arch);
            }
        }
        true
    }

    fn add_implied_permission(&mut self, name: &str, reason: &str, max_sdk_version: i32) {
        self.implied_permissions.push(UsesPermissionData {
            implied: true,
            name: name.to_string(),
            implied_reason: reason.to_string(),
            max_sdk_version,
            required: 1,
            ..Default::default()
        });
    }

    /// Port of `ManifestExtractor::Visit`.
    fn visit(&mut self, el: &XmlElement, parent_tag: &str) -> usize {
        let idx = self.inflate(el, parent_tag);
        self.parent_stack.insert(0, idx);
        let children: Vec<usize> = el
            .child_elements()
            .map(|child| self.visit(child, &el.name))
            .collect();
        self.arena[idx].children.extend(children);
        self.parent_stack.remove(0);
        idx
    }

    /// Recursively iterates the children of `root` in depth-first order
    /// (`ForEachChild`).
    fn for_each_child(&self, root: usize, f: &mut impl FnMut(usize)) {
        for &child in &self.arena[root].children {
            f(child);
            self.for_each_child(child, f);
        }
    }

    /// Port of `FindElement`: checks the root, then the children in
    /// reverse order, recursively.
    fn find_element(&self, root: usize, f: &dyn Fn(usize) -> bool) -> Option<usize> {
        if f(root) {
            return Some(root);
        }
        for &child in self.arena[root].children.iter().rev() {
            if let Some(found) = self.find_element(child, f) {
                return Some(found);
            }
        }
        None
    }

    /// Port of `ManifestExtractor::Element::Inflate` + the per-class
    /// `Extract` methods.
    fn inflate(&mut self, el: &XmlElement, parent_tag: &str) -> usize {
        let featured = VALID_CHILD_PARENT_TAGS
            .iter()
            .any(|(child, parent)| *child == el.name && *parent == parent_tag);
        let data = if featured {
            self.extract_data(el)
        } else {
            Data::None
        };
        self.arena.push(El {
            data,
            children: Vec::new(),
        });
        self.arena.len() - 1
    }

    fn extract_data(&mut self, el: &XmlElement) -> Data {
        let apk = self.apk;
        let config = default_config();
        let s = |attr: Option<&XmlAttribute>| get_attr_string(apk, attr, &config);
        let sd = |attr: Option<&XmlAttribute>, def: &str| {
            get_attr_string_default(apk, attr, def, &config)
        };
        let i = |attr: Option<&XmlAttribute>| get_attr_integer(apk, attr, &config);
        let id = |attr: Option<&XmlAttribute>, def: i32| {
            get_attr_integer_default(apk, attr, def, &config)
        };

        match el.name.as_str() {
            "manifest" => Data::Manifest(ManifestData {
                only_package_name: false,
                package: sd(find_attr(el, "", "package"), ""),
                version_code: id(find_attr_by_id(el, VERSION_CODE_ATTR), 0),
                version_name: sd(find_attr_by_id(el, VERSION_NAME_ATTR), ""),
                split: s(find_attr(el, "", "split")),
                platform_version_name: s(find_attr(el, "", "platformBuildVersionName")),
                platform_version_code: s(find_attr(el, "", "platformBuildVersionCode")),
                platform_version_name_int: i(find_attr(el, "", "platformBuildVersionName")),
                platform_version_code_int: i(find_attr(el, "", "platformBuildVersionCode")),
                compilesdk_version: i(find_attr_by_id(el, COMPILE_SDK_VERSION_ATTR)),
                compilesdk_version_codename: s(find_attr_by_id(
                    el,
                    COMPILE_SDK_VERSION_CODENAME_ATTR,
                )),
                install_location: i(find_attr_by_id(el, INSTALL_LOCATION_ATTR)),
            }),
            "application" => {
                let mut data = ApplicationData {
                    label: sd(find_attr_by_id(el, LABEL_ATTR), ""),
                    icon: sd(find_attr_by_id(el, ICON_ATTR), ""),
                    test_only: id(find_attr_by_id(el, TEST_ONLY_ATTR), 0),
                    banner: sd(find_attr_by_id(el, BANNER_ATTR), ""),
                    is_game: id(find_attr_by_id(el, ISGAME_ATTR), 0),
                    debuggable: id(find_attr_by_id(el, DEBUGGABLE_ATTR), 0),
                    has_multi_arch: id(find_attr(el, ANDROID_NS, "multiArch"), 0) != 0,
                    ..Default::default()
                };

                // App names for every locale the app supports.
                let label_attr = find_attr_by_id(el, LABEL_ATTR);
                for (locale, locale_config) in &self.locales {
                    if let Some(label) = get_attr_string(apk, label_attr, locale_config) {
                        data.locale_labels.insert(locale.clone(), label);
                    }
                }

                // Icons for the densities the app supports.
                let icon_attr = find_attr_by_id(el, ICON_ATTR);
                for (&density, density_config) in &self.densities {
                    if let Some(icon) = get_attr_string(apk, icon_attr, density_config) {
                        data.density_icons.insert(density, icon);
                    }
                }
                Data::Application(data)
            }
            "uses-sdk" => {
                let data = UsesSdkData {
                    min_sdk: i(find_attr_by_id(el, MIN_SDK_VERSION_ATTR)),
                    min_sdk_name: s(find_attr_by_id(el, MIN_SDK_VERSION_ATTR)),
                    max_sdk: i(find_attr_by_id(el, MAX_SDK_VERSION_ATTR)),
                    target_sdk: i(find_attr_by_id(el, TARGET_SDK_VERSION_ATTR)),
                    target_sdk_name: s(find_attr_by_id(el, TARGET_SDK_VERSION_ATTR)),
                };

                // Reset and recompute the target SDK; only the values of the
                // last <uses-sdk> element are used.
                self.target_sdk = 0;
                if data.min_sdk_name.as_deref() == Some("Donut")
                    || data.target_sdk_name.as_deref() == Some("Donut")
                {
                    self.raise_target_sdk(SDK_DONUT);
                }
                if let Some(min_sdk) = data.min_sdk {
                    self.raise_target_sdk(min_sdk);
                }
                if let Some(target_sdk) = data.target_sdk {
                    self.raise_target_sdk(target_sdk);
                } else if data.target_sdk_name.is_some() {
                    self.raise_target_sdk(SDK_CUR_DEVELOPMENT);
                }
                Data::UsesSdk(data)
            }
            "uses-configuration" => Data::UsesConfiguration(UsesConfigurationData {
                req_touch_screen: id(find_attr_by_id(el, REQ_TOUCH_SCREEN_ATTR), 0),
                req_keyboard_type: id(find_attr_by_id(el, REQ_KEYBOARD_TYPE_ATTR), 0),
                req_hard_keyboard: id(find_attr_by_id(el, REQ_HARD_KEYBOARD_ATTR), 0),
                req_navigation: id(find_attr_by_id(el, REQ_NAVIGATION_ATTR), 0),
                req_five_way_nav: id(find_attr_by_id(el, REQ_FIVE_WAY_NAV_ATTR), 0),
            }),
            "supports-screens" => {
                let mut data = SupportsScreenData {
                    small_screen: id(find_attr_by_id(el, SMALL_SCREEN_ATTR), 1),
                    normal_screen: id(find_attr_by_id(el, NORMAL_SCREEN_ATTR), 1),
                    large_screen: id(find_attr_by_id(el, LARGE_SCREEN_ATTR), 1),
                    xlarge_screen: id(find_attr_by_id(el, XLARGE_SCREEN_ATTR), 1),
                    any_density: id(find_attr_by_id(el, ANY_DENSITY_ATTR), 1),
                    requires_smallest_width_dp: id(
                        find_attr_by_id(el, REQUIRES_SMALLEST_WIDTH_DP_ATTR),
                        0,
                    ),
                    compatible_width_limit_dp: id(
                        find_attr_by_id(el, COMPATIBLE_WIDTH_LIMIT_DP_ATTR),
                        0,
                    ),
                    largest_width_limit_dp: id(find_attr_by_id(el, LARGEST_WIDTH_LIMIT_DP_ATTR), 0),
                };
                // Infer screen size buckets from the width ranges.
                if data.small_screen > 0
                    && data.normal_screen > 0
                    && data.large_screen > 0
                    && data.xlarge_screen > 0
                    && data.requires_smallest_width_dp > 0
                {
                    let compat_width = if data.compatible_width_limit_dp > 0 {
                        data.compatible_width_limit_dp
                    } else {
                        data.requires_smallest_width_dp
                    };
                    let rs = data.requires_smallest_width_dp;
                    data.small_screen = if rs <= 240 && compat_width >= 240 {
                        -1
                    } else {
                        0
                    };
                    data.normal_screen = if rs <= 320 && compat_width >= 320 {
                        -1
                    } else {
                        0
                    };
                    data.large_screen = if rs <= 480 && compat_width >= 480 {
                        -1
                    } else {
                        0
                    };
                    data.xlarge_screen = if rs <= 720 && compat_width >= 720 {
                        -1
                    } else {
                        0
                    };
                }
                Data::SupportsScreen(data)
            }
            "feature-group" => Data::FeatureGroup(FeatureGroupData {
                label: sd(find_attr_by_id(el, LABEL_ATTR), ""),
                ..Default::default()
            }),
            "uses-feature" => {
                let name = s(find_attr_by_id(el, NAME_ATTR));
                let gl = i(find_attr_by_id(el, GL_ES_VERSION_ATTR));
                let mut required = id(find_attr_by_id(el, REQUIRED_ATTR), 1) != 0;
                let version = id(find_attr(el, ANDROID_NS, "version"), 0);

                // Add the feature to the parent feature group if one
                // exists; otherwise to the common feature group.
                let parent_group = self
                    .parent_stack
                    .first()
                    .copied()
                    .filter(|&p| matches!(self.arena[p].data, Data::FeatureGroup(_)));
                if parent_group.is_some() {
                    // All features inside <feature-group> are required.
                    required = true;
                }
                match parent_group {
                    Some(p) => {
                        if let Data::FeatureGroup(group) = &mut self.arena[p].data {
                            if let Some(name) = &name {
                                group.add_feature(name, required, version);
                            } else if let Some(gl) = gl {
                                group.open_gles_version = group.open_gles_version.max(gl);
                            }
                        }
                    }
                    None => {
                        if let Some(name) = &name {
                            self.common.group.add_feature(name, required, version);
                        } else if let Some(gl) = gl {
                            self.common.group.open_gles_version =
                                self.common.group.open_gles_version.max(gl);
                        }
                    }
                }
                Data::UsesFeature
            }
            "uses-permission" => {
                let name = sd(find_attr_by_id(el, NAME_ATTR), "");
                let mut required_features = Vec::new();
                let feature = sd(find_attr_by_id(el, REQUIRED_FEATURE_ATTR), "");
                if !feature.is_empty() {
                    required_features.push(feature);
                }
                let mut required_not_features = Vec::new();
                let feature = sd(find_attr_by_id(el, REQUIRED_NOT_FEATURE_ATTR), "");
                if !feature.is_empty() {
                    required_not_features.push(feature);
                }
                let data = UsesPermissionData {
                    implied: false,
                    name: name.clone(),
                    required_features,
                    required_not_features,
                    required: id(find_attr_by_id(el, REQUIRED_ATTR), 1),
                    max_sdk_version: id(find_attr_by_id(el, MAX_SDK_VERSION_ATTR), -1),
                    uses_permission_flags: id(find_attr_by_id(el, USES_PERMISSION_FLAGS_ATTR), 0),
                    implied_reason: String::new(),
                };
                if !name.is_empty() {
                    let target_sdk = self.target_sdk;
                    self.common
                        .add_implied_features_for_permission(target_sdk, &name, false);
                }
                Data::UsesPermission(data)
            }
            "required-feature" => {
                let name = sd(find_attr_by_id(el, NAME_ATTR), "");
                if !name.is_empty() {
                    if let Some(&parent) = self.parent_stack.first() {
                        if let Data::UsesPermission(p) = &mut self.arena[parent].data {
                            p.required_features.push(name);
                        }
                    }
                }
                Data::RequiredFeature
            }
            "required-not-feature" => {
                let name = sd(find_attr_by_id(el, NAME_ATTR), "");
                if !name.is_empty() {
                    if let Some(&parent) = self.parent_stack.first() {
                        if let Data::UsesPermission(p) = &mut self.arena[parent].data {
                            p.required_not_features.push(name);
                        }
                    }
                }
                Data::RequiredNotFeature
            }
            "uses-permission-sdk-23" => {
                let data = UsesPermissionSdk23Data {
                    name: s(find_attr_by_id(el, NAME_ATTR)),
                    max_sdk_version: i(find_attr_by_id(el, MAX_SDK_VERSION_ATTR)),
                };
                if let Some(name) = data.name.clone() {
                    let target_sdk = self.target_sdk;
                    self.common
                        .add_implied_features_for_permission(target_sdk, &name, true);
                }
                Data::UsesPermissionSdk23(data)
            }
            "permission" => Data::Permission {
                name: sd(find_attr_by_id(el, NAME_ATTR), ""),
            },
            "activity" => {
                let mut name = sd(find_attr_by_id(el, NAME_ATTR), "");
                let label = sd(find_attr_by_id(el, LABEL_ATTR), "");
                let icon = sd(find_attr_by_id(el, ICON_ATTR), "");
                let banner = sd(find_attr_by_id(el, BANNER_ATTR), "");

                // Retrieve the package name from the manifest.
                let mut package = String::new();
                for &parent in &self.parent_stack {
                    if let Data::Manifest(manifest) = &self.arena[parent].data {
                        package = manifest.package.clone();
                        break;
                    }
                }

                // Fully qualify the activity name.
                match name.find('.') {
                    Some(0) => name = format!("{package}{name}"),
                    None => name = format!("{package}.{name}"),
                    Some(_) => {}
                }

                if let Some(orientation) = i(find_attr_by_id(el, SCREEN_ORIENTATION_ATTR)) {
                    match orientation {
                        0 | 6 | 8 => self.common.add_implied_feature(
                            "android.hardware.screen.landscape",
                            "one or more activities have specified a landscape orientation",
                            false,
                        ),
                        1 | 7 | 9 => self.common.add_implied_feature(
                            "android.hardware.screen.portrait",
                            "one or more activities have specified a portrait orientation",
                            false,
                        ),
                        _ => {}
                    }
                }
                Data::Activity(ActivityData {
                    name,
                    icon,
                    label,
                    banner,
                    ..Default::default()
                })
            }
            "intent-filter" => Data::IntentFilter,
            "category" => {
                let category = s(find_attr_by_id(el, NAME_ATTR));
                let mut component = String::new();
                if let (Some(category), [first, second, ..]) = (&category, &self.parent_stack[..]) {
                    if matches!(self.arena[*first].data, Data::IntentFilter) {
                        let second = *second;
                        if let Data::Activity(activity) = &mut self.arena[second].data {
                            match category.as_str() {
                                "android.intent.category.LAUNCHER" => {
                                    activity.has_launcher_category = true;
                                }
                                "android.intent.category.LEANBACK_LAUNCHER" => {
                                    activity.has_leanback_launcher_category = true;
                                }
                                "android.intent.category.HOME" => {
                                    component = "launcher".to_string();
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Data::Category { component }
            }
            "provider" => {
                let exported = i(find_attr_by_id(el, EXPORTED_ATTR));
                let grant_uri_permissions = i(find_attr_by_id(el, GRANT_URI_PERMISSIONS_ATTR));
                let permission = s(find_attr_by_id(el, PERMISSION_ATTR));
                Data::Provider {
                    has_required_saf_attributes: exported.is_some_and(|e| e != 0)
                        && grant_uri_permissions.is_some_and(|g| g != 0)
                        && permission.as_deref() == Some("android.permission.MANAGE_DOCUMENTS"),
                }
            }
            "receiver" => Data::Receiver {
                permission: s(find_attr_by_id(el, PERMISSION_ATTR)),
                has_component: false,
            },
            "service" => Data::Service {
                permission: s(find_attr_by_id(el, PERMISSION_ATTR)),
                has_component: false,
            },
            "action" => {
                let action = sd(find_attr_by_id(el, NAME_ATTR), "");
                let mut component = String::new();
                if let [first, second, ..] = self.parent_stack[..] {
                    if matches!(self.arena[first].data, Data::IntentFilter) {
                        match &mut self.arena[second].data {
                            Data::Activity(activity) => {
                                let mapped = match action.as_str() {
                                    "android.intent.action.MAIN" => Some("main"),
                                    "android.media.action.VIDEO_CAMERA"
                                    | "android.media.action.STILL_IMAGE_CAMERA" => Some("camera"),
                                    "android.media.action.STILL_IMAGE_CAMERA_SECURE" => {
                                        Some("camera-secure")
                                    }
                                    _ => None,
                                };
                                if let Some(mapped) = mapped {
                                    component = mapped.to_string();
                                    activity.has_component = true;
                                }
                                if action == "android.intent.action.MAIN" {
                                    activity.has_main_action = true;
                                }
                            }
                            Data::Receiver {
                                permission,
                                has_component,
                            } => {
                                let mapped = match action.as_str() {
                                    "android.appwidget.action.APPWIDGET_UPDATE" => {
                                        Some(("app-widget", None))
                                    }
                                    "android.app.action.DEVICE_ADMIN_ENABLED" => Some((
                                        "device-admin",
                                        Some("android.permission.BIND_DEVICE_ADMIN"),
                                    )),
                                    _ => None,
                                };
                                if let Some((mapped, required_permission)) = mapped {
                                    let allowed = match required_permission {
                                        None => true,
                                        Some(required) => permission.as_deref() == Some(required),
                                    };
                                    if allowed {
                                        *has_component = true;
                                        component = mapped.to_string();
                                    }
                                }
                            }
                            Data::Service {
                                permission,
                                has_component,
                            } => {
                                let mapped: Option<(&str, Option<&str>)> = match action.as_str() {
                                    "android.view.InputMethod" => Some(("ime", None)),
                                    "android.service.wallpaper.WallpaperService" => {
                                        Some(("wallpaper", None))
                                    }
                                    "android.accessibilityservice.AccessibilityService" => Some((
                                        "accessibility",
                                        Some("android.permission.BIND_ACCESSIBILITY_SERVICE"),
                                    )),
                                    "android.printservice.PrintService" => Some((
                                        "print-service",
                                        Some("android.permission.BIND_PRINT_SERVICE"),
                                    )),
                                    "android.nfc.cardemulation.action.HOST_APDU_SERVICE" => Some((
                                        "host-apdu",
                                        Some("android.permission.BIND_NFC_SERVICE"),
                                    )),
                                    "android.nfc.cardemulation.action.OFF_HOST_APDU_SERVICE" => {
                                        Some((
                                            "offhost-apdu",
                                            Some("android.permission.BIND_NFC_SERVICE"),
                                        ))
                                    }
                                    "android.service.notification.NotificationListenerService" => {
                                        Some((
                                            "notification-listener",
                                            Some("android.permission.BIND_NOTIFICATION_LISTENER_SERVICE"),
                                        ))
                                    }
                                    "android.service.dreams.DreamService" => Some((
                                        "dream",
                                        Some("android.permission.BIND_DREAM_SERVICE"),
                                    )),
                                    _ => None,
                                };
                                if let Some((mapped, required_permission)) = mapped {
                                    let allowed = match required_permission {
                                        None => true,
                                        Some(required) => permission.as_deref() == Some(required),
                                    };
                                    if allowed {
                                        *has_component = true;
                                        component = mapped.to_string();
                                    }
                                }
                            }
                            Data::Provider {
                                has_required_saf_attributes,
                            } => {
                                if action == "android.content.action.DOCUMENTS_PROVIDER"
                                    && *has_required_saf_attributes
                                {
                                    component = "document-provider".to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Represents a searchable interface.
                if action == "android.intent.action.SEARCH" {
                    component = "search".to_string();
                }
                Data::Action { component }
            }
            "uses-library" => Data::UsesLibrary {
                name: sd(find_attr_by_id(el, NAME_ATTR), ""),
                required: id(find_attr_by_id(el, REQUIRED_ATTR), 1),
            },
            "static-library" => Data::StaticLibrary {
                name: sd(find_attr_by_id(el, NAME_ATTR), ""),
                version: id(find_attr_by_id(el, VERSION_ATTR), 0),
                version_major: id(find_attr_by_id(el, VERSION_MAJOR_ATTR), 0),
            },
            "uses-static-library" => {
                let mut cert_digests = Vec::new();
                let digest: String = sd(find_attr_by_id(el, CERT_DIGEST_ATTR), "")
                    .chars()
                    .filter(|&c| c != ':')
                    .collect();
                if !digest.is_empty() {
                    cert_digests.push(digest);
                }
                Data::UsesStaticLibrary {
                    name: sd(find_attr_by_id(el, NAME_ATTR), ""),
                    version: id(find_attr_by_id(el, VERSION_ATTR), 0),
                    version_major: id(find_attr_by_id(el, VERSION_MAJOR_ATTR), 0),
                    cert_digests,
                }
            }
            "sdk-library" => Data::SdkLibrary {
                name: sd(find_attr_by_id(el, NAME_ATTR), ""),
                version_major: id(find_attr_by_id(el, VERSION_MAJOR_ATTR), 0),
            },
            "uses-sdk-library" => {
                let mut cert_digests = Vec::new();
                let digest: String = sd(find_attr_by_id(el, CERT_DIGEST_ATTR), "")
                    .chars()
                    .filter(|&c| c != ':')
                    .collect();
                if !digest.is_empty() {
                    cert_digests.push(digest);
                }
                Data::UsesSdkLibrary {
                    name: sd(find_attr_by_id(el, NAME_ATTR), ""),
                    version_major: id(find_attr_by_id(el, VERSION_MAJOR_ATTR), 0),
                    cert_digests,
                }
            }
            "uses-native-library" => Data::UsesNativeLibrary {
                name: sd(find_attr_by_id(el, NAME_ATTR), ""),
                required: id(find_attr_by_id(el, REQUIRED_ATTR), 1),
            },
            "meta-data" => Data::MetaData(MetaDataData {
                name: sd(find_attr_by_id(el, NAME_ATTR), ""),
                value: sd(find_attr_by_id(el, VALUE_ATTR), ""),
                value_int: i(find_attr_by_id(el, VALUE_ATTR)),
                resource: sd(find_attr_by_id(el, RESOURCE_ATTR), ""),
                resource_int: i(find_attr_by_id(el, RESOURCE_ATTR)),
            }),
            "supports-input" => Data::SupportsInput { inputs: Vec::new() },
            "input-type" => {
                let name = s(find_attr_by_id(el, NAME_ATTR));
                if let (Some(name), Some(&parent)) = (name, self.parent_stack.first()) {
                    if let Data::SupportsInput { inputs } = &mut self.arena[parent].data {
                        inputs.push(name);
                    }
                }
                Data::InputType
            }
            "install-constraints" => {
                let mut fingerprint_prefixes = Vec::new();
                for child in el.child_elements() {
                    if child.name == "fingerprint-prefix" {
                        if let Some(attr) = find_attr(child, ANDROID_NS, "value") {
                            fingerprint_prefixes.push(attr.value.clone());
                        }
                    }
                }
                Data::InstallConstraints {
                    fingerprint_prefixes,
                }
            }
            "original-package" => Data::OriginalPackage {
                name: s(find_attr_by_id(el, NAME_ATTR)),
            },
            "overlay" => Data::Overlay(OverlayData {
                target_package: s(find_attr_by_id(el, TARGET_PACKAGE_ATTR)),
                priority: id(find_attr_by_id(el, PRIORITY_ATTR), 0),
                is_static: id(find_attr_by_id(el, IS_STATIC_ATTR), 0) != 0,
                required_property_name: s(find_attr_by_id(el, REQUIRED_SYSTEM_PROPERTY_NAME_ATTR)),
                required_property_value: s(find_attr_by_id(
                    el,
                    REQUIRED_SYSTEM_PROPERTY_VALUE_ATTR,
                )),
            }),
            "package-verifier" => Data::PackageVerifier {
                name: s(find_attr_by_id(el, NAME_ATTR)),
                public_key: s(find_attr_by_id(el, PUBLIC_KEY_ATTR)),
            },
            "uses-package" => {
                let mut cert_digests = Vec::new();
                let digest: String = sd(find_attr_by_id(el, CERT_DIGEST_ATTR), "")
                    .chars()
                    .filter(|&c| c != ':')
                    .collect();
                if !digest.is_empty() {
                    cert_digests.push(digest);
                }
                Data::UsesPackage(UsesPackageData {
                    package_type: s(find_attr_by_id(el, PACKAGE_TYPE_ATTR)),
                    name: s(find_attr_by_id(el, NAME_ATTR)),
                    version: id(find_attr_by_id(el, VERSION_ATTR), 0),
                    version_major: id(find_attr_by_id(el, VERSION_MAJOR_ATTR), 0),
                    cert_digests,
                })
            }
            "additional-certificate" => {
                let digest: String = sd(find_attr_by_id(el, CERT_DIGEST_ATTR), "")
                    .chars()
                    .filter(|&c| c != ':')
                    .collect();
                if !digest.is_empty() {
                    if let Some(&parent) = self.parent_stack.first() {
                        match &mut self.arena[parent].data {
                            Data::UsesPackage(uses) => uses.cert_digests.push(digest),
                            Data::UsesStaticLibrary { cert_digests, .. } => {
                                cert_digests.push(digest)
                            }
                            _ => {}
                        }
                    }
                }
                Data::AdditionalCertificate
            }
            "screen" => Data::Screen {
                size: i(find_attr_by_id(el, SCREEN_SIZE_ATTR)),
                density: i(find_attr_by_id(el, SCREEN_DENSITY_ATTR)),
            },
            "compatible-screens" => Data::CompatibleScreens,
            "supports-gl-texture" => Data::SupportsGlTexture {
                name: s(find_attr_by_id(el, NAME_ATTR)),
            },
            "property" => Data::Property(PropertyData {
                name: sd(find_attr_by_id(el, NAME_ATTR), ""),
                value: sd(find_attr_by_id(el, VALUE_ATTR), ""),
                value_int: i(find_attr_by_id(el, VALUE_ATTR)),
                resource: sd(find_attr_by_id(el, RESOURCE_ATTR), ""),
                resource_int: i(find_attr_by_id(el, RESOURCE_ATTR)),
            }),
            _ => Data::None,
        }
    }

    // ───────────────────────── printing ─────────────────────────

    /// Port of `ManifestExtractor::Dump`.
    pub fn dump(&self, printer: &mut Printer) {
        if let Some(root) = self.root {
            self.print_element(root, printer);
        }
        if self.options.only_permissions {
            return;
        }

        for implied_permission in &self.implied_permissions {
            implied_permission.print(printer);
        }

        if self.feature_group_indices.is_empty() {
            self.common.print_group(printer);
        } else {
            for &idx in &self.feature_group_indices {
                if let Data::FeatureGroup(group) = &self.arena[idx].data {
                    group.print_group(printer);
                }
            }
        }

        self.components.print(printer);
        self.supports_screen.print_screens(printer, self.target_sdk);

        // All the unique locales of the APK.
        printer.print("locales:");
        for locale in self.locales.keys() {
            if locale.is_empty() {
                printer.print(" '--_--'");
            } else {
                printer.print(format!(" '{locale}'"));
            }
        }
        printer.print("\n");

        // All the densities of the APK.
        printer.print("densities:");
        for density in self.densities.keys() {
            printer.print(format!(" '{density}'"));
        }
        printer.print("\n");

        self.architectures.print(printer);
    }

    fn print_element(&self, idx: usize, printer: &mut Printer) {
        self.print_data(idx, printer);
        for &child in &self.arena[idx].children {
            self.print_element(child, printer);
        }
    }

    fn print_data(&self, idx: usize, printer: &mut Printer) {
        match &self.arena[idx].data {
            Data::Manifest(data) => self.print_manifest(data, printer),
            Data::Application(data) => self.print_application(data, printer),
            Data::UsesSdk(data) => {
                if let Some(min_sdk) = data.min_sdk {
                    printer.print(format!("minSdkVersion:'{min_sdk}'\n"));
                } else if let Some(min_sdk_name) = &data.min_sdk_name {
                    printer.print(format!("minSdkVersion:'{min_sdk_name}'\n"));
                }
                if let Some(max_sdk) = data.max_sdk {
                    printer.print(format!("maxSdkVersion:'{max_sdk}'\n"));
                }
                if let Some(target_sdk) = data.target_sdk {
                    printer.print(format!("targetSdkVersion:'{target_sdk}'\n"));
                } else if let Some(target_sdk_name) = &data.target_sdk_name {
                    printer.print(format!("targetSdkVersion:'{target_sdk_name}'\n"));
                }
            }
            Data::UsesConfiguration(data) => {
                printer.print("uses-configuration:");
                if data.req_touch_screen != 0 {
                    printer.print(format!(" reqTouchScreen='{}'", data.req_touch_screen));
                }
                if data.req_keyboard_type != 0 {
                    printer.print(format!(" reqKeyboardType='{}'", data.req_keyboard_type));
                }
                if data.req_hard_keyboard != 0 {
                    printer.print(format!(" reqHardKeyboard='{}'", data.req_hard_keyboard));
                }
                if data.req_navigation != 0 {
                    printer.print(format!(" reqNavigation='{}'", data.req_navigation));
                }
                if data.req_five_way_nav != 0 {
                    printer.print(format!(" reqFiveWayNav='{}'", data.req_five_way_nav));
                }
                printer.print("\n");
            }
            Data::UsesPermission(data) => data.print(printer),
            Data::UsesPermissionSdk23(data) => {
                if let Some(name) = &data.name {
                    printer.print(format!("uses-permission-sdk-23: name='{name}'"));
                    if let Some(max_sdk_version) = data.max_sdk_version {
                        printer.print(format!(" maxSdkVersion='{max_sdk_version}'"));
                    }
                    printer.print("\n");
                }
            }
            Data::Permission { name } => {
                if self.options.only_permissions && !name.is_empty() {
                    printer.print(format!("permission: {name}\n"));
                }
            }
            Data::Activity(data) => {
                if data.has_main_action && data.has_launcher_category {
                    printer.print("launchable-activity:");
                    if !data.name.is_empty() {
                        printer.print(format!(" name='{}' ", data.name));
                    }
                    printer.print(format!(
                        " label='{}' icon='{}'\n",
                        normalize_for_output(&data.label),
                        data.icon
                    ));
                }
                if data.has_leanback_launcher_category {
                    printer.print("leanback-launchable-activity:");
                    if !data.name.is_empty() {
                        printer.print(format!(" name='{}' ", data.name));
                    }
                    printer.print(format!(
                        " label='{}' icon='{}' banner='{}'\n",
                        normalize_for_output(&data.label),
                        data.icon,
                        data.banner
                    ));
                }
            }
            Data::UsesLibrary { name, required } => {
                if !name.is_empty() {
                    printer.print(format!(
                        "uses-library{}:'{}'\n",
                        if *required == 0 { "-not-required" } else { "" },
                        name
                    ));
                }
            }
            Data::StaticLibrary {
                name,
                version,
                version_major,
            } => {
                printer.print(format!(
                    "static-library: name='{name}' version='{version}' versionMajor='{version_major}'\n"
                ));
            }
            Data::UsesStaticLibrary {
                name,
                version,
                version_major,
                cert_digests,
            } => {
                printer.print(format!(
                    "uses-static-library: name='{name}' version='{version}' versionMajor='{version_major}'"
                ));
                for digest in cert_digests {
                    printer.print(format!(" certDigest='{digest}'"));
                }
                printer.print("\n");
            }
            Data::SdkLibrary {
                name,
                version_major,
            } => {
                printer.print(format!(
                    "sdk-library: name='{name}' versionMajor='{version_major}'\n"
                ));
            }
            Data::UsesSdkLibrary {
                name,
                version_major,
                cert_digests,
            } => {
                printer.print(format!(
                    "uses-sdk-library: name='{name}' versionMajor='{version_major}'"
                ));
                for digest in cert_digests {
                    printer.print(format!(" certDigest='{digest}'"));
                }
                printer.print("\n");
            }
            Data::UsesNativeLibrary { name, required } => {
                if !name.is_empty() {
                    printer.print(format!(
                        "uses-native-library{}:'{}'\n",
                        if *required == 0 { "-not-required" } else { "" },
                        name
                    ));
                }
            }
            Data::MetaData(data) => {
                if self.options.include_meta_data && !data.name.is_empty() {
                    printer.print(format!("meta-data: name='{}'", data.name));
                    if !data.value.is_empty() {
                        printer.print(format!(" value='{}'", data.value));
                    } else if let Some(value_int) = data.value_int {
                        printer.print(format!(" value='{value_int}'"));
                    } else if !data.resource.is_empty() {
                        printer.print(format!(" resource='{}'", data.resource));
                    } else if let Some(resource_int) = data.resource_int {
                        printer.print(format!(" resource='{resource_int}'"));
                    }
                    printer.print("\n");
                }
            }
            Data::SupportsInput { inputs } => {
                if !inputs.is_empty() {
                    printer.print("supports-input: '");
                    for input in inputs {
                        printer.print(format!("value='{input}' "));
                    }
                    printer.print("\n");
                }
            }
            Data::InstallConstraints {
                fingerprint_prefixes,
            } => {
                if !fingerprint_prefixes.is_empty() {
                    printer.print("install-constraints:\n");
                    for prefix in fingerprint_prefixes {
                        printer.print(format!("  fingerprint-prefix='{prefix}'\n"));
                    }
                }
            }
            Data::OriginalPackage { name } => {
                if let Some(name) = name {
                    printer.print(format!("original-package:'{name}'\n"));
                }
            }
            Data::Overlay(data) => {
                printer.print("overlay:");
                if let Some(target_package) = &data.target_package {
                    printer.print(format!(" targetPackage='{target_package}'"));
                }
                printer.print(format!(" priority='{}'", data.priority));
                printer.print(format!(
                    " isStatic='{}'",
                    if data.is_static { "true" } else { "false" }
                ));
                if let Some(required_property_name) = &data.required_property_name {
                    printer.print(format!(" requiredPropertyName='{required_property_name}'"));
                }
                if let Some(required_property_value) = &data.required_property_value {
                    printer.print(format!(
                        " requiredPropertyValue='{required_property_value}'"
                    ));
                }
                printer.print("\n");
            }
            Data::PackageVerifier { name, public_key } => {
                if let (Some(name), Some(public_key)) = (name, public_key) {
                    printer.print(format!(
                        "package-verifier: name='{name}' publicKey='{public_key}'\n"
                    ));
                }
            }
            Data::UsesPackage(data) => {
                if let Some(name) = &data.name {
                    if let Some(package_type) = &data.package_type {
                        printer.print(format!(
                            "uses-typed-package: type='{package_type}' name='{name}' version='{}' versionMajor='{}'",
                            data.version, data.version_major
                        ));
                        for digest in &data.cert_digests {
                            printer.print(format!(" certDigest='{digest}'"));
                        }
                        printer.print("\n");
                    } else {
                        printer.print(format!("uses-package:'{name}'\n"));
                    }
                }
            }
            Data::CompatibleScreens => {
                printer.print("compatible-screens:");
                let mut first = true;
                self.for_each_child(idx, &mut |child| {
                    if let Data::Screen { size, density } = &self.arena[child].data {
                        if first {
                            first = false;
                        } else {
                            printer.print(",");
                        }
                        if let (Some(size), Some(density)) = (size, density) {
                            printer.print(format!("'{size}/{density}'"));
                        }
                    }
                });
                printer.print("\n");
            }
            Data::SupportsGlTexture { name } => {
                if let Some(name) = name {
                    printer.print(format!("supports-gl-texture:'{name}'\n"));
                }
            }
            Data::Property(data) => {
                printer.print(format!("property: name='{}' ", data.name));
                if !data.value.is_empty() {
                    printer.print(format!("value='{}' ", data.value));
                } else if let Some(value_int) = data.value_int {
                    printer.print(format!("value='{value_int}' "));
                } else if !data.resource.is_empty() {
                    printer.print(format!("resource='{}' ", data.resource));
                } else if let Some(resource_int) = data.resource_int {
                    printer.print(format!("resource='{resource_int}' "));
                }
                printer.print("\n");
            }
            _ => {}
        }
    }

    /// Port of `Manifest::Print`/`PrintFull`.
    fn print_manifest(&self, data: &ManifestData, printer: &mut Printer) {
        if data.only_package_name {
            printer.println(format!("package: {}", data.package));
            return;
        }
        printer.print(format!("package: name='{}' ", data.package));
        printer.print(format!(
            "versionCode='{}' ",
            if data.version_code > 0 {
                data.version_code.to_string()
            } else {
                String::new()
            }
        ));
        printer.print(format!("versionName='{}'", data.version_name));
        if let Some(split) = &data.split {
            printer.print(format!(" split='{split}'"));
        }
        if let Some(platform_version_name) = &data.platform_version_name {
            printer.print(format!(
                " platformBuildVersionName='{platform_version_name}'"
            ));
        } else if let Some(platform_version_name_int) = data.platform_version_name_int {
            printer.print(format!(
                " platformBuildVersionName='{platform_version_name_int}'"
            ));
        }
        if let Some(platform_version_code) = &data.platform_version_code {
            printer.print(format!(
                " platformBuildVersionCode='{platform_version_code}'"
            ));
        } else if let Some(platform_version_code_int) = data.platform_version_code_int {
            printer.print(format!(
                " platformBuildVersionCode='{platform_version_code_int}'"
            ));
        }
        if let Some(compilesdk_version) = data.compilesdk_version {
            printer.print(format!(" compileSdkVersion='{compilesdk_version}'"));
        }
        if let Some(codename) = &data.compilesdk_version_codename {
            printer.print(format!(" compileSdkVersionCodename='{codename}'"));
        }
        printer.print("\n");

        if let Some(install_location) = data.install_location {
            match install_location {
                0 => {
                    printer.print("install-location:'auto'\n");
                }
                1 => {
                    printer.print("install-location:'internalOnly'\n");
                }
                2 => {
                    printer.print("install-location:'preferExternal'\n");
                }
                _ => {}
            }
        }
    }

    /// Port of `Application::Print`.
    fn print_application(&self, data: &ApplicationData, printer: &mut Printer) {
        for (locale, label) in &data.locale_labels {
            if locale.is_empty() {
                printer.print(format!(
                    "application-label:'{}'\n",
                    normalize_for_output(label)
                ));
            } else {
                printer.print(format!(
                    "application-label-{locale}:'{}'\n",
                    normalize_for_output(label)
                ));
            }
        }
        for (density, icon) in &data.density_icons {
            printer.print(format!("application-icon-{density}:'{icon}'\n"));
        }
        printer.print(format!(
            "application: label='{}' ",
            normalize_for_output(&data.label)
        ));
        printer.print(format!("icon='{}'", data.icon));
        if !data.banner.is_empty() {
            printer.print(format!(" banner='{}'", data.banner));
        }
        printer.print("\n");
        if data.test_only != 0 {
            printer.print(format!("testOnly='{}'\n", data.test_only));
        }
        if data.is_game != 0 {
            printer.print("application-isGame\n");
        }
        if data.debuggable != 0 {
            printer.print("application-debuggable\n");
        }
    }
}

/// Port of `DumpManifest`: extract + print. Returns the process exit
/// code contribution (0 ok, 1 error).
pub fn dump_manifest(
    apk: &LoadedApk,
    options: DumpManifestOptions,
    printer: &mut Printer,
    diag: &Diagnostics,
) -> i32 {
    let mut extractor = ManifestExtractor::new(apk, options);
    if !extractor.extract(diag) {
        return 1;
    }
    extractor.dump(printer);
    0
}
