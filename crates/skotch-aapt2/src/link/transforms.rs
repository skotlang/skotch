//! Table transformation passes run during linking.
//!
//! Ports of `link/NoDefaultResourceRemover`, `link/PrivateAttributeMover`,
//! `link/AutoVersioner` (Linkers.h), `link/VersionCollapser`,
//! `link/ResourceExcluder`, `optimize/ResourceDeduper`, and
//! `process/ProductFilter` (keep-default mode).

use crate::diag::Diagnostics;
use crate::res::config::ConfigDescription;
use crate::res::table::{ResourceTable, ResourceTableType, VisibilityLevel};
use crate::res::value::ValueKind;
use crate::res::{ResourceNamedType, ResourceType};
use anyhow::{bail, Result};

/// Removes resources that have no value for the default configuration
/// (referencing them would crash at runtime). Resources where all
/// configs carry an SDK version qualifier are kept (they're versioned
/// alternatives). Mirrors `NoDefaultResourceRemover`.
pub fn remove_no_default_resources(table: &mut ResourceTable, diag: &Diagnostics) -> Result<()> {
    for package in &mut table.packages {
        for ty in &mut package.types {
            // IDs are sentinels and styleables don't end up in the
            // table; both are exempt (mirroring KeepResource).
            if matches!(ty.named_type.ty, ResourceType::Id | ResourceType::Styleable) {
                continue;
            }
            ty.entries.retain(|entry| {
                if entry.visibility.level == VisibilityLevel::Public {
                    return true;
                }
                if entry.values.is_empty() {
                    // Entry-only declarations (visibility etc.) stay.
                    return true;
                }
                if entry.has_default_value() {
                    return true;
                }
                // Keep entries whose configs are all version-qualified
                // or density-qualified variants of one another.
                let keep = entry.values.iter().all(|cv| {
                    let mut without_version = cv.config;
                    without_version.sdk_version = 0;
                    without_version.density = 0;
                    without_version.is_default()
                });
                if !keep {
                    diag.warn(format!(
                        "removing resource {} without required default value",
                        entry.name
                    ));
                }
                keep
            });
        }
    }
    Ok(())
}

/// Moves `attr` entries with non-public visibility into the
/// `^attr-private` type (framework builds only). Mirrors
/// `PrivateAttributeMover`.
pub fn move_private_attrs(table: &mut ResourceTable) -> Result<()> {
    for package in &mut table.packages {
        let Some(attr_index) = package
            .types
            .iter()
            .position(|t| t.named_type.ty == ResourceType::Attr)
        else {
            continue;
        };
        if package.types[attr_index].visibility_level != VisibilityLevel::Public {
            continue;
        }
        let (public, private): (Vec<_>, Vec<_>) = package.types[attr_index]
            .entries
            .drain(..)
            .partition(|e| e.visibility.level == VisibilityLevel::Public);
        package.types[attr_index].entries = public;
        if !private.is_empty() {
            let named_type = ResourceNamedType::with_default_name(ResourceType::AttrPrivate);
            let private_type = match package
                .types
                .iter()
                .position(|t| t.named_type == named_type)
            {
                Some(i) => &mut package.types[i],
                None => {
                    package.types.push(ResourceTableType::new(named_type));
                    package.types.last_mut().unwrap()
                }
            };
            private_type.entries.extend(private);
        }
    }
    Ok(())
}

/// Filters values by product, keeping the best match per (entry,
/// config). Mirrors `ProductFilter` with
/// `remove_default_config_values = false`. An empty product list keeps
/// "default" products only.
pub fn filter_products(
    table: &mut ResourceTable,
    products: &[String],
    diag: &Diagnostics,
) -> Result<()> {
    let mut error = false;
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                let values = std::mem::take(&mut entry.values);
                let mut kept = Vec::with_capacity(values.len());

                let mut index = 0;
                while index < values.len() {
                    // Find the run of values with the same config.
                    let mut end = index + 1;
                    while end < values.len() && values[end].config == values[index].config {
                        end += 1;
                    }
                    let group = &values[index..end];

                    let mut selected: Option<usize> = None;
                    let mut default_value: Option<usize> = None;
                    let mut ambiguous = false;
                    for (offset, value) in group.iter().enumerate() {
                        if products.contains(&value.product) {
                            if selected.is_some() {
                                diag.error(format!(
                                    "selection of product '{}' for resource {} is ambiguous",
                                    value.product, entry.name
                                ));
                                ambiguous = true;
                            } else {
                                selected = Some(offset);
                            }
                        }
                        if value.product.is_empty() || value.product == "default" {
                            if default_value.is_some() {
                                diag.error(format!(
                                    "multiple default products defined for resource {}",
                                    entry.name
                                ));
                                ambiguous = true;
                            } else {
                                default_value = Some(offset);
                            }
                        }
                    }
                    if ambiguous {
                        error = true;
                    } else if let Some(offset) = selected.or(default_value) {
                        let mut value = group[offset].clone();
                        value.product = String::new();
                        kept.push(value);
                    } else {
                        diag.error(format!(
                            "no default product defined for resource {}",
                            entry.name
                        ));
                        error = true;
                    }
                    index = end;
                }
                entry.values = kept;
            }
        }
    }
    if error {
        bail!("failed stripping products");
    }
    Ok(())
}

/// Automatic SDK versioning of styles: when a style defined below SDK L
/// uses attributes introduced later, a copy is created under the higher
/// `-vN` config. Mirrors `AutoVersioner::Consume`.
///
/// Note: the attribute→SDK-level table (`SdkConstants.cpp`) drives which
/// entries trigger copies; references with IDs in the framework range
/// are checked against [`crate::res::utils`]'s SDK constants.
pub fn auto_version(table: &mut ResourceTable) -> Result<()> {
    use crate::res::config::SDK_LOLLIPOP_MR1;
    for package in &mut table.packages {
        for ty in &mut package.types {
            if ty.named_type.ty != ResourceType::Style {
                continue;
            }
            for entry in &mut ty.entries {
                let mut new_values = Vec::new();
                for config_value in &entry.values {
                    if config_value.config.sdk_version >= SDK_LOLLIPOP_MR1 {
                        // Beyond L MR1 the runtime handles missing attrs.
                        continue;
                    }
                    let Some(value) = &config_value.value else {
                        continue;
                    };
                    let ValueKind::Style(style) = &value.kind else {
                        continue;
                    };

                    // The minimum SDK any used attribute requires.
                    let mut needed_sdk = 0u16;
                    for style_entry in &style.entries {
                        if let Some(id) = style_entry.key.id {
                            let attr_sdk = attr_id_to_sdk_level(id.0);
                            needed_sdk = needed_sdk.max(attr_sdk);
                        }
                    }
                    if needed_sdk > config_value.config.sdk_version
                        && needed_sdk > 1
                        && config_value.config.sdk_version < needed_sdk
                    {
                        // Only add when no value already exists for the
                        // versioned config.
                        let mut versioned_config = config_value.config;
                        versioned_config.sdk_version = needed_sdk;
                        if entry
                            .find_value(&versioned_config, &config_value.product)
                            .is_none()
                        {
                            let mut copy = config_value.clone();
                            copy.config = versioned_config;
                            new_values.push(copy);
                        }
                    }
                }
                for value in new_values {
                    let slot = entry.find_or_create_value(&value.config, &value.product);
                    if slot.value.is_none() {
                        slot.value = value.value;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Maps a framework attribute resource ID to the SDK level it was
/// introduced in. Port of `FindAttributeSdkLevel` over the
/// `sAttrIdMap` table in `SdkConstants.cpp` (bucketed by entry ID).
pub fn attr_id_to_sdk_level(attr_id: u32) -> u16 {
    if attr_id & 0xffff_0000 != 0x0101_0000 {
        // Not a framework attribute.
        return 0;
    }
    let entry = (attr_id & 0xffff) as u16;
    // (max entry id, sdk) pairs from SdkConstants.cpp.
    const MAP: &[(u16, u16)] = &[
        (0x021c, 1),
        (0x021d, 2),
        (0x0269, 3),
        (0x028d, 4),
        (0x02ad, 5),
        (0x02b3, 6),
        (0x02b5, 7),
        (0x02bd, 8),
        (0x02cb, 9),
        (0x0361, 11),
        (0x0366, 12),
        (0x03a6, 13),
        (0x03ae, 14),
        (0x03cc, 15),
        (0x03da, 16),
        (0x03f1, 17),
        (0x03f6, 18),
        (0x0419, 19),
        (0x041a, 20),
        (0x0437, 21),
        (0x044d, 22),
        (0x0469, 23),
        (0x0480, 24),
        (0x0482, 25),
        (0x04a5, 26),
        (0x04ad, 27),
        (0x04d6, 28),
        (0x0510, 29),
        (0x0530, 30),
    ];
    for &(max_entry, sdk) in MAP {
        if entry <= max_entry {
            return sdk;
        }
    }
    // Anything newer maps to the latest known level.
    31
}

/// Collapses versioned configs that can never be selected because the
/// app's minSdkVersion already guarantees a newer variant. Mirrors
/// `VersionCollapser`.
pub fn collapse_versions(table: &mut ResourceTable, min_sdk: i32) -> Result<()> {
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                // Group by config-without-version; in each group keep
                // the highest version <= minSdk plus everything above.
                let values = std::mem::take(&mut entry.values);
                let mut kept: Vec<crate::res::table::ResourceConfigValue> = Vec::new();
                let mut groups: Vec<(
                    ConfigDescription,
                    Vec<crate::res::table::ResourceConfigValue>,
                )> = Vec::new();
                for value in values {
                    let mut key = value.config;
                    key.sdk_version = 0;
                    match groups.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, group)) => group.push(value),
                        None => groups.push((key, vec![value])),
                    }
                }
                for (_, group) in &mut groups {
                    // Highest version still <= minSdk wins the "base" slot.
                    let mut best_le: Option<usize> = None;
                    for (i, value) in group.iter().enumerate() {
                        let v = value.config.sdk_version as i32;
                        if v <= min_sdk {
                            best_le = match best_le {
                                Some(b) if group[b].config.sdk_version as i32 >= v => Some(b),
                                _ => Some(i),
                            };
                        }
                    }
                    for (i, mut value) in std::mem::take(group).into_iter().enumerate() {
                        let v = value.config.sdk_version as i32;
                        if v > min_sdk {
                            kept.push(value);
                        } else if Some(i) == best_le {
                            // Strip the now-redundant version qualifier
                            // (matching aapt2, which keeps the config
                            // qualifier only when it was != 0 already).
                            if value.config.sdk_version as i32 <= min_sdk {
                                value.config.sdk_version = 0;
                            }
                            kept.push(value);
                        }
                        // else: dropped — dominated by best_le.
                    }
                }
                kept.sort_by(|a, b| {
                    a.config
                        .cmp(&b.config)
                        .then_with(|| a.product.cmp(&b.product))
                });
                // Collapsing version qualifiers can produce duplicate
                // configs; keep the last (highest original version).
                kept.dedup_by(|b, a| {
                    if a.config == b.config && a.product == b.product {
                        a.value = b.value.take();
                        true
                    } else {
                        false
                    }
                });
                entry.values = kept;
            }
        }
    }
    Ok(())
}

/// Removes values whose config contains all the qualifiers of any
/// excluded config (`--exclude-configs`). Mirrors `ResourceExcluder`:
/// a value matches when, on every axis the excluded config sets, the
/// value's config does not differ from it.
pub fn exclude_configs(table: &mut ResourceTable, excluded: &[ConfigDescription]) -> Result<()> {
    let default_config = ConfigDescription::default();
    // Pre-compute each excluded config's set axes.
    let targets: Vec<(&ConfigDescription, u32)> = excluded
        .iter()
        .map(|config| (config, config.diff(&default_config)))
        .collect();
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                entry.values.retain(|value| {
                    !targets.iter().any(|(target, target_axes)| {
                        *target_axes != 0 && (value.config.diff(target) & target_axes) == 0
                    })
                });
            }
        }
    }
    Ok(())
}

/// Removes duplicated values in dominated configurations. A value is a
/// duplicate when an equal value exists in a config that dominates it.
/// Mirrors `ResourceDeduper` (simplified to direct dominance pairs).
pub fn dedupe_resources(table: &mut ResourceTable) -> Result<()> {
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                let mut remove = vec![false; entry.values.len()];
                for i in 0..entry.values.len() {
                    for j in 0..entry.values.len() {
                        if i == j || remove[j] || remove[i] {
                            continue;
                        }
                        let (dominator, candidate) = (&entry.values[i], &entry.values[j]);
                        if dominator.product != candidate.product {
                            continue;
                        }
                        if !dominator.config.dominates(&candidate.config) {
                            continue;
                        }
                        if dominator.config == candidate.config {
                            continue;
                        }
                        match (&dominator.value, &candidate.value) {
                            (Some(a), Some(b)) if a.equals(b) => remove[j] = true,
                            _ => {}
                        }
                    }
                }
                let mut keep_iter = remove.iter();
                entry.values.retain(|_| !*keep_iter.next().unwrap());
            }
        }
    }
    Ok(())
}

/// Options for [`filter_feature_flags`]. Mirrors
/// `FeatureFlagsFilterOptions`.
#[derive(Debug, Clone, Copy)]
pub struct FeatureFlagsFilterOptions {
    /// Remove elements whose `android:featureFlag` evaluates disabled.
    pub remove_disabled_elements: bool,
    pub fail_on_unrecognized_flags: bool,
    pub flags_must_have_value: bool,
    pub flags_must_be_readonly: bool,
}

impl Default for FeatureFlagsFilterOptions {
    fn default() -> Self {
        FeatureFlagsFilterOptions {
            remove_disabled_elements: true,
            fail_on_unrecognized_flags: true,
            flags_must_have_value: true,
            flags_must_be_readonly: false,
        }
    }
}

/// Walks an XML document removing elements behind disabled
/// `android:featureFlag` attributes and validating flag usage.
/// Port of `link/FeatureFlagsFilter.cpp`.
pub fn filter_feature_flags(
    root: &mut crate::xml::Element,
    feature_flags: &crate::compile::FeatureFlagValues,
    options: &FeatureFlagsFilterOptions,
    diag: &Diagnostics,
) -> Result<()> {
    let mut has_error = false;
    filter_element(root, feature_flags, options, diag, &mut has_error);
    if has_error {
        bail!("feature flag validation failed");
    }
    Ok(())
}

fn filter_element(
    element: &mut crate::xml::Element,
    feature_flags: &crate::compile::FeatureFlagValues,
    options: &FeatureFlagsFilterOptions,
    diag: &Diagnostics,
    has_error: &mut bool,
) {
    element.children.retain(|child| {
        let crate::xml::Node::Element(el) = child else {
            return true;
        };
        let Some(attr) = el.find_attribute(crate::xml::SCHEMA_ANDROID, "featureFlag") else {
            return true;
        };
        let mut flag_name = crate::util::trim_whitespace(&attr.value);
        let mut negated = false;
        if let Some(rest) = flag_name.strip_prefix('!') {
            negated = true;
            flag_name = rest;
        }
        match feature_flags.get(flag_name) {
            Some(properties) => {
                if let Some(enabled) = properties.enabled {
                    if options.flags_must_be_readonly && !properties.read_only {
                        diag.error(format!(
                            "attribute 'android:featureFlag' has flag '{flag_name}' which must \
                             be readonly but is not"
                        ));
                        *has_error = true;
                        return true;
                    }
                    if options.remove_disabled_elements {
                        // Remove when flag==true && attr=="!flag" OR
                        // flag==false && attr=="flag".
                        return enabled != negated;
                    }
                } else if options.flags_must_have_value {
                    diag.error(format!(
                        "attribute 'android:featureFlag' has flag '{flag_name}' without a \
                         true/false value from --feature_flags parameter"
                    ));
                    *has_error = true;
                }
            }
            None => {
                if options.fail_on_unrecognized_flags {
                    diag.error(format!(
                        "attribute 'android:featureFlag' has flag '{flag_name}' not found in \
                         flags from --feature_flags parameter"
                    ));
                    *has_error = true;
                }
            }
        }
        true
    });
    for child in element.child_elements_mut() {
        filter_element(child, feature_flags, options, diag, has_error);
    }
}

/// Removes resources that exist only behind disabled feature flags, and
/// blanks any flag-disabled strings that remain. Mirrors
/// `FlagDisabledResourceRemover` plus the `FlagDisabledStringVisitor`
/// in `Link.cpp::WriteApk`.
///
/// In this model disabled values are segregated into
/// `flag_disabled_values` rather than mixed into `values` with a status
/// (as in C++), so the C++ rule "remove the entry when every value is
/// disabled" becomes "remove the entry when `values` is empty and
/// `flag_disabled_values` is not".
pub fn remove_flag_disabled(table: &mut ResourceTable) -> Result<()> {
    use crate::res::FlagStatus;
    for package in &mut table.packages {
        for ty in &mut package.types {
            ty.entries
                .retain(|entry| !entry.values.is_empty() || entry.flag_disabled_values.is_empty());
            for entry in &mut ty.entries {
                // The final table carries no disabled payloads.
                entry.flag_disabled_values.clear();
                // Defensive: blank disabled strings that sit in `values`
                // (can occur for values merged from foreign tables).
                for config_value in &mut entry.values {
                    if let Some(value) = &mut config_value.value {
                        if value.meta.flag_status == FlagStatus::Disabled {
                            if let ValueKind::Item(item) = &mut value.kind {
                                match item {
                                    crate::res::value::Item::String { value: s, .. }
                                    | crate::res::value::Item::RawString(s) => s.clear(),
                                    crate::res::value::Item::StyledString {
                                        value: s,
                                        spans,
                                        ..
                                    } => {
                                        s.clear();
                                        spans.clear();
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::res::value::{Item, Value};
    use crate::res::{ResourceName, ResourceType};

    fn string_value(s: &str) -> Value {
        Value::item(Item::String {
            value: s.to_string(),
            untranslatable_sections: vec![],
        })
    }

    #[test]
    fn dedupe_dominated_identical() {
        // Locale axes are explicitly excluded from dominance (aapt2
        // does not dedupe across locales), so use orientation.
        let mut table = ResourceTable::new();
        let name = ResourceName::new("app", ResourceType::String, "x");
        table
            .add_value(
                name.clone(),
                ConfigDescription::default(),
                string_value("same"),
            )
            .unwrap();
        table
            .add_value(
                name.clone(),
                ConfigDescription::parse("land").unwrap(),
                string_value("same"),
            )
            .unwrap();
        table
            .add_value(
                name.clone(),
                ConfigDescription::parse("port").unwrap(),
                string_value("different"),
            )
            .unwrap();
        dedupe_resources(&mut table).unwrap();
        let entry = table.find_resource(&name).unwrap().entry;
        assert_eq!(
            entry.values.len(),
            2,
            "{:?}",
            entry
                .values
                .iter()
                .map(|v| v.config.to_string())
                .collect::<Vec<_>>()
        );
        assert!(entry.values.iter().all(|v| v.config.to_string() != "land"));
    }

    #[test]
    fn collapse_drops_dominated_versions() {
        let mut table = ResourceTable::new();
        let name = ResourceName::new("app", ResourceType::String, "x");
        table
            .add_value(
                name.clone(),
                ConfigDescription::default(),
                string_value("base"),
            )
            .unwrap();
        table
            .add_value(
                name.clone(),
                ConfigDescription::parse("v4").unwrap(),
                string_value("v4"),
            )
            .unwrap();
        table
            .add_value(
                name.clone(),
                ConfigDescription::parse("v21").unwrap(),
                string_value("v21"),
            )
            .unwrap();
        collapse_versions(&mut table, 21).unwrap();
        let entry = table.find_resource(&name).unwrap().entry;
        // v21 wins the base slot; default and v4 die.
        assert_eq!(entry.values.len(), 1);
        assert!(entry.values[0].config.is_default());
    }

    #[test]
    fn no_default_removal() {
        let mut table = ResourceTable::new();
        let with_default = ResourceName::new("app", ResourceType::String, "ok");
        table
            .add_value(
                with_default.clone(),
                ConfigDescription::default(),
                string_value("x"),
            )
            .unwrap();
        let without_default = ResourceName::new("app", ResourceType::String, "bad");
        table
            .add_value(
                without_default.clone(),
                ConfigDescription::parse("de").unwrap(),
                string_value("nur deutsch"),
            )
            .unwrap();
        let diag = Diagnostics::collecting();
        remove_no_default_resources(&mut table, &diag).unwrap();
        assert!(table.find_resource(&with_default).is_some());
        assert!(table.find_resource(&without_default).is_none());
    }
}
