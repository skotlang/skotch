//! Binary `resources.arsc` flattener.
//!
//! Port of aapt2's `format/binary/TableFlattener.{h,cpp}` and
//! `format/binary/ResEntryWriter.{h,cpp}` (the chunk plumbing lives in
//! [`super`], port of `ChunkWriter.h`): serializes a [`ResourceTable`]
//! into the flattened table format the Android runtime (and this crate's
//! [`super::arsc_parser`]) reads.
//!
//! Layout produced (all chunks 4-byte aligned, sizes back-patched):
//!
//! ```text
//! RES_TABLE_TYPE (ResTable_header)
//!   RES_STRING_POOL_TYPE             -- global value pool, UTF-8, sorted
//!   RES_TABLE_PACKAGE_TYPE per package view
//!     RES_STRING_POOL_TYPE           -- type names, UTF-16, insertion order
//!     RES_STRING_POOL_TYPE           -- entry keys, UTF-8, insertion order
//!     per type (by id): RES_TABLE_TYPE_SPEC_TYPE
//!                       RES_TABLE_TYPE_TYPE per config (config sort order)
//!     RES_TABLE_LIBRARY_TYPE         -- when shared libs exist or id == 0x00
//!     RES_TABLE_OVERLAYABLE_TYPE (+ nested POLICY chunks) per group
//!     RES_TABLE_STAGED_ALIAS_TYPE    -- when staged IDs exist
//! ```
//!
//! Design notes mirroring the C++:
//!
//! - `TableFlattener::Consume` sorts the value pool by `(priority,
//!   config)`; `String`/`StyledString` values carry `kNormalPriority` and
//!   `FileReference` paths `kHighPriority` (see `ProtoDeserialize.cpp` /
//!   `ResourceUtils::ParseBinaryResValue`, which create those contexts).
//!   The C++ `Value` objects hold pool refs already; this port interns the
//!   plain strings of the table in a first pass and resolves indices after
//!   sorting.
//! - The package list is the C++ `GetPartitionedView(create_alias_entries
//!   = true)`: packages keyed by `(name, package-id)`, types by `(id,
//!   named type)`, entries by `(name, entry-id)`; entries with a staged ID
//!   get a clone entry under the alias ID; types whose named type appears
//!   with multiple type IDs are extracted into trailing packages.
//! - Compact entries (`FLAG_COMPACT` + `FLAG_OFFSET16`) and sparse type
//!   chunks (`FLAG_SPARSE`) follow `ResEntryWriter`/`FlattenConfig`,
//!   including the `kSparseEncodingThreshold` populated-entry heuristic.
//!   The C++ additionally gates both on the link-time `--min-sdk-version`;
//!   this port has no link context (minSdk 0), so compact entries are
//!   gated only on the 16-bit key constraint and sparse encoding on the
//!   config carrying no `sdkVersion` (the exact C++ behavior at minSdk 0).

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::*;
use crate::res::config::ConfigDescription;
use crate::res::string_pool::{Context, Ref, StringPool, StyleRef};
use crate::res::table::{
    Overlayable, OverlayableItem, ResourceConfigValue, ResourceEntry, ResourceTable,
    ResourceTablePackage, ResourceTableType, StagedId, Visibility, VisibilityLevel,
};
use crate::res::value::{
    res_value_type, Item, ResValue, StyleEntry, Value, ValueKind, PLURAL_COUNT,
};
use crate::res::{
    ResourceId, ResourceNamedType, ResourceType, APP_PACKAGE_ID, FRAMEWORK_PACKAGE_ID,
};

/// `kSparseEncodingThreshold` (TableFlattener.h): a type/config chunk is
/// sparse-encoded only when fewer than this percentage of its entry slots
/// are populated.
pub const SPARSE_ENCODING_THRESHOLD: usize = 60;

/// `Obfuscator::kObfuscatedResourceName`: the single key every entry name
/// collapses to when `collapse_key_stringpool` is enabled.
pub const OBFUSCATED_RESOURCE_NAME: &str = "0_resource_name_obfuscated";

/// Options controlling the flattened output, port of the corresponding
/// fields of `aapt::TableFlattenerOptions`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TableFlattenerOptions {
    /// Encode type chunks with a sparse `(idx, offset)` entry map when
    /// beneficial (`SparseEntriesMode::Enabled`).
    pub use_sparse_entries: bool,
    /// Use 8-byte compact entries (`ResTable_entry::FLAG_COMPACT`) for
    /// simple values whose key index fits in 16 bits.
    pub use_compact_entries: bool,
    /// Sorted string pool with deduped keys when collapsing: every entry
    /// name (except overlayable entries) becomes
    /// [`OBFUSCATED_RESOURCE_NAME`].
    pub collapse_key_stringpool: bool,
}

/// Flattens `table` to a complete `resources.arsc` blob.
///
/// Port of `TableFlattener::Consume`. Every resource entry must have an
/// assigned ID (like the C++, which CHECK-fails otherwise).
pub fn flatten_table(table: &ResourceTable, options: &TableFlattenerOptions) -> Result<Vec<u8>> {
    let view = build_view(table);

    // Intern every pooled string of the table, then sort the pool the way
    // TableFlattener::Consume does (priority, then config) before any
    // index is resolved.
    let mut value_pool = StringPool::new();
    let mut item_refs: HashMap<usize, PoolRef> = HashMap::new();
    for package in &view {
        for ty in &package.types {
            for entry in &ty.entries {
                for config_value in &entry.values {
                    if let Some(value) = config_value.value.as_ref() {
                        intern_value_strings(
                            value,
                            &config_value.config,
                            &mut value_pool,
                            &mut item_refs,
                        );
                    }
                }
            }
        }
    }
    value_pool.sort();

    let mut out = Vec::new();
    let table_writer = ChunkWriter::start(&mut out, RES_TABLE_TYPE, RES_TABLE_HEADER_SIZE);
    patch_u32(&mut out, table_writer.start_offset() + 8, view.len() as u32); // packageCount

    // Flatten the values string pool (UTF-8, like StringPool::FlattenUtf8).
    out.extend_from_slice(&value_pool.flatten_utf8());

    // Referenced (shared-library) packages: the table's own list plus a
    // self-mapping for every package whose ID is non-standard. Mirrors the
    // PackageType::kApp branch of TableFlattener::Consume, including the
    // mid-loop mutation (earlier packages don't see later self-mappings).
    let mut shared_libs: BTreeMap<u8, String> = BTreeMap::new();
    for (id, name) in &table.included_packages {
        shared_libs.entry(*id).or_insert_with(|| name.clone());
    }

    for package in &view {
        let package_id = package.id.ok_or_else(|| {
            anyhow!(
                "resource IDs have not been assigned before flattening the table \
                 (package '{}' has entries without IDs)",
                package.name
            )
        })?;
        if package_id != APP_PACKAGE_ID && package_id != FRAMEWORK_PACKAGE_ID {
            match shared_libs.get(&package_id) {
                Some(existing) if *existing != package.name => bail!(
                    "can't map package ID {package_id:02x} to '{}'. Already mapped to '{existing}'",
                    package.name
                ),
                Some(_) => {}
                None => {
                    shared_libs.insert(package_id, package.name.clone());
                }
            }
        }

        let mut flattener = PackageFlattener {
            package,
            package_id,
            overlayables: &table.overlayables,
            shared_libs: &shared_libs,
            options,
            value_pool: &value_pool,
            item_refs: &item_refs,
            type_pool: StringPool::new(),
            key_pool: StringPool::new(),
            aliases: BTreeMap::new(),
        };
        flattener.flatten_package(&mut out)?;
    }

    table_writer.finish(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Partitioned table view (port of ResourceTable::GetPartitionedView).
// ---------------------------------------------------------------------------

struct EntryView<'a> {
    name: &'a str,
    id: Option<u16>,
    visibility: &'a Visibility,
    overlayable_item: Option<&'a OverlayableItem>,
    staged_id: Option<StagedId>,
    values: Vec<&'a ResourceConfigValue>,
}

struct TypeView<'a> {
    named_type: ResourceNamedType,
    id: Option<u8>,
    entries: Vec<EntryView<'a>>,
}

struct PackageView<'a> {
    name: String,
    id: Option<u8>,
    types: Vec<TypeView<'a>>,
}

/// Builds the partitioned view with alias entries, port of
/// `GetPartitionedView({.create_alias_entries = true})`.
fn build_view(table: &ResourceTable) -> Vec<PackageView<'_>> {
    let mut packages: Vec<PackageView> = Vec::new();
    for package in &table.packages {
        for ty in &package.types {
            for entry in &ty.entries {
                insert_entry_into_view(
                    &mut packages,
                    package,
                    ty,
                    entry,
                    entry.id,
                    entry.staged_id,
                );
                if let Some(staged) = entry.staged_id {
                    // The alias clone keeps everything but the staged ID.
                    insert_entry_into_view(
                        &mut packages,
                        package,
                        ty,
                        entry,
                        Some(staged.id),
                        None,
                    );
                }
            }
        }
    }
    extract_duplicate_type_ids(&mut packages);
    packages
}

/// Port of `InsertEntryIntoTableView`: sorted find-or-insert at every
/// level, keeping the first entry on an exact `(name, id)` duplicate.
fn insert_entry_into_view<'a>(
    packages: &mut Vec<PackageView<'a>>,
    package: &'a ResourceTablePackage,
    ty: &'a ResourceTableType,
    entry: &'a ResourceEntry,
    id: Option<ResourceId>,
    staged_id: Option<StagedId>,
) {
    let package_id = id.map(|i| i.package_id());
    let package_pos = match packages
        .binary_search_by(|p| (p.name.as_str(), p.id).cmp(&(package.name.as_str(), package_id)))
    {
        Ok(i) => i,
        Err(i) => {
            packages.insert(
                i,
                PackageView {
                    name: package.name.clone(),
                    id: package_id,
                    types: Vec::new(),
                },
            );
            i
        }
    };
    let package_view = &mut packages[package_pos];

    let type_id = id.map(|i| i.type_id());
    let type_pos = match package_view
        .types
        .binary_search_by(|t| (t.id, &t.named_type).cmp(&(type_id, &ty.named_type)))
    {
        Ok(i) => i,
        Err(i) => {
            package_view.types.insert(
                i,
                TypeView {
                    named_type: ty.named_type.clone(),
                    id: type_id,
                    entries: Vec::new(),
                },
            );
            i
        }
    };
    let type_view = &mut package_view.types[type_pos];

    let entry_id = id.map(|i| i.entry_id());
    match type_view
        .entries
        .binary_search_by(|e| (e.name, e.id).cmp(&(entry.name.as_str(), entry_id)))
    {
        Ok(_) => {} // SortedVectorInserter::Insert keeps the existing one.
        Err(i) => type_view.entries.insert(
            i,
            EntryView {
                name: &entry.name,
                id: entry_id,
                visibility: &entry.visibility,
                overlayable_item: entry.overlayable_item.as_ref(),
                staged_id,
                values: entry.values.iter().collect(),
            },
        ),
    }
}

/// The Android runtime cannot query resources when one package holds the
/// same resource type under multiple type IDs, so every repeated named
/// type is extracted into its own (trailing) package chunk. Port of the
/// second half of `GetPartitionedView`.
fn extract_duplicate_type_ids(packages: &mut Vec<PackageView<'_>>) {
    let mut new_packages: Vec<PackageView> = Vec::new();
    for package in packages.iter_mut() {
        let start_index = new_packages.len();
        let mut type_new_package_index: HashMap<ResourceNamedType, usize> = HashMap::new();
        let mut i = 0;
        while i < package.types.len() {
            let named_type = package.types[i].named_type.clone();
            match type_new_package_index.get(&named_type).copied() {
                None => {
                    // First occurrence: stays in this package.
                    type_new_package_index.insert(named_type, start_index);
                    i += 1;
                }
                Some(index) => {
                    if new_packages.len() == index {
                        new_packages.push(PackageView {
                            name: package.name.clone(),
                            id: package.id,
                            types: Vec::new(),
                        });
                    }
                    type_new_package_index.insert(named_type, index + 1);
                    let ty = package.types.remove(i);
                    let other = &mut new_packages[index];
                    if let Err(pos) = other
                        .types
                        .binary_search_by(|t| (t.id, &t.named_type).cmp(&(ty.id, &ty.named_type)))
                    {
                        other.types.insert(pos, ty);
                    }
                }
            }
        }
    }
    // Insert newly created packages right after their original package.
    for new_package in new_packages {
        let pos = packages.partition_point(|p| {
            (p.name.as_str(), p.id) < (new_package.name.as_str(), new_package.id)
        });
        let insert_at = (pos + 1).min(packages.len());
        packages.insert(insert_at, new_package);
    }
}

// ---------------------------------------------------------------------------
// Value string pool interning.
// ---------------------------------------------------------------------------

/// Where a string-bearing item lives in the value pool.
enum PoolRef {
    Plain(Ref),
    Styled(StyleRef),
}

/// A sort key whose lexicographic byte order matches
/// `ResTable_config::compare` — the C++ `StringPool::Context` stores the
/// `ConfigDescription` itself and the sort comparator calls `compare`;
/// the Rust pool stores an opaque key instead.
fn config_sort_key(config: &ConfigDescription) -> Vec<u8> {
    let mut key = Vec::with_capacity(45);
    key.extend_from_slice(&config.imsi().to_be_bytes());
    key.extend_from_slice(&config.locale_u32().to_be_bytes());
    let script: [u8; 4] = if config.locale_script_was_computed {
        [0; 4]
    } else {
        config.locale_script
    };
    key.extend_from_slice(&script);
    key.extend_from_slice(&config.locale_variant);
    key.extend_from_slice(&config.locale_numbering_system);
    key.push(config.grammatical_inflection);
    key.extend_from_slice(&config.screen_type().to_be_bytes());
    key.extend_from_slice(&config.input24().to_be_bytes());
    key.extend_from_slice(&config.screen_size_u32().to_be_bytes());
    key.extend_from_slice(&config.version_u32().to_be_bytes());
    key.push(config.screen_layout);
    key.push(config.screen_layout2);
    key.push(config.color_mode);
    key.push(config.ui_mode);
    key.extend_from_slice(&config.smallest_screen_width_dp.to_be_bytes());
    key.extend_from_slice(&config.screen_size_dp_u32().to_be_bytes());
    key
}

/// Interns every pooled string reachable from `value`.
fn intern_value_strings(
    value: &Value,
    config: &ConfigDescription,
    pool: &mut StringPool,
    refs: &mut HashMap<usize, PoolRef>,
) {
    match &value.kind {
        ValueKind::Item(item) => intern_item_strings(item, config, pool, refs),
        ValueKind::Style(style) => {
            for entry in &style.entries {
                intern_item_strings(&entry.value.item, config, pool, refs);
            }
        }
        ValueKind::Array(array) => {
            for element in &array.elements {
                intern_item_strings(&element.item, config, pool, refs);
            }
        }
        ValueKind::Plural(plural) => {
            for slot in plural.values.iter().flatten() {
                intern_item_strings(&slot.item, config, pool, refs);
            }
        }
        ValueKind::Attribute(_) | ValueKind::Styleable(_) | ValueKind::Macro(_) => {}
    }
}

/// Interns one item if it carries a string. Contexts mirror what the C++
/// value constructors use: `Context(config)` (normal priority) for
/// strings, `Context(kHighPriority, config)` for file references.
fn intern_item_strings(
    item: &Item,
    config: &ConfigDescription,
    pool: &mut StringPool,
    refs: &mut HashMap<usize, PoolRef>,
) {
    let key = item_key(item);
    if refs.contains_key(&key) {
        // Alias entries share value objects with their original entry;
        // intern once (the C++ shares the StringPool::Ref the same way).
        return;
    }
    match item {
        Item::String { value, .. } => {
            let r = pool.make_ref(
                value,
                Context::new(Context::NORMAL_PRIORITY, config_sort_key(config)),
            );
            refs.insert(key, PoolRef::Plain(r));
        }
        Item::RawString(value) => {
            let r = pool.make_ref(
                value,
                Context::new(Context::NORMAL_PRIORITY, config_sort_key(config)),
            );
            refs.insert(key, PoolRef::Plain(r));
        }
        Item::StyledString { value, spans, .. } => {
            let spans = spans
                .iter()
                .map(|s| (s.name.clone(), s.first_char, s.last_char))
                .collect();
            let r = pool.make_style_ref(
                value,
                spans,
                Context::new(Context::NORMAL_PRIORITY, config_sort_key(config)),
            );
            refs.insert(key, PoolRef::Styled(r));
        }
        Item::FileReference(file) => {
            let r = pool.make_ref(
                &file.path,
                Context::new(Context::HIGH_PRIORITY, config_sort_key(config)),
            );
            refs.insert(key, PoolRef::Plain(r));
        }
        Item::Reference(_) | Item::Id | Item::BinaryPrimitive(_) => {}
    }
}

/// Identity of an item within the (immutably borrowed) table; used only
/// as a map key, never dereferenced.
fn item_key(item: &Item) -> usize {
    item as *const Item as usize
}

// ---------------------------------------------------------------------------
// Per-package flattening (port of the C++ PackageFlattener).
// ---------------------------------------------------------------------------

/// One value to write into a type/config chunk, port of `FlatEntry`.
struct FlatEntry<'a> {
    entry: &'a EntryView<'a>,
    value: &'a Value,
    /// Key-pool index of the entry name.
    entry_key: u32,
}

struct PackageFlattener<'a> {
    package: &'a PackageView<'a>,
    package_id: u8,
    overlayables: &'a [Overlayable],
    shared_libs: &'a BTreeMap<u8, String>,
    options: &'a TableFlattenerOptions,
    value_pool: &'a StringPool,
    item_refs: &'a HashMap<usize, PoolRef>,
    type_pool: StringPool,
    key_pool: StringPool,
    /// staged (alias) resource ID -> finalized resource ID.
    aliases: BTreeMap<u32, u32>,
}

impl<'a> PackageFlattener<'a> {
    /// Port of `PackageFlattener::FlattenPackage`.
    fn flatten_package(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let pkg_writer = ChunkWriter::start(out, RES_TABLE_PACKAGE_TYPE, PACKAGE_HEADER_SIZE);
        let start = pkg_writer.start_offset();
        patch_u32(out, start + 8, self.package_id as u32);
        // AAPT truncated long package names (app packages); mirror that.
        write_utf16_fixed(out, start + 12, 128, &self.package.name);

        // Serialize the types first so the type and key pools are
        // populated; the pools are written before the type chunks.
        let mut type_buffer = Vec::new();
        self.flatten_types(&mut type_buffer)?;

        let type_strings_offset = pkg_writer.size(out) as u32;
        patch_u32(out, start + 268, type_strings_offset); // typeStrings
        out.extend_from_slice(&self.type_pool.flatten_utf16());
        let key_strings_offset = pkg_writer.size(out) as u32;
        patch_u32(out, start + 276, key_strings_offset); // keyStrings
        out.extend_from_slice(&self.key_pool.flatten_utf8());
        // lastPublicType (+272), lastPublicKey (+280), typeIdOffset (+284)
        // stay zero, exactly like the zero-initialized C++ header.

        out.extend_from_slice(&type_buffer);

        // If there are libraries (or if the package ID is 0x00), encode a
        // library chunk.
        if self.package_id == 0x00 || !self.shared_libs.is_empty() {
            self.flatten_library_spec(out);
        }

        self.flatten_overlayable(out)?;
        self.flatten_aliases(out);

        pkg_writer.finish(out);
        Ok(())
    }

    /// Port of `PackageFlattener::FlattenTypes`.
    fn flatten_types(&mut self, buffer: &mut Vec<u8>) -> Result<()> {
        let package = self.package;
        let mut expected_type_id: u16 = 1;
        for ty in &package.types {
            if matches!(
                ty.named_type.ty,
                ResourceType::Styleable | ResourceType::Macro
            ) {
                // Styleables and macros are not real resource types.
                continue;
            }
            let type_id = ty.id.ok_or_else(|| {
                anyhow!(
                    "type '{}' in package '{}' has no ID; \
                     resource IDs must be assigned before flattening",
                    ty.named_type,
                    package.name
                )
            })?;

            // If there is a gap in the type IDs, fill in the StringPool
            // with placeholder values until we reach the ID we expect.
            while (type_id as u16) > expected_type_id {
                self.type_pool
                    .make_ref_plain(&format!("?{expected_type_id}"));
                expected_type_id += 1;
            }
            expected_type_id += 1;
            self.type_pool.make_ref_plain(&ty.named_type.name);

            // Entries are sorted by (name, id); there may be holes in the
            // ID space, so the entry count is the maximum ID plus one.
            let mut num_entries = 0usize;
            for entry in &ty.entries {
                let id = entry.id.ok_or_else(|| {
                    anyhow!(
                        "resource '{}:{}/{}' has no ID; \
                         resource IDs must be assigned before flattening",
                        package.name,
                        ty.named_type,
                        entry.name
                    )
                })?;
                num_entries = num_entries.max(id as usize + 1);
            }
            if num_entries > u16::MAX as usize {
                bail!(
                    "type '{}' has too many entries ({num_entries})",
                    ty.named_type
                );
            }

            let spec_start = flatten_type_spec(ty, type_id, num_entries, buffer);

            if ty.entries.is_empty() {
                continue;
            }

            // The binary table lists entries per configuration; the model
            // stores them inverted. Group by config (std::map order ==
            // ResTable_config::compare == our Ord).
            let mut config_to_entries: BTreeMap<ConfigDescription, Vec<FlatEntry>> =
                BTreeMap::new();
            for entry in &ty.entries {
                if let Some(staged) = entry.staged_id {
                    self.aliases.insert(
                        staged.id.0,
                        ResourceId::new(self.package_id, type_id, entry.id.unwrap()).0,
                    );
                }

                // Port of Obfuscator::ObfuscateResourceName: with key
                // collapsing, every name becomes the single obfuscated key
                // except overlayable entries (renaming those would break
                // runtime overlays; the C++ warns and keeps the name).
                let key_ref =
                    if self.options.collapse_key_stringpool && entry.overlayable_item.is_none() {
                        self.key_pool.make_ref_plain(OBFUSCATED_RESOURCE_NAME)
                    } else {
                        self.key_pool.make_ref_plain(entry.name)
                    };
                let entry_key = self.key_pool.resolve(key_ref) as u32;

                for config_value in &entry.values {
                    let Some(value) = config_value.value.as_ref() else {
                        continue;
                    };
                    config_to_entries
                        .entry(config_value.config)
                        .or_default()
                        .push(FlatEntry {
                            entry,
                            value,
                            entry_key,
                        });
                }
            }

            for (config, entries) in &config_to_entries {
                self.flatten_config(type_id, config, num_entries, entries, buffer)?;
            }

            // Update the type-chunk count in the typeSpec header.
            patch_u16(
                buffer,
                spec_start + 10,
                config_to_entries.len().min(u16::MAX as usize) as u16,
            );
        }
        Ok(())
    }

    /// Port of `PackageFlattener::FlattenConfig`.
    fn flatten_config(
        &self,
        type_id: u8,
        config: &ConfigDescription,
        num_total_entries: usize,
        entries: &[FlatEntry],
        buffer: &mut Vec<u8>,
    ) -> Result<()> {
        let header_size = (TYPE_HEADER_PREFIX_SIZE + ConfigDescription::SIZE) as u16;
        let type_writer = ChunkWriter::start(buffer, RES_TABLE_TYPE_TYPE, header_size);
        let start = type_writer.start_offset();
        buffer[start + 8] = type_id;
        let config_bytes = config.to_bytes();
        buffer[start + TYPE_HEADER_PREFIX_SIZE..start + header_size as usize]
            .copy_from_slice(&config_bytes);

        // Use compact entries only when enabled and every key index fits
        // in 16 bits (the C++ also requires minSdk > T; see module docs).
        let compact_entry = self.options.use_compact_entries
            && entries.iter().all(|e| e.entry_key < u16::MAX as u32);

        let mut offsets = vec![NO_ENTRY; num_total_entries];
        let mut values_buffer: Vec<u8> = Vec::new();
        for flat_entry in entries {
            let entry_id = flat_entry.entry.id.unwrap() as usize; // validated by caller
            debug_assert!(entry_id < num_total_entries);
            offsets[entry_id] =
                self.write_entry(flat_entry, compact_entry, &mut values_buffer)? as u32;
        }

        // Whether the offsets can be represented in 2 bytes.
        let short_offsets = values_buffer.len() / 4 < u16::MAX as usize;

        let mut sparse_encode = self.options.use_sparse_entries;
        // Port of the minSdk gating with GetMinSdkVersion() == 0: sparse
        // encoding stays enabled only for configs without an sdkVersion.
        if config.sdk_version != 0 {
            sparse_encode = false;
        }
        // Only sparse encode if the offsets fit in 2 bytes.
        sparse_encode = sparse_encode && short_offsets;
        // Only sparse encode if the ratio of populated entries to total
        // entries is below the threshold.
        sparse_encode =
            sparse_encode && (100 * entries.len()) / num_total_entries < SPARSE_ENCODING_THRESHOLD;

        if sparse_encode {
            patch_u32(buffer, start + 12, entries.len() as u32); // entryCount
            buffer[start + 9] |= FLAG_SPARSE;
            for (idx, offset) in offsets.iter().enumerate() {
                if *offset != NO_ENTRY {
                    debug_assert_eq!(offset & 0x3, 0);
                    push_u16(buffer, idx as u16);
                    push_u16(buffer, (offset / 4) as u16);
                }
            }
        } else {
            patch_u32(buffer, start + 12, num_total_entries as u32); // entryCount
            if compact_entry && short_offsets {
                // 16-bit offsets are used only with compact entries.
                // NO_ENTRY / 4 truncates to NO_ENTRY16, like the C++.
                buffer[start + 9] |= FLAG_OFFSET16;
                for offset in &offsets {
                    push_u16(buffer, (offset / 4) as u16);
                }
            } else {
                for offset in &offsets {
                    push_u32(buffer, *offset);
                }
            }
        }

        align4(buffer);
        let entries_start = type_writer.size(buffer) as u32;
        patch_u32(buffer, start + 16, entries_start);
        buffer.extend_from_slice(&values_buffer);
        type_writer.finish(buffer);
        Ok(())
    }

    /// Port of `ResEntryWriter::Write` (+ `WriteEntry`, `WriteItemToBuffer`
    /// and `WriteMapToBuffer`): writes one entry into the values buffer and
    /// returns its offset.
    fn write_entry(
        &self,
        flat_entry: &FlatEntry,
        compact: bool,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        let offset = out.len();
        let mut flags: u16 = 0;
        if flat_entry.entry.visibility.level == VisibilityLevel::Public {
            flags |= ENTRY_FLAG_PUBLIC;
        }
        if flat_entry.value.meta.weak {
            flags |= ENTRY_FLAG_WEAK;
        }

        if let ValueKind::Item(item) = &flat_entry.value.kind {
            let value = self.flatten_item(item)?;
            if compact {
                // Compact entry: key in 16 bits, data type in the high
                // byte of flags, data inline.
                flags |= ENTRY_FLAG_COMPACT | ((value.data_type as u16) << 8);
                push_u16(out, flat_entry.entry_key as u16);
                push_u16(out, flags);
                push_u32(out, value.data);
            } else {
                push_u16(out, 8); // sizeof(ResTable_entry)
                push_u16(out, flags);
                push_u32(out, flat_entry.entry_key);
                write_res_value(out, value);
            }
        } else {
            // ResTable_map_entry: a complex entry, never compact.
            flags |= ENTRY_FLAG_COMPLEX;
            push_u16(out, 16); // sizeof(ResTable_map_entry)
            push_u16(out, flags);
            push_u32(out, flat_entry.entry_key);
            let parent_at = out.len();
            push_u32(out, 0); // parent, patched by the style arm
            let count_at = out.len();
            push_u32(out, 0); // count, patched below
            let count = self.write_map_body(flat_entry, parent_at, out)?;
            patch_u32(out, count_at, count as u32);
        }
        Ok(offset)
    }

    /// Port of `MapFlattenVisitor`: writes the `ResTable_map` rows of a
    /// complex value and returns how many were written.
    fn write_map_body(
        &self,
        flat_entry: &FlatEntry,
        parent_at: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        let mut count = 0usize;
        match &flat_entry.value.kind {
            ValueKind::Attribute(attr) => {
                write_map_row(
                    out,
                    ATTR_TYPE,
                    ResValue::new(res_value_type::TYPE_INT_DEC, attr.type_mask),
                );
                count += 1;
                if attr.min_int != i32::MIN {
                    write_map_row(
                        out,
                        ATTR_MIN,
                        ResValue::new(res_value_type::TYPE_INT_DEC, attr.min_int as u32),
                    );
                    count += 1;
                }
                if attr.max_int != i32::MAX {
                    write_map_row(
                        out,
                        ATTR_MAX,
                        ResValue::new(res_value_type::TYPE_INT_DEC, attr.max_int as u32),
                    );
                    count += 1;
                }
                for symbol in &attr.symbols {
                    let id = symbol.symbol.id.ok_or_else(|| {
                        anyhow!("attribute symbol of '{}' has no ID", flat_entry.entry.name)
                    })?;
                    write_map_row(out, id.0, ResValue::new(symbol.data_type, symbol.value));
                    count += 1;
                }
            }
            ValueKind::Style(style) => {
                if let Some(parent) = &style.parent {
                    let id = parent.id.ok_or_else(|| {
                        anyhow!("style parent of '{}' has no ID", flat_entry.entry.name)
                    })?;
                    patch_u32(out, parent_at, id.0);
                }
                let mut sorted: Vec<&StyleEntry> = style.entries.iter().collect();
                sorted.sort_by(|a, b| {
                    use std::cmp::Ordering;
                    if style_entry_less(a, b) {
                        Ordering::Less
                    } else if style_entry_less(b, a) {
                        Ordering::Greater
                    } else {
                        Ordering::Equal
                    }
                });
                for entry in sorted {
                    let id = entry.key.id.ok_or_else(|| {
                        anyhow!("style entry key of '{}' has no ID", flat_entry.entry.name)
                    })?;
                    write_map_row(out, id.0, self.flatten_item(&entry.value.item)?);
                    count += 1;
                }
            }
            ValueKind::Styleable(styleable) => {
                for reference in &styleable.entries {
                    let id = reference.id.ok_or_else(|| {
                        anyhow!("styleable entry of '{}' has no ID", flat_entry.entry.name)
                    })?;
                    write_map_row(out, id.0, ResValue::default());
                    count += 1;
                }
            }
            ValueKind::Array(array) => {
                for (i, element) in array.elements.iter().enumerate() {
                    write_map_row(out, ATTR_MIN + i as u32, self.flatten_item(&element.item)?);
                    count += 1;
                }
            }
            ValueKind::Plural(plural) => {
                const PLURAL_ATTR_KEYS: [u32; PLURAL_COUNT] = [
                    ATTR_ZERO, ATTR_ONE, ATTR_TWO, ATTR_FEW, ATTR_MANY, ATTR_OTHER,
                ];
                for (i, slot) in plural.values.iter().enumerate() {
                    let Some(slot) = slot else { continue };
                    write_map_row(out, PLURAL_ATTR_KEYS[i], self.flatten_item(&slot.item)?);
                    count += 1;
                }
            }
            ValueKind::Item(_) | ValueKind::Macro(_) => {
                bail!(
                    "value of '{}' cannot be flattened as a map",
                    flat_entry.entry.name
                )
            }
        }
        Ok(count)
    }

    /// Flattens one item to its binary `Res_value`, resolving pooled
    /// strings through the (sorted) value pool. Port of `Item::Flatten`.
    fn flatten_item(&self, item: &Item) -> Result<ResValue> {
        match item {
            Item::String { .. }
            | Item::RawString(_)
            | Item::StyledString { .. }
            | Item::FileReference(_) => match self.item_refs.get(&item_key(item)) {
                Some(PoolRef::Plain(r)) => Ok(ResValue::new(
                    res_value_type::TYPE_STRING,
                    self.value_pool.resolve(*r) as u32,
                )),
                Some(PoolRef::Styled(r)) => Ok(ResValue::new(
                    res_value_type::TYPE_STRING,
                    self.value_pool.resolve_style(*r) as u32,
                )),
                None => bail!("string value was not interned into the value pool"),
            },
            _ => item
                .flatten()
                .ok_or_else(|| anyhow!("unable to flatten value to binary form")),
        }
    }

    /// Port of `PackageFlattener::FlattenLibrarySpec`.
    fn flatten_library_spec(&self, out: &mut Vec<u8>) {
        let lib_writer = ChunkWriter::start(out, RES_TABLE_LIBRARY_TYPE, LIB_HEADER_SIZE);
        let self_entry = usize::from(self.package_id == 0x00);
        let num_entries = self_entry + self.shared_libs.len();
        patch_u32(out, lib_writer.start_offset() + 8, num_entries as u32);

        let write_lib_entry = |out: &mut Vec<u8>, id: u32, name: &str| {
            push_u32(out, id);
            let name_off = out.len();
            out.resize(name_off + 256, 0);
            write_utf16_fixed(out, name_off, 128, name);
        };
        if self.package_id == 0x00 {
            // Add this package.
            write_lib_entry(out, 0, &self.package.name);
        }
        for (id, name) in self.shared_libs {
            write_lib_entry(out, *id as u32, name);
        }
        lib_writer.finish(out);
    }

    /// Port of `PackageFlattener::FlattenOverlayable`.
    fn flatten_overlayable(&self, out: &mut Vec<u8>) -> Result<()> {
        struct OverlayableChunk<'a> {
            actor: &'a str,
            source: &'a crate::res::Source,
            policy_ids: BTreeMap<u32, BTreeSet<u32>>,
        }

        let mut seen_ids: HashSet<u32> = HashSet::new();
        let mut chunks: BTreeMap<&str, OverlayableChunk> = BTreeMap::new();

        for ty in &self.package.types {
            let Some(type_id) = ty.id else { continue };
            for entry in &ty.entries {
                let Some(item) = entry.overlayable_item else {
                    continue;
                };
                let overlayable =
                    self.overlayables
                        .get(item.overlayable_index)
                        .ok_or_else(|| {
                            anyhow!(
                                "overlayable index {} of resource '{}' is out of range",
                                item.overlayable_index,
                                entry.name
                            )
                        })?;
                let Some(entry_id) = entry.id else { continue };
                let id = ResourceId::new(self.package_id, type_id, entry_id);

                // Resource ids should only appear once in the table.
                if !seen_ids.insert(id.0) {
                    bail!(
                        "multiple overlayable definitions found for resource \
                         {}:{}/{}",
                        self.package.name,
                        ty.named_type,
                        entry.name
                    );
                }

                let chunk = match chunks.get_mut(overlayable.name.as_str()) {
                    Some(chunk) => {
                        if chunk.source != &overlayable.source {
                            bail!("duplicate overlayable name '{}'", overlayable.name);
                        }
                        if chunk.actor != overlayable.actor {
                            bail!(
                                "overlayable '{}' declared with different actors ('{}' vs '{}')",
                                overlayable.name,
                                chunk.actor,
                                overlayable.actor
                            );
                        }
                        chunk
                    }
                    None => chunks.entry(&overlayable.name).or_insert(OverlayableChunk {
                        actor: &overlayable.actor,
                        source: &overlayable.source,
                        policy_ids: BTreeMap::new(),
                    }),
                };

                if item.policies == 0 {
                    bail!("overlayable {} does not specify policy", entry.name);
                }
                chunk
                    .policy_ids
                    .entry(item.policies)
                    .or_default()
                    .insert(id.0);
            }
        }

        for (name, chunk) in &chunks {
            if name.encode_utf16().count() >= 256 {
                bail!("overlayable name '{name}' exceeds maximum length (256 utf16 characters)");
            }
            if chunk.actor.encode_utf16().count() >= 256 {
                bail!(
                    "overlayable name '{}' exceeds maximum length (256 utf16 characters)",
                    chunk.actor
                );
            }

            let overlayable_writer =
                ChunkWriter::start(out, RES_TABLE_OVERLAYABLE_TYPE, OVERLAYABLE_HEADER_SIZE);
            let start = overlayable_writer.start_offset();
            write_utf16_fixed(out, start + 8, 256, name);
            write_utf16_fixed(out, start + 8 + 512, 256, chunk.actor);

            // Write each policy block for the overlayable.
            for (policies, ids) in &chunk.policy_ids {
                let policy_writer = ChunkWriter::start(
                    out,
                    RES_TABLE_OVERLAYABLE_POLICY_TYPE,
                    OVERLAYABLE_POLICY_HEADER_SIZE,
                );
                patch_u32(out, policy_writer.start_offset() + 8, *policies);
                patch_u32(out, policy_writer.start_offset() + 12, ids.len() as u32);
                for id in ids {
                    push_u32(out, *id);
                }
                policy_writer.finish(out);
            }
            overlayable_writer.finish(out);
        }
        Ok(())
    }

    /// Port of `PackageFlattener::FlattenAliases`.
    fn flatten_aliases(&self, out: &mut Vec<u8>) {
        if self.aliases.is_empty() {
            return;
        }
        let alias_writer =
            ChunkWriter::start(out, RES_TABLE_STAGED_ALIAS_TYPE, STAGED_ALIAS_HEADER_SIZE);
        patch_u32(
            out,
            alias_writer.start_offset() + 8,
            self.aliases.len() as u32,
        );
        for (staged, finalized) in &self.aliases {
            push_u32(out, *staged);
            push_u32(out, *finalized);
        }
        alias_writer.finish(out);
    }
}

/// Port of `PackageFlattener::FlattenTypeSpec`: writes the typeSpec chunk
/// and returns its start offset (so `typesCount` can be patched later).
fn flatten_type_spec(
    ty: &TypeView,
    type_id: u8,
    num_entries: usize,
    buffer: &mut Vec<u8>,
) -> usize {
    let spec_writer = ChunkWriter::start(buffer, RES_TABLE_TYPE_SPEC_TYPE, TYPE_SPEC_HEADER_SIZE);
    let start = spec_writer.start_offset();
    buffer[start + 8] = type_id;

    if ty.entries.is_empty() {
        spec_writer.finish(buffer);
        return start;
    }

    patch_u32(buffer, start + 12, num_entries as u32); // entryCount

    // Reserve space for the configuration masks of each resource in this
    // type: which configuration axes the resource changes over, plus the
    // public/staged spec flags.
    let masks_off = buffer.len();
    buffer.resize(masks_off + num_entries * 4, 0);

    for entry in &ty.entries {
        let entry_id = entry.id.unwrap() as usize; // validated by the caller
        let slot = masks_off + entry_id * 4;
        let mut mask = read_u32(buffer, slot).unwrap_or(0);
        if entry.visibility.level == VisibilityLevel::Public {
            mask |= SPEC_PUBLIC;
        }
        if entry.visibility.staged_api {
            mask |= SPEC_STAGED_API;
        }
        let config_count = entry.values.len();
        for i in 0..config_count {
            let config = &entry.values[i].config;
            for j in (i + 1)..config_count {
                mask |= config.diff(&entry.values[j].config);
            }
        }
        patch_u32(buffer, slot, mask);
    }
    spec_writer.finish(buffer);
    start
}

/// Appends a `Res_value` (size 8, res0 0).
fn write_res_value(out: &mut Vec<u8>, value: ResValue) {
    push_u16(out, ResValue::SIZE);
    out.push(0); // res0
    out.push(value.data_type);
    push_u32(out, value.data);
}

/// Appends one `ResTable_map` row.
fn write_map_row(out: &mut Vec<u8>, key: u32, value: ResValue) {
    push_u32(out, key);
    write_res_value(out, value);
}

/// Port of `cmp_ids_dynamic_after_framework` (Resource.h): dynamic IDs
/// (package 0x00) sort after framework IDs (package 0x01) so the runtime
/// sees them sorted after dynamic-reference resolution.
fn cmp_ids_dynamic_after_framework(a: ResourceId, b: ResourceId) -> bool {
    if (a.package_id() == FRAMEWORK_PACKAGE_ID && b.package_id() == 0x00)
        || (a.package_id() == 0x00 && b.package_id() == FRAMEWORK_PACKAGE_ID)
    {
        b < a
    } else {
        a < b
    }
}

/// Port of `less_style_entries` (ResEntryWriter.cpp).
fn style_entry_less(a: &StyleEntry, b: &StyleEntry) -> bool {
    match (a.key.id, b.key.id) {
        (Some(a_id), Some(b_id)) => cmp_ids_dynamic_after_framework(a_id, b_id),
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => a.key.name < b.key.name,
    }
}

// ---------------------------------------------------------------------------
// Tests. The main build → flatten → parse round trips (plain/styled
// strings, configs, attributes, styles, arrays, plurals, ids, file
// references, public entries, compact + sparse modes) and the framework
// resources.arsc test live in `super::arsc_parser::tests`; these cover
// the flattener-specific chunks and behaviors.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::arsc_parser::parse_table;
    use super::*;
    use crate::res::string_pool::BinaryStringPool;
    use crate::res::table::{policy, NewResource, OnIdConflict};
    use crate::res::value::{format, Attribute, Item};

    fn string_item(s: &str) -> Item {
        Item::String {
            value: s.to_string(),
            untranslatable_sections: Vec::new(),
        }
    }

    fn add(table: &mut ResourceTable, name: &str, id: u32, value: Value) {
        let (name, _) = crate::res::parse_resource_name(name).expect("valid name");
        table
            .add_resource_overlay(
                NewResource::with_name(name)
                    .value(value)
                    .id_with_conflict(ResourceId(id), OnIdConflict::CreateEntry)
                    .allow_mangled(true),
            )
            .expect("add resource");
    }

    #[test]
    fn empty_table_round_trip() {
        let table = ResourceTable::new_unvalidated();
        let bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        assert_eq!(bytes.len() % 4, 0);
        let parsed = parse_table(&bytes).expect("parse empty table");
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn missing_resource_id_is_an_error() {
        let mut table = ResourceTable::new_unvalidated();
        let (name, _) = crate::res::parse_resource_name("com.app:string/x").unwrap();
        table
            .add_resource_overlay(NewResource::with_name(name).value(Value::item(string_item("x"))))
            .unwrap();
        let err = flatten_table(&table, &TableFlattenerOptions::default()).unwrap_err();
        assert!(err.to_string().contains("ID"), "{err}");
    }

    #[test]
    fn library_chunk_round_trip() {
        let mut table = ResourceTable::new_unvalidated();
        table.included_packages.push((0x05, "lib.five".to_string()));
        table.included_packages.push((0x02, "lib.two".to_string()));
        add(
            &mut table,
            "com.app:string/s",
            0x7f01_0000,
            Value::item(string_item("v")),
        );

        let bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        let parsed = parse_table(&bytes).expect("parse");
        let mut included = parsed.included_packages.clone();
        included.sort();
        assert_eq!(
            included,
            vec![
                (0x02, "lib.two".to_string()),
                (0x05, "lib.five".to_string())
            ]
        );
    }

    #[test]
    fn non_standard_package_id_gets_self_mapping() {
        let mut table = ResourceTable::new_unvalidated();
        add(
            &mut table,
            "com.feature:string/s",
            0x8001_0000,
            Value::item(string_item("v")),
        );

        let bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        let parsed = parse_table(&bytes).expect("parse");
        assert_eq!(
            parsed.included_packages,
            vec![(0x80, "com.feature".to_string())]
        );
        let (name, _) = crate::res::parse_resource_name("com.feature:string/s").unwrap();
        assert_eq!(
            parsed.find_resource(&name).unwrap().entry.id,
            Some(ResourceId(0x8001_0000))
        );
    }

    #[test]
    fn overlayable_round_trip() {
        let mut table = ResourceTable::new_unvalidated();
        add(
            &mut table,
            "com.app:string/a",
            0x7f01_0000,
            Value::item(string_item("a")),
        );
        add(
            &mut table,
            "com.app:string/b",
            0x7f01_0001,
            Value::item(string_item("b")),
        );
        add(
            &mut table,
            "com.app:string/c",
            0x7f01_0002,
            Value::item(string_item("c")),
        );

        table.overlayables.push(Overlayable {
            name: "ThemeGroup".to_string(),
            actor: "overlay://theme".to_string(),
            source: Default::default(),
        });
        let add_overlayable = |table: &mut ResourceTable, name: &str, id: u32, policies: u32| {
            let (name, _) = crate::res::parse_resource_name(name).unwrap();
            table
                .add_resource_overlay(
                    NewResource::with_name(name)
                        .id_with_conflict(ResourceId(id), OnIdConflict::CreateEntry)
                        .overlayable(OverlayableItem {
                            overlayable_index: 0,
                            policies,
                            ..Default::default()
                        })
                        .allow_mangled(true),
                )
                .unwrap();
        };
        add_overlayable(
            &mut table,
            "com.app:string/a",
            0x7f01_0000,
            policy::PUBLIC | policy::SYSTEM_PARTITION,
        );
        add_overlayable(
            &mut table,
            "com.app:string/b",
            0x7f01_0001,
            policy::PRODUCT_PARTITION,
        );
        add_overlayable(
            &mut table,
            "com.app:string/c",
            0x7f01_0002,
            policy::PUBLIC | policy::SYSTEM_PARTITION,
        );

        let bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        let parsed = parse_table(&bytes).expect("parse");

        assert_eq!(parsed.overlayables.len(), 1);
        assert_eq!(parsed.overlayables[0].name, "ThemeGroup");
        assert_eq!(parsed.overlayables[0].actor, "overlay://theme");

        for (name, policies) in [
            (
                "com.app:string/a",
                policy::PUBLIC | policy::SYSTEM_PARTITION,
            ),
            ("com.app:string/b", policy::PRODUCT_PARTITION),
            (
                "com.app:string/c",
                policy::PUBLIC | policy::SYSTEM_PARTITION,
            ),
        ] {
            let (resource_name, _) = crate::res::parse_resource_name(name).unwrap();
            let entry = parsed.find_resource(&resource_name).unwrap().entry;
            let item = entry
                .overlayable_item
                .as_ref()
                .unwrap_or_else(|| panic!("{name} lost its overlayable item"));
            assert_eq!(item.policies, policies, "policies of {name}");
            assert_eq!(
                parsed.overlayables[item.overlayable_index].name,
                "ThemeGroup"
            );
        }
    }

    #[test]
    fn missing_overlayable_policy_is_an_error() {
        let mut table = ResourceTable::new_unvalidated();
        table.overlayables.push(Overlayable {
            name: "G".to_string(),
            actor: String::new(),
            source: Default::default(),
        });
        let (name, _) = crate::res::parse_resource_name("com.app:string/a").unwrap();
        table
            .add_resource_overlay(
                NewResource::with_name(name)
                    .value(Value::item(string_item("a")))
                    .id_with_conflict(ResourceId(0x7f01_0000), OnIdConflict::CreateEntry)
                    .overlayable(OverlayableItem {
                        overlayable_index: 0,
                        ..Default::default()
                    })
                    .allow_mangled(true),
            )
            .unwrap();
        let err = flatten_table(&table, &TableFlattenerOptions::default()).unwrap_err();
        assert!(err.to_string().contains("policy"), "{err}");
    }

    #[test]
    fn staged_alias_round_trip() {
        let mut table = ResourceTable::new_unvalidated();
        add(
            &mut table,
            "com.app:string/s",
            0x7f01_0000,
            Value::item(string_item("v")),
        );
        add(
            &mut table,
            "com.app:string/other",
            0x7f01_0001,
            Value::item(string_item("w")),
        );
        let (name, _) = crate::res::parse_resource_name("com.app:string/s").unwrap();
        table
            .add_resource_overlay(
                NewResource::with_name(name.clone())
                    .id_with_conflict(ResourceId(0x7f01_0000), OnIdConflict::CreateEntry)
                    .staged_id(StagedId {
                        id: ResourceId(0x7f02_0000),
                        ..Default::default()
                    })
                    .allow_mangled(true),
            )
            .unwrap();

        let bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        let parsed = parse_table(&bytes).expect("parse");

        // One logical package; the alias clone (created under type ID 0x02
        // in an extracted package chunk) must have been removed again.
        assert_eq!(parsed.packages.len(), 1);
        let entry = parsed.find_resource(&name).unwrap().entry;
        assert_eq!(entry.id, Some(ResourceId(0x7f01_0000)));
        let staged = entry.staged_id.expect("staged id survived the round trip");
        assert_eq!(staged.id, ResourceId(0x7f02_0000));

        let string_type = parsed.packages[0]
            .types
            .iter()
            .find(|t| t.named_type.name == "string")
            .expect("string type");
        assert_eq!(
            string_type.entries.iter().filter(|e| e.name == "s").count(),
            1,
            "the staged-ID clone entry must be removed during parsing"
        );
    }

    #[test]
    fn collapse_key_stringpool_obfuscates_names() {
        let mut table = ResourceTable::new_unvalidated();
        add(
            &mut table,
            "com.app:string/first",
            0x7f01_0000,
            Value::item(string_item("a")),
        );
        add(
            &mut table,
            "com.app:string/second",
            0x7f01_0001,
            Value::item(string_item("b")),
        );

        let options = TableFlattenerOptions {
            collapse_key_stringpool: true,
            ..Default::default()
        };
        let bytes = flatten_table(&table, &options).unwrap();
        let parsed = parse_table(&bytes).expect("parse");

        let ty = parsed.packages[0]
            .types
            .iter()
            .find(|t| t.named_type.name == "string")
            .unwrap();
        assert_eq!(ty.entries.len(), 2);
        for entry in &ty.entries {
            assert_eq!(entry.name, OBFUSCATED_RESOURCE_NAME);
        }
    }

    #[test]
    fn attr_min_max_round_trip() {
        let mut table = ResourceTable::new_unvalidated();
        let mut attr = Attribute::new(format::INTEGER);
        attr.min_int = 0;
        attr.max_int = 100;
        add(
            &mut table,
            "com.app:attr/bounded",
            0x7f01_0000,
            Value::new(ValueKind::Attribute(attr)),
        );

        let bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        let parsed = parse_table(&bytes).expect("parse");
        let (name, _) = crate::res::parse_resource_name("com.app:attr/bounded").unwrap();
        let entry = parsed.find_resource(&name).unwrap().entry;
        match &entry.values[0].value.as_ref().unwrap().kind {
            ValueKind::Attribute(parsed_attr) => {
                assert_eq!(parsed_attr.type_mask, format::INTEGER);
                assert_eq!(parsed_attr.min_int, 0);
                assert_eq!(parsed_attr.max_int, 100);
            }
            other => panic!("unexpected value {other:?}"),
        }
    }

    #[test]
    fn type_id_gap_emits_placeholder_type_names() {
        let mut table = ResourceTable::new_unvalidated();
        // Type IDs 1 and 3, leaving a gap at 2 that the type pool must
        // fill with a "?2" placeholder for the id/index correspondence.
        add(&mut table, "com.app:array/a", 0x7f01_0000, {
            let mut array = crate::res::value::Array::default();
            array
                .elements
                .push(crate::res::value::ItemValue::new(string_item("x")));
            Value::new(ValueKind::Array(array))
        });
        add(
            &mut table,
            "com.app:string/s",
            0x7f03_0000,
            Value::item(string_item("v")),
        );

        let bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        let parsed = parse_table(&bytes).expect("parse");
        for (name, id) in [
            ("com.app:array/a", 0x7f01_0000u32),
            ("com.app:string/s", 0x7f03_0000),
        ] {
            let (resource_name, _) = crate::res::parse_resource_name(name).unwrap();
            let entry = parsed.find_resource(&resource_name).unwrap().entry;
            assert_eq!(entry.id, Some(ResourceId(id)), "{name}");
        }

        // The package's type pool (the 1st pool inside the package chunk)
        // carries ["array", "?2", "string"] in type-ID order.
        let mut iter = ChunkIterator::new(&bytes);
        let table_chunk = iter.next().unwrap();
        let package_chunk = table_chunk
            .children()
            .find(|c| c.type_id == RES_TABLE_PACKAGE_TYPE)
            .expect("package chunk");
        let type_pool_chunk = package_chunk
            .children()
            .find(|c| c.type_id == crate::res::string_pool::RES_STRING_POOL_TYPE)
            .expect("type pool");
        let type_pool = BinaryStringPool::parse(type_pool_chunk.data).expect("parse type pool");
        assert_eq!(type_pool.get(0).as_deref(), Some("array"));
        assert_eq!(type_pool.get(1).as_deref(), Some("?2"));
        assert_eq!(type_pool.get(2).as_deref(), Some("string"));
        assert!(!type_pool.is_utf8(), "type pool must be UTF-16 like aapt2");
    }

    #[test]
    fn value_pool_puts_file_references_first() {
        let mut table = ResourceTable::new_unvalidated();
        add(
            &mut table,
            "com.app:string/aaa",
            0x7f01_0000,
            Value::item(string_item("aaa text")),
        );
        add(
            &mut table,
            "com.app:drawable/icon",
            0x7f02_0000,
            Value::item(Item::FileReference(crate::res::value::FileReference {
                path: "res/drawable/icon.png".to_string(),
                file_type: crate::res::value::FileType::Png,
                file_contents: None,
            })),
        );

        let bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        let mut iter = ChunkIterator::new(&bytes);
        let table_chunk = iter.next().unwrap();
        let value_pool_chunk = table_chunk
            .children()
            .find(|c| c.type_id == crate::res::string_pool::RES_STRING_POOL_TYPE)
            .expect("value pool");
        let value_pool = BinaryStringPool::parse(value_pool_chunk.data).expect("parse value pool");
        assert!(value_pool.is_utf8(), "value pool must be UTF-8 like aapt2");
        // kHighPriority file paths sort before kNormalPriority strings.
        assert_eq!(value_pool.get(0).as_deref(), Some("res/drawable/icon.png"));
        assert_eq!(value_pool.get(1).as_deref(), Some("aaa text"));
    }
}
