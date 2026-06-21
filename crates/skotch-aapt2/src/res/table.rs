//! In-memory resource table.
//!
//! Port of aapt2's `ResourceTable.h`/`ResourceTable.cpp`: the central
//! data structure produced by `compile`, merged/linked by `link`, and
//! flattened to `resources.arsc` or proto.

use super::config::ConfigDescription;
use super::value::{Value, ValueKind};
use super::{
    FeatureFlagAttribute, FlagStatus, ResourceId, ResourceName, ResourceNamedType, ResourceType,
    Source,
};
use std::fmt;

/// Visibility of a resource entry outside its package.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VisibilityLevel {
    /// Not specified; treated as private but omitted from both the public
    /// and private R.java when split generation is used.
    #[default]
    Undefined,
    /// Explicitly `private` (java-symbol).
    Private,
    /// Explicitly `<public>`.
    Public,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Visibility {
    pub level: VisibilityLevel,
    pub source: Source,
    pub comment: String,
    /// Set for resources in `<staging-public-group>`: the R.java field
    /// must not be final since the ID may change between builds.
    pub staged_api: bool,
}

/// Represents `<add-resource>` in an overlay.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AllowNew {
    pub source: Source,
    pub comment: String,
}

/// The staged (finalized) resource ID of a resource.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StagedId {
    pub id: ResourceId,
    pub source: Source2,
}

// StagedId keeps a tiny copyable source to stay `Copy`; the full path
// lives in the entry's visibility/source when needed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Source2;

/// An `<overlayable>` declaration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overlayable {
    pub name: String,
    pub actor: String,
    pub source: Source,
}

impl Overlayable {
    pub const ACTOR_SCHEME: &'static str = "overlay";
    pub const ACTOR_SCHEME_URI: &'static str = "overlay://";
}

/// Runtime-resource-overlay policy flags, matching
/// `android::ResTable_overlayable_policy_header::PolicyFlags`.
pub mod policy {
    pub const NONE: u32 = 0;
    pub const PUBLIC: u32 = 0x0000_0001;
    pub const SYSTEM_PARTITION: u32 = 0x0000_0002;
    pub const VENDOR_PARTITION: u32 = 0x0000_0004;
    pub const PRODUCT_PARTITION: u32 = 0x0000_0008;
    pub const SIGNATURE: u32 = 0x0000_0010;
    pub const ODM_PARTITION: u32 = 0x0000_0020;
    pub const OEM_PARTITION: u32 = 0x0000_0040;
    pub const ACTOR_SIGNATURE: u32 = 0x0000_0080;
    pub const CONFIG_SIGNATURE: u32 = 0x0000_0100;
}

/// Declares one resource as overlayable, with the policies from its
/// enclosing `<policy>` tag.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverlayableItem {
    /// Index into [`ResourceTable::overlayables`].
    pub overlayable_index: usize,
    pub policies: u32,
    pub comment: String,
    pub source: Source,
}

/// A value defined for a particular configuration (and product).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceConfigValue {
    pub config: ConfigDescription,
    pub product: String,
    pub value: Option<Value>,
}

impl ResourceConfigValue {
    pub fn new(config: ConfigDescription, product: impl Into<String>) -> Self {
        ResourceConfigValue {
            config,
            product: product.into(),
            value: None,
        }
    }
}

/// A resource entry: one `name` within a type, holding per-config values.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResourceEntry {
    pub name: String,
    /// The full resource ID, if assigned.
    pub id: Option<ResourceId>,
    pub visibility: Visibility,
    pub allow_new: Option<AllowNew>,
    pub overlayable_item: Option<OverlayableItem>,
    pub staged_id: Option<StagedId>,
    /// Values sorted by (config, product).
    pub values: Vec<ResourceConfigValue>,
    /// Values behind disabled feature flags (kept for `--feature-flags`
    /// aware linking).
    pub flag_disabled_values: Vec<ResourceConfigValue>,
}

impl ResourceEntry {
    pub fn new(name: impl Into<String>) -> Self {
        ResourceEntry {
            name: name.into(),
            ..Default::default()
        }
    }

    fn position(&self, config: &ConfigDescription, product: &str) -> Result<usize, usize> {
        self.values.binary_search_by(|v| {
            v.config
                .cmp(config)
                .then_with(|| v.product.as_str().cmp(product))
        })
    }

    pub fn find_value(
        &self,
        config: &ConfigDescription,
        product: &str,
    ) -> Option<&ResourceConfigValue> {
        self.position(config, product).ok().map(|i| &self.values[i])
    }

    pub fn find_value_mut(
        &mut self,
        config: &ConfigDescription,
        product: &str,
    ) -> Option<&mut ResourceConfigValue> {
        self.position(config, product)
            .ok()
            .map(move |i| &mut self.values[i])
    }

    pub fn find_or_create_value(
        &mut self,
        config: &ConfigDescription,
        product: &str,
    ) -> &mut ResourceConfigValue {
        let index = match self.position(config, product) {
            Ok(i) => i,
            Err(i) => {
                self.values
                    .insert(i, ResourceConfigValue::new(config.clone(), product));
                i
            }
        };
        &mut self.values[index]
    }

    /// All values defined for `config` regardless of product.
    pub fn find_all_values(&self, config: &ConfigDescription) -> Vec<&ResourceConfigValue> {
        self.values.iter().filter(|v| &v.config == config).collect()
    }

    /// Finds or creates a value slot in the flag-disabled list keyed by
    /// (flag, config, product). The caller must set the value's flag.
    pub fn find_or_create_flag_disabled_value(
        &mut self,
        flag: &FeatureFlagAttribute,
        config: &ConfigDescription,
        product: &str,
    ) -> &mut ResourceConfigValue {
        let found = self.flag_disabled_values.iter().position(|v| {
            v.config == *config
                && v.product == product
                && v.value
                    .as_ref()
                    .and_then(|val| val.meta.flag.as_ref())
                    .is_some_and(|f| f == flag)
        });
        match found {
            Some(i) => &mut self.flag_disabled_values[i],
            None => {
                self.flag_disabled_values
                    .push(ResourceConfigValue::new(config.clone(), product));
                self.flag_disabled_values.last_mut().unwrap()
            }
        }
    }

    /// The default config is at the front when values are sorted.
    pub fn has_default_value(&self) -> bool {
        self.values.first().is_some_and(|v| v.config.is_default())
    }
}

/// A resource type (string, drawable, …) holding entries.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceTableType {
    pub named_type: ResourceNamedType,
    pub visibility_level: VisibilityLevel,
    pub entries: Vec<ResourceEntry>,
}

impl ResourceTableType {
    pub fn new(named_type: ResourceNamedType) -> Self {
        ResourceTableType {
            named_type,
            visibility_level: VisibilityLevel::Undefined,
            entries: Vec::new(),
        }
    }

    pub fn find_entry(&self, name: &str) -> Option<&ResourceEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn find_entry_mut(&mut self, name: &str) -> Option<&mut ResourceEntry> {
        self.entries.iter_mut().find(|e| e.name == name)
    }

    pub fn find_or_create_entry(&mut self, name: &str) -> &mut ResourceEntry {
        let index = match self.entries.iter().position(|e| e.name == name) {
            Some(i) => i,
            None => {
                self.entries.push(ResourceEntry::new(name));
                self.entries.len() - 1
            }
        };
        &mut self.entries[index]
    }
}

/// A package within a resource table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResourceTablePackage {
    pub name: String,
    pub types: Vec<ResourceTableType>,
}

impl ResourceTablePackage {
    pub fn new(name: impl Into<String>) -> Self {
        ResourceTablePackage {
            name: name.into(),
            types: Vec::new(),
        }
    }

    pub fn find_type(&self, ty: &ResourceNamedType) -> Option<&ResourceTableType> {
        self.types.iter().find(|t| &t.named_type == ty)
    }

    pub fn find_type_mut(&mut self, ty: &ResourceNamedType) -> Option<&mut ResourceTableType> {
        self.types.iter_mut().find(|t| &t.named_type == ty)
    }

    pub fn find_or_create_type(&mut self, ty: &ResourceNamedType) -> &mut ResourceTableType {
        let index = match self.types.iter().position(|t| &t.named_type == ty) {
            Some(i) => i,
            None => {
                self.types.push(ResourceTableType::new(ty.clone()));
                self.types.len() - 1
            }
        };
        &mut self.types[index]
    }
}

/// How to behave when a resource is added with an ID that clashes with
/// an existing entry of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnIdConflict {
    /// Reject the new value.
    Error,
    /// Create a separate entry with this name+ID combination.
    CreateEntry,
}

/// Outcome of merging a value into an occupied (name, config, product)
/// slot. Mirrors `ResourceTable::CollisionResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionResult {
    KeepOriginal,
    Conflict,
    TakeNew,
}

/// Resolves a same-slot collision between two flag statuses.
/// Mirrors `ResourceTable::ResolveFlagCollision`.
pub fn resolve_flag_collision(existing: FlagStatus, incoming: FlagStatus) -> CollisionResult {
    use FlagStatus::*;
    match (existing, incoming) {
        (NoFlag, NoFlag) => CollisionResult::Conflict,
        (NoFlag, Disabled) => CollisionResult::KeepOriginal,
        (NoFlag, Enabled) => CollisionResult::TakeNew,
        (Disabled, NoFlag) => CollisionResult::TakeNew,
        (Disabled, Disabled) => CollisionResult::KeepOriginal,
        (Disabled, Enabled) => CollisionResult::TakeNew,
        (Enabled, NoFlag) => CollisionResult::KeepOriginal,
        (Enabled, Disabled) => CollisionResult::KeepOriginal,
        (Enabled, Enabled) => CollisionResult::Conflict,
    }
}

/// The default collision handler: weak values lose to strong values, and
/// attributes get USE-vs-DECL handling. Mirrors
/// `ResourceTable::ResolveValueCollision`.
pub fn resolve_value_collision(existing: &Value, incoming: &Value) -> CollisionResult {
    let existing_attr = match &existing.kind {
        ValueKind::Attribute(a) => Some(a),
        _ => None,
    };
    let incoming_attr = match &incoming.kind {
        ValueKind::Attribute(a) => Some(a),
        _ => None,
    };

    let Some(incoming_attr) = incoming_attr else {
        if incoming.meta.weak {
            return CollisionResult::KeepOriginal;
        }
        if existing.meta.weak {
            return CollisionResult::TakeNew;
        }
        return CollisionResult::Conflict;
    };

    let Some(existing_attr) = existing_attr else {
        if existing.meta.weak {
            return CollisionResult::TakeNew;
        }
        return CollisionResult::Conflict;
    };

    // Both are attributes: USE vs DECL handling.
    if attribute_compatible_with(existing_attr, incoming_attr) {
        return if existing.meta.weak {
            CollisionResult::TakeNew
        } else {
            CollisionResult::KeepOriginal
        };
    }

    use super::value::format;
    if existing.meta.weak && existing_attr.type_mask == format::ANY {
        return CollisionResult::TakeNew;
    }
    if incoming.meta.weak && incoming_attr.type_mask == format::ANY {
        return CollisionResult::KeepOriginal;
    }
    CollisionResult::Conflict
}

/// Whether two attribute definitions have compatible formats: references
/// are ignored on both sides, and enums/flags are never compatible.
/// Mirrors `Attribute::IsCompatibleWith`.
pub fn attribute_compatible_with(a: &super::value::Attribute, b: &super::value::Attribute) -> bool {
    use super::value::format;
    let a_mask = a.type_mask & !format::REFERENCE;
    let b_mask = b.type_mask & !format::REFERENCE;
    let enum_or_flags = format::ENUM | format::FLAGS;
    if a_mask & enum_or_flags != 0 || b_mask & enum_or_flags != 0 {
        return false;
    }
    a_mask == b_mask && a.min_int == b.min_int && a.max_int == b.max_int
}

/// A fully described resource ready to be inserted into the table.
/// Mirrors `aapt::NewResource` + `NewResourceBuilder`.
#[derive(Debug, Default)]
pub struct NewResource {
    pub name: Option<ResourceName>,
    pub value: Option<Value>,
    pub config: ConfigDescription,
    pub product: String,
    pub id: Option<(ResourceId, OnIdConflict)>,
    pub visibility: Option<Visibility>,
    pub overlayable: Option<OverlayableItem>,
    pub allow_new: Option<AllowNew>,
    pub staged_id: Option<StagedId>,
    pub allow_mangled: bool,
}

impl NewResource {
    pub fn with_name(name: ResourceName) -> Self {
        NewResource {
            name: Some(name),
            ..Default::default()
        }
    }

    pub fn value(mut self, value: Value) -> Self {
        self.value = Some(value);
        self
    }

    pub fn config(mut self, config: ConfigDescription) -> Self {
        self.config = config;
        self
    }

    pub fn product(mut self, product: impl Into<String>) -> Self {
        self.product = product.into();
        self
    }

    pub fn id(mut self, id: ResourceId) -> Self {
        self.id = Some((id, OnIdConflict::Error));
        self
    }

    pub fn id_with_conflict(mut self, id: ResourceId, on_conflict: OnIdConflict) -> Self {
        self.id = Some((id, on_conflict));
        self
    }

    pub fn visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = Some(visibility);
        self
    }

    pub fn overlayable(mut self, item: OverlayableItem) -> Self {
        self.overlayable = Some(item);
        self
    }

    pub fn allow_new(mut self, allow_new: AllowNew) -> Self {
        self.allow_new = Some(allow_new);
        self
    }

    pub fn staged_id(mut self, staged_id: StagedId) -> Self {
        self.staged_id = Some(staged_id);
        self
    }

    pub fn allow_mangled(mut self, allow: bool) -> Self {
        self.allow_mangled = allow;
        self
    }
}

/// Error produced when a resource cannot be added to the table.
#[derive(Debug, Clone)]
pub struct TableError {
    pub message: String,
    pub source: Source,
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.source.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.source, self.message)
        }
    }
}

impl std::error::Error for TableError {}

/// How strictly resource names are validated when added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ValidatorKind {
    /// Enforce valid Java-identifier-compatible entry names.
    #[default]
    Resource,
    /// Skip name validation (used when merging already-built tables).
    Skip,
}

/// The in-memory resource table.
#[derive(Debug, Default)]
pub struct ResourceTable {
    pub packages: Vec<ResourceTablePackage>,
    /// `<overlayable>` declarations, referenced by index from
    /// [`OverlayableItem`].
    pub overlayables: Vec<Overlayable>,
    /// String pool holding source file paths (only used to mirror the
    /// proto serialization; populated lazily during serialization).
    pub validator: ValidatorKind,
    /// `(tool, version)` fingerprints of the tools that produced this table.
    pub tool_fingerprints: Vec<(String, String)>,
    /// References to non-local packages: `(package_id, package_name)`.
    pub included_packages: Vec<(u8, String)>,
}

impl ResourceTable {
    pub fn new() -> Self {
        ResourceTable {
            validator: ValidatorKind::Resource,
            ..Default::default()
        }
    }

    /// A table that skips name validation (for deserialized tables).
    pub fn new_unvalidated() -> Self {
        ResourceTable {
            validator: ValidatorKind::Skip,
            ..Default::default()
        }
    }

    pub fn find_package(&self, name: &str) -> Option<&ResourceTablePackage> {
        self.packages.iter().find(|p| p.name == name)
    }

    pub fn find_package_mut(&mut self, name: &str) -> Option<&mut ResourceTablePackage> {
        self.packages.iter_mut().find(|p| p.name == name)
    }

    pub fn find_or_create_package(&mut self, name: &str) -> &mut ResourceTablePackage {
        let index = match self.packages.iter().position(|p| p.name == name) {
            Some(i) => i,
            None => {
                self.packages.push(ResourceTablePackage::new(name));
                self.packages.len() - 1
            }
        };
        &mut self.packages[index]
    }

    pub fn find_resource(&self, name: &ResourceName) -> Option<SearchResult<'_>> {
        let package = self.find_package(&name.package)?;
        let ty = package.find_type(&name.ty)?;
        let entry = ty.find_entry(&name.entry)?;
        Some(SearchResult { package, ty, entry })
    }

    pub fn find_resource_mut(&mut self, name: &ResourceName) -> Option<&mut ResourceEntry> {
        self.find_package_mut(&name.package)?
            .find_type_mut(&name.ty)?
            .find_entry_mut(&name.entry)
    }

    pub fn has_resource(&self, name: &ResourceName) -> bool {
        self.find_resource(name).is_some()
    }

    /// Adds a resource, performing collision resolution against any value
    /// already occupying the same (name, config, product) slot.
    ///
    /// Mirrors `ResourceTable::AddResource`.
    pub fn add_resource(&mut self, res: NewResource) -> Result<(), TableError> {
        self.add_resource_impl(res, resolve_value_collision)
    }

    /// Adds a resource with overlay semantics: the incoming value always
    /// replaces the existing one on collision (used by `-R` inputs with
    /// `--auto-add-overlay`-style merging).
    pub fn add_resource_overlay(&mut self, res: NewResource) -> Result<(), TableError> {
        self.add_resource_impl(res, |_, _| CollisionResult::TakeNew)
    }

    fn add_resource_impl(
        &mut self,
        res: NewResource,
        resolver: impl Fn(&Value, &Value) -> CollisionResult,
    ) -> Result<(), TableError> {
        let name = res.name.clone().ok_or_else(|| TableError {
            message: "resource has no name".to_string(),
            source: Source::default(),
        })?;

        let source = res
            .value
            .as_ref()
            .map(|v| v.meta.source.clone())
            .unwrap_or_default();

        // Validate the entry name unless this table was built from
        // already-valid (compiled) input.
        if self.validator == ValidatorKind::Resource && !res.allow_mangled {
            if let Some(bad) = first_invalid_entry_name_char(&name.entry) {
                return Err(TableError {
                    message: format!(
                        "resource '{name}' has invalid entry name '{}'. Invalid character '{bad}'",
                        name.entry
                    ),
                    source,
                });
            }
        }

        if let Some((id, _)) = res.id {
            if !id.is_valid() {
                return Err(TableError {
                    message: format!(
                        "trying to add resource '{name}' with ID {id} but that ID is invalid"
                    ),
                    source,
                });
            }
        }

        let package = self.find_or_create_package(&name.package);
        let ty = package.find_or_create_type(&name.ty);

        // Handle ID conflicts: an existing entry with a different ID.
        let existing_with_other_id = ty.entries.iter().position(|e| {
            e.name == name.entry
                && res.id.is_some()
                && e.id.is_some()
                && e.id != res.id.map(|(id, _)| id)
        });
        let use_new_entry = match existing_with_other_id {
            Some(_) => match res.id {
                Some((id, OnIdConflict::Error)) => {
                    let existing = ty.find_entry(&name.entry).unwrap();
                    return Err(TableError {
                        message: format!(
                            "trying to add resource '{name}' with ID {id} but resource already has ID {}",
                            existing.id.unwrap()
                        ),
                        source,
                    });
                }
                _ => true,
            },
            None => false,
        };

        let entry_index = if use_new_entry {
            ty.entries.push(ResourceEntry::new(&name.entry));
            ty.entries.len() - 1
        } else {
            match ty.entries.iter().position(|e| e.name == name.entry) {
                Some(i) => i,
                None => {
                    ty.entries.push(ResourceEntry::new(&name.entry));
                    ty.entries.len() - 1
                }
            }
        };
        let entry = &mut ty.entries[entry_index];

        if let Some((id, _)) = res.id {
            entry.id = Some(id);
        }

        if let Some(value) = res.value {
            let flag_status = value.meta.flag_status;
            let is_disabled = flag_status == FlagStatus::Disabled;
            if is_disabled {
                let flag = value.meta.flag.clone().unwrap_or_default();
                let slot =
                    entry.find_or_create_flag_disabled_value(&flag, &res.config, &res.product);
                if slot.value.is_some() {
                    return Err(TableError {
                        message: format!(
                            "duplicate value for resource '{name}' with config '{}' and flag '{flag}'",
                            res.config
                        ),
                        source,
                    });
                }
                slot.value = Some(value);
            } else {
                let slot = entry.find_or_create_value(&res.config, &res.product);
                match &slot.value {
                    None => slot.value = Some(value),
                    Some(existing) => {
                        let existing_flag = existing.meta.flag_status;
                        let collision = if existing_flag != FlagStatus::NoFlag
                            || flag_status != FlagStatus::NoFlag
                        {
                            resolve_flag_collision(existing_flag, flag_status)
                        } else {
                            resolver(existing, &value)
                        };
                        match collision {
                            CollisionResult::TakeNew => slot.value = Some(value),
                            CollisionResult::KeepOriginal => {}
                            CollisionResult::Conflict => {
                                let existing_source = existing.meta.source.clone();
                                return Err(TableError {
                                    message: format!(
                                        "duplicate value for resource '{name}' with config '{}': resource previously defined here: {}",
                                        res.config, existing_source
                                    ),
                                    source,
                                });
                            }
                        }
                    }
                }
            }
        }

        if let Some(visibility) = res.visibility {
            let entry = &mut ty.entries[entry_index];
            // Only raise the visibility, never lower it.
            if visibility.level > entry.visibility.level {
                entry.visibility = visibility.clone();
            }
            if visibility.level == VisibilityLevel::Public {
                // The entry stays public even if a future definition omits
                // <public>.
                entry.visibility.level = VisibilityLevel::Public;
            }
            // The type becomes public if any entry is public.
            let entry_is_public = entry.visibility.level == VisibilityLevel::Public;
            if entry_is_public {
                ty.visibility_level = VisibilityLevel::Public;
            }
        }
        let entry = &mut ty.entries[entry_index];

        if let Some(allow_new) = res.allow_new {
            entry.allow_new = Some(allow_new);
        }

        if let Some(overlayable) = res.overlayable {
            if entry.overlayable_item.is_some() {
                return Err(TableError {
                    message: format!("duplicate overlayable declaration for resource '{name}'"),
                    source,
                });
            }
            entry.overlayable_item = Some(overlayable);
        }

        if let Some(staged_id) = res.staged_id {
            entry.staged_id = Some(staged_id);
        }

        Ok(())
    }

    /// Convenience: add a simple (name, config, value) triple.
    pub fn add_value(
        &mut self,
        name: ResourceName,
        config: ConfigDescription,
        value: Value,
    ) -> Result<(), TableError> {
        self.add_resource(NewResource::with_name(name).config(config).value(value))
    }

    /// Iterates over every (package, type, entry) in sorted view order:
    /// packages by (name, id), types by (id, named_type), entries by
    /// (name, id). Returns owned references for read-only passes.
    pub fn sorted_view(
        &self,
    ) -> Vec<(
        &ResourceTablePackage,
        Vec<(&ResourceTableType, Vec<&ResourceEntry>)>,
    )> {
        let mut packages: Vec<&ResourceTablePackage> = self.packages.iter().collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        packages
            .into_iter()
            .map(|p| {
                let mut types: Vec<&ResourceTableType> = p.types.iter().collect();
                types.sort_by(|a, b| a.named_type.cmp(&b.named_type));
                let with_entries = types
                    .into_iter()
                    .map(|t| {
                        let mut entries: Vec<&ResourceEntry> = t.entries.iter().collect();
                        entries.sort_by(|a, b| a.name.cmp(&b.name));
                        (t, entries)
                    })
                    .collect();
                (p, with_entries)
            })
            .collect()
    }
}

/// Result of a table lookup.
pub struct SearchResult<'a> {
    pub package: &'a ResourceTablePackage,
    pub ty: &'a ResourceTableType,
    pub entry: &'a ResourceEntry,
}

/// Returns the first character of `name` that is not valid in a resource
/// entry name, if any. Mirrors `ResourceUtils::IsValidResourceEntryName`
/// (letters, digits, `_`, `.`, `-`, and `$` for AAPT compatibility).
pub fn first_invalid_entry_name_char(name: &str) -> Option<char> {
    name.chars().find(|&c| {
        !(c.is_ascii_alphanumeric()
            || c == '_'
            || c == '.'
            || c == '-'
            || c == '$'
            || !c.is_ascii())
    })
}

/// Mangles an entry name from a merged static library:
/// `package$entry`. Mirrors `NameMangler::MangleEntry`.
pub fn mangle_entry(package: &str, entry: &str) -> String {
    format!("{package}${entry}")
}

/// Reverses [`mangle_entry`], returning `(package, entry)`.
pub fn unmangle_entry(mangled: &str) -> Option<(&str, &str)> {
    let dollar = mangled.find('$')?;
    Some((&mangled[..dollar], &mangled[dollar + 1..]))
}

/// Sugar for building a `ResourceName` from type + strings.
pub fn resource_name(package: &str, ty: ResourceType, entry: &str) -> ResourceName {
    ResourceName::new(package, ty, entry)
}

#[cfg(test)]
mod tests {
    use super::super::value::{Item, Value};
    use super::*;

    fn string_value(s: &str) -> Value {
        Value::item(Item::String {
            value: s.to_string(),
            untranslatable_sections: vec![],
        })
    }

    #[test]
    fn add_and_find() {
        let mut table = ResourceTable::new();
        let name = resource_name("com.app", ResourceType::String, "hello");
        table
            .add_value(
                name.clone(),
                ConfigDescription::default(),
                string_value("Hello"),
            )
            .unwrap();
        let result = table.find_resource(&name).unwrap();
        assert_eq!(result.entry.values.len(), 1);
        assert!(result.entry.has_default_value());
    }

    #[test]
    fn duplicate_strong_values_conflict() {
        let mut table = ResourceTable::new();
        let name = resource_name("com.app", ResourceType::String, "hello");
        table
            .add_value(
                name.clone(),
                ConfigDescription::default(),
                string_value("a"),
            )
            .unwrap();
        let err = table
            .add_value(name, ConfigDescription::default(), string_value("b"))
            .unwrap_err();
        assert!(err.message.contains("duplicate"), "{}", err.message);
    }

    #[test]
    fn weak_loses_to_strong() {
        let mut table = ResourceTable::new();
        let name = resource_name("com.app", ResourceType::Id, "x");
        let mut weak = string_value("weak");
        weak.meta.weak = true;
        table
            .add_value(name.clone(), ConfigDescription::default(), weak)
            .unwrap();
        table
            .add_value(
                name.clone(),
                ConfigDescription::default(),
                string_value("strong"),
            )
            .unwrap();
        let result = table.find_resource(&name).unwrap();
        match &result.entry.values[0].value.as_ref().unwrap().kind {
            ValueKind::Item(Item::String { value, .. }) => assert_eq!(value, "strong"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn invalid_entry_name_rejected() {
        let mut table = ResourceTable::new();
        let name = resource_name("com.app", ResourceType::String, "he//o");
        let err = table
            .add_value(name, ConfigDescription::default(), string_value("x"))
            .unwrap_err();
        assert!(
            err.message.contains("invalid entry name"),
            "{}",
            err.message
        );
    }

    #[test]
    fn mangling_round_trip() {
        let mangled = mangle_entry("com.lib", "foo");
        assert_eq!(mangled, "com.lib$foo");
        assert_eq!(unmangle_entry(&mangled), Some(("com.lib", "foo")));
    }
}
