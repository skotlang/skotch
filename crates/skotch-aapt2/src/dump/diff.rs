//! `aapt2 diff`: port of `cmd/Diff.cpp`. Compares the resource tables
//! of two APKs and prints the differences to stderr.

use super::values::value_print_string;
use crate::apk::LoadedApk;
use crate::res::config::ConfigDescription;
use crate::res::table::{
    ResourceConfigValue, ResourceEntry, ResourceTablePackage, ResourceTableType, Visibility,
    VisibilityLevel,
};
use crate::res::value::{Item, Reference, Value, ValueKind};
use crate::res::{FeatureFlagAttribute, APP_PACKAGE_ID};

fn emit_diff_line(source: &str, message: &str) {
    eprintln!("{source}: {message}");
}

fn is_symbol_visibility_different(a: &Visibility, b: &Visibility) -> bool {
    a.level != b.level || a.staged_api != b.staged_api
}

fn is_id_diff<T: PartialEq + Copy>(
    level_a: VisibilityLevel,
    id_a: Option<T>,
    level_b: VisibilityLevel,
    id_b: Option<T>,
) -> bool {
    if level_a == VisibilityLevel::Public || level_b == VisibilityLevel::Public {
        return id_a != id_b;
    }
    false
}

fn visibility_string(visibility: &Visibility) -> String {
    let mut out = String::new();
    if visibility.staged_api {
        out.push_str("STAGED ");
    }
    if visibility.level == VisibilityLevel::Public {
        out.push_str("PUBLIC");
    } else {
        out.push_str("PRIVATE");
    }
    out
}

fn value_flag(value: &ResourceConfigValue) -> Option<&FeatureFlagAttribute> {
    value.value.as_ref().and_then(|v| v.meta.flag.as_ref())
}

fn find_flag_disabled_value<'e>(
    entry: &'e ResourceEntry,
    flag: Option<&FeatureFlagAttribute>,
    config: &ConfigDescription,
) -> Option<&'e ResourceConfigValue> {
    entry
        .flag_disabled_values
        .iter()
        .find(|v| v.config == *config && value_flag(v) == flag)
}

fn emit_resource_config_value_diff(
    apk_a: &LoadedApk,
    pkg_a: &ResourceTablePackage,
    type_a: &ResourceTableType,
    entry_a: &ResourceEntry,
    config_value_a: &ResourceConfigValue,
    apk_b: &LoadedApk,
    config_value_b: &ResourceConfigValue,
) -> bool {
    let _ = apk_a;
    let (Some(value_a), Some(value_b)) = (&config_value_a.value, &config_value_b.value) else {
        return false;
    };
    if !value_a.equals(value_b) {
        let message = format!(
            "value {}:{}/{} config='{}' does not match:\n{}\n vs \n{}",
            pkg_a.name,
            type_a.named_type,
            entry_a.name,
            config_value_a.config,
            value_print_string(value_a),
            value_print_string(value_b)
        );
        emit_diff_line(&apk_b.source, &message);
        return true;
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn emit_resource_entry_diff(
    apk_a: &LoadedApk,
    pkg_a: &ResourceTablePackage,
    type_a: &ResourceTableType,
    entry_a: &ResourceEntry,
    apk_b: &LoadedApk,
    pkg_b: &ResourceTablePackage,
    type_b: &ResourceTableType,
    entry_b: &ResourceEntry,
) -> bool {
    let _ = (pkg_b, type_b);
    let mut diff = false;
    for config_value_a in &entry_a.values {
        match entry_b.find_value(&config_value_a.config, &config_value_a.product) {
            None => {
                emit_diff_line(
                    &apk_b.source,
                    &format!(
                        "missing {}:{}/{} config={}",
                        pkg_a.name, type_a.named_type, entry_a.name, config_value_a.config
                    ),
                );
                diff = true;
            }
            Some(config_value_b) => {
                diff |= emit_resource_config_value_diff(
                    apk_a,
                    pkg_a,
                    type_a,
                    entry_a,
                    config_value_a,
                    apk_b,
                    config_value_b,
                );
            }
        }
    }
    for config_value_a in &entry_a.flag_disabled_values {
        match find_flag_disabled_value(entry_b, value_flag(config_value_a), &config_value_a.config)
        {
            None => {
                emit_diff_line(
                    &apk_b.source,
                    &format!(
                        "missing disabled value {}:{}/{} config={} flag={}",
                        pkg_a.name,
                        type_a.named_type,
                        entry_a.name,
                        config_value_a.config,
                        value_flag(config_value_a)
                            .map(|f| f.to_string())
                            .unwrap_or_default()
                    ),
                );
                diff = true;
            }
            Some(config_value_b) => {
                diff |= emit_resource_config_value_diff(
                    apk_a,
                    pkg_a,
                    type_a,
                    entry_a,
                    config_value_a,
                    apk_b,
                    config_value_b,
                );
            }
        }
    }

    // Check for any newly added config values.
    for config_value_b in &entry_b.values {
        if entry_a
            .find_value(&config_value_b.config, &config_value_b.product)
            .is_none()
        {
            emit_diff_line(
                &apk_b.source,
                &format!(
                    "new config {}:{}/{} config={}",
                    pkg_b.name, type_b.named_type, entry_b.name, config_value_b.config
                ),
            );
            diff = true;
        }
    }
    for config_value_b in &entry_b.flag_disabled_values {
        if find_flag_disabled_value(entry_a, value_flag(config_value_b), &config_value_b.config)
            .is_none()
        {
            emit_diff_line(
                &apk_b.source,
                &format!(
                    "new disabled config {}:{}/{} config={} flag={}",
                    pkg_b.name,
                    type_b.named_type,
                    entry_b.name,
                    config_value_b.config,
                    value_flag(config_value_b)
                        .map(|f| f.to_string())
                        .unwrap_or_default()
                ),
            );
            diff = true;
        }
    }
    diff
}

fn emit_resource_type_diff(
    apk_a: &LoadedApk,
    pkg_a: &ResourceTablePackage,
    type_a: &ResourceTableType,
    entries_a: &[&ResourceEntry],
    apk_b: &LoadedApk,
    pkg_b: &ResourceTablePackage,
    type_b: &ResourceTableType,
    entries_b: &[&ResourceEntry],
) -> bool {
    let mut diff = false;
    let mut iter_a = entries_a.iter();
    let mut iter_b = entries_b.iter();
    let mut entry_a = iter_a.next();
    let mut entry_b = iter_b.next();
    while entry_a.is_some() || entry_b.is_some() {
        match (entry_a, entry_b) {
            (Some(a), None) => {
                emit_diff_line(
                    &apk_a.source,
                    &format!("missing {}:{}/{}", pkg_a.name, type_a.named_type, a.name),
                );
                diff = true;
            }
            (None, Some(b)) => {
                emit_diff_line(
                    &apk_b.source,
                    &format!("new entry {}:{}/{}", pkg_b.name, type_b.named_type, b.name),
                );
                diff = true;
            }
            (Some(a), Some(b)) => {
                if is_symbol_visibility_different(&a.visibility, &b.visibility) {
                    emit_diff_line(
                        &apk_b.source,
                        &format!(
                            "{}:{}/{} has different visibility ({} vs {})",
                            pkg_a.name,
                            type_a.named_type,
                            a.name,
                            visibility_string(&b.visibility),
                            visibility_string(&a.visibility)
                        ),
                    );
                    diff = true;
                } else if is_id_diff(
                    a.visibility.level,
                    a.id,
                    b.visibility.level,
                    b.id,
                ) {
                    let id_string = |id: Option<crate::res::ResourceId>| match id {
                        Some(id) => format!("0x{:x}", id.0),
                        None => "none".to_string(),
                    };
                    emit_diff_line(
                        &apk_b.source,
                        &format!(
                            "{}:{}/{} has different public ID ({} vs {})",
                            pkg_a.name,
                            type_a.named_type,
                            a.name,
                            id_string(b.id),
                            id_string(a.id)
                        ),
                    );
                    diff = true;
                }
                diff |= emit_resource_entry_diff(
                    apk_a, pkg_a, type_a, a, apk_b, pkg_b, type_b, b,
                );
            }
            (None, None) => unreachable!(),
        }
        if entry_a.is_some() {
            entry_a = iter_a.next();
        }
        if entry_b.is_some() {
            entry_b = iter_b.next();
        }
    }
    diff
}

type SortedPackage<'t> = (
    &'t ResourceTablePackage,
    Vec<(&'t ResourceTableType, Vec<&'t ResourceEntry>)>,
);

fn emit_resource_package_diff(
    apk_a: &LoadedApk,
    pkg_a: &SortedPackage<'_>,
    apk_b: &LoadedApk,
    pkg_b: &SortedPackage<'_>,
) -> bool {
    let mut diff = false;
    let mut iter_a = pkg_a.1.iter();
    let mut iter_b = pkg_b.1.iter();
    let mut type_a = iter_a.next();
    let mut type_b = iter_b.next();
    while type_a.is_some() || type_b.is_some() {
        match (type_a, type_b) {
            (Some((ta, _)), None) => {
                emit_diff_line(
                    &apk_a.source,
                    &format!("missing {}:{}", pkg_a.0.name, ta.named_type),
                );
                diff = true;
            }
            (None, Some((tb, _))) => {
                emit_diff_line(
                    &apk_b.source,
                    &format!("new type {}:{}", pkg_b.0.name, tb.named_type),
                );
                diff = true;
            }
            (Some((ta, entries_a)), Some((tb, entries_b))) => {
                if ta.visibility_level != tb.visibility_level {
                    let level_string = |level: VisibilityLevel| {
                        if level == VisibilityLevel::Public {
                            "PUBLIC"
                        } else {
                            "PRIVATE"
                        }
                    };
                    emit_diff_line(
                        &apk_b.source,
                        &format!(
                            "{}:{} has different visibility ({} vs {})",
                            pkg_a.0.name,
                            ta.named_type,
                            level_string(tb.visibility_level),
                            level_string(ta.visibility_level)
                        ),
                    );
                    diff = true;
                }
                diff |= emit_resource_type_diff(
                    apk_a, pkg_a.0, ta, entries_a, apk_b, pkg_b.0, tb, entries_b,
                );
            }
            (None, None) => unreachable!(),
        }
        if type_a.is_some() {
            type_a = iter_a.next();
        }
        if type_b.is_some() {
            type_b = iter_b.next();
        }
    }
    diff
}

fn emit_resource_table_diff(apk_a: &LoadedApk, apk_b: &LoadedApk) -> bool {
    let table_a = apk_a.table.sorted_view();
    let table_b = apk_b.table.sorted_view();

    let mut diff = false;
    let mut iter_a = table_a.iter();
    let mut iter_b = table_b.iter();
    let mut package_a = iter_a.next();
    let mut package_b = iter_b.next();
    while package_a.is_some() || package_b.is_some() {
        match (package_a, package_b) {
            (Some((pa, _)), None) => {
                emit_diff_line(&apk_a.source, &format!("missing package {}", pa.name));
                diff = true;
            }
            (None, Some((pb, _))) => {
                emit_diff_line(&apk_b.source, &format!("new package {}", pb.name));
                diff = true;
            }
            (Some(pa), Some(pb)) => {
                diff |= emit_resource_package_diff(apk_a, pa, apk_b, pb);
            }
            (None, None) => unreachable!(),
        }
        if package_a.is_some() {
            package_a = iter_a.next();
        }
        if package_b.is_some() {
            package_b = iter_b.next();
        }
    }
    diff
}

fn zero_out_reference(reference: &mut Reference) {
    if reference.name.is_some() {
        if let Some(id) = reference.id {
            if id.package_id() == APP_PACKAGE_ID {
                reference.id = None;
            }
        }
    }
}

fn zero_out_item(item: &mut Item) {
    if let Item::Reference(reference) = item {
        zero_out_reference(reference);
    }
}

fn zero_out_value(value: &mut Value) {
    match &mut value.kind {
        ValueKind::Item(item) => zero_out_item(item),
        ValueKind::Attribute(attr) => {
            for symbol in &mut attr.symbols {
                zero_out_reference(&mut symbol.symbol);
            }
        }
        ValueKind::Style(style) => {
            if let Some(parent) = &mut style.parent {
                zero_out_reference(parent);
            }
            for entry in &mut style.entries {
                zero_out_reference(&mut entry.key);
                zero_out_item(&mut entry.value.item);
            }
        }
        ValueKind::Styleable(styleable) => {
            for entry in &mut styleable.entries {
                zero_out_reference(entry);
            }
        }
        ValueKind::Array(array) => {
            for element in &mut array.elements {
                zero_out_item(&mut element.item);
            }
        }
        ValueKind::Plural(plural) => {
            for value in plural.values.iter_mut().flatten() {
                zero_out_item(&mut value.item);
            }
        }
        ValueKind::Macro(_) => {}
    }
}

/// Port of `ZeroOutAppReferences`: removes app-package (0x7f) IDs from
/// named references so tables linked with different ID assignments
/// still compare equal.
fn zero_out_app_references(apk: &mut LoadedApk) {
    for package in &mut apk.table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                for config_value in entry
                    .values
                    .iter_mut()
                    .chain(entry.flag_disabled_values.iter_mut())
                {
                    if let Some(value) = &mut config_value.value {
                        zero_out_value(value);
                    }
                }
            }
        }
    }
}

/// Port of `DiffCommand::Action`.
pub fn diff_apks(mut apk_a: LoadedApk, mut apk_b: LoadedApk) -> i32 {
    zero_out_app_references(&mut apk_a);
    zero_out_app_references(&mut apk_b);
    if emit_resource_table_diff(&apk_a, &apk_b) {
        // A diff was emitted: return 1 (failure).
        return 1;
    }
    0
}
