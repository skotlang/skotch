//! Binary `resources.arsc` parser.
//!
//! Port of aapt2's `format/binary/BinaryResourceParser.{h,cpp}` (plus the
//! entry-iteration logic of `androidfw/TypeWrappers.cpp` and the value
//! decoding of `ResourceUtils::ParseBinaryResValue`): reads a flattened
//! resource table into the crate's [`ResourceTable`] model.
//!
//! Design notes mirroring the C++ parser:
//!
//! - The walk is RES_TABLE → (global value string pool, packages) →
//!   (type/key string pools, type specs, types, library, overlayable,
//!   staged aliases).
//! - Every parsed entry is inserted with
//!   `NewResource::id_with_conflict(id, OnIdConflict::CreateEntry)` via
//!   [`ResourceTable::add_resource_overlay`], the same insertion style the
//!   proto deserializer uses.
//! - After parsing, references that carry a resource ID belonging to this
//!   table get their symbolic name filled in (`ReferenceIdToNameVisitor`).
//! - All reads are bounds-checked; unknown chunk types and malformed
//!   entries are skipped, mirroring the original's warn-and-continue
//!   behavior. Parsing never panics on untrusted input.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};

use super::*;
use crate::res::config::ConfigDescription;
use crate::res::string_pool::BinaryStringPool;
use crate::res::table::{
    NewResource, OnIdConflict, Overlayable, OverlayableItem, ResourceTable, StagedId, Visibility,
    VisibilityLevel,
};
use crate::res::value::{
    res_value_type, Array, Attribute, AttributeSymbol, FileReference, FileType, Item, ItemValue,
    Plural, Reference, ReferenceType, ResValue, Span, Style, StyleEntry, Value, ValueKind,
    PLURAL_FEW, PLURAL_MANY, PLURAL_ONE, PLURAL_OTHER, PLURAL_TWO, PLURAL_ZERO,
};
use crate::res::{ResourceId, ResourceName, ResourceNamedType, ResourceType};

/// Parses a complete `resources.arsc` blob into a [`ResourceTable`].
///
/// Port of `BinaryResourceParser::Parse` + `ParseTable`.
pub fn parse_table(data: &[u8]) -> Result<ResourceTable> {
    let mut parser = Parser::new();

    let mut iter = ChunkIterator::new(data);
    let table_chunk = iter
        .next()
        .ok_or_else(|| anyhow!("corrupt resources.arsc: {}", iter.error.unwrap_or("empty input")))?;
    if table_chunk.type_id != RES_TABLE_TYPE {
        bail!("unknown chunk of type 0x{:04x}", table_chunk.type_id);
    }
    parser.parse_table_chunk(&table_chunk)?;
    // C++ warns about (but tolerates) trailing chunks after RES_TABLE_TYPE.

    if !parser.staged_entries_to_remove.is_empty() {
        bail!(
            "didn't find {} original staged resources",
            parser.staged_entries_to_remove.len()
        );
    }

    parser.fix_reference_names();
    Ok(parser.table)
}

/// One decoded `ResTable_map` row: `(name.ident, value.dataType, value.data)`.
type MapRow = (u32, u8, u32);

struct Parser {
    table: ResourceTable,
    /// The global value string pool (`value_pool_`).
    value_pool: Option<BinaryStringPool>,
    /// Per-entry `ResTable_typeSpec` flags (`entry_type_spec_flags_`).
    entry_type_spec_flags: HashMap<ResourceId, u32>,
    /// Resource ID → name mapping used to resolve references and
    /// overlayable/staged-alias targets (`id_index_`).
    id_index: HashMap<ResourceId, ResourceName>,
    /// Staged-alias clone entries that must be skipped when their type
    /// chunk is parsed after the alias chunk (`staged_entries_to_remove_`).
    staged_entries_to_remove: HashSet<(ResourceName, ResourceId)>,
}

impl Parser {
    fn new() -> Parser {
        Parser {
            // Deserialized tables skip entry-name validation, like the C++
            // `ResourceTable(Validation::kDisabled)` used for binary input.
            table: ResourceTable::new_unvalidated(),
            value_pool: None,
            entry_type_spec_flags: HashMap::new(),
            id_index: HashMap::new(),
            staged_entries_to_remove: HashSet::new(),
        }
    }

    /// Port of `BinaryResourceParser::ParseTable`.
    fn parse_table_chunk(&mut self, chunk: &Chunk) -> Result<()> {
        if (chunk.header_size as usize) < RES_TABLE_HEADER_SIZE as usize {
            bail!("corrupt ResTable_header chunk");
        }
        let mut children = chunk.children();
        while let Some(child) = children.next() {
            match child.type_id {
                RES_STRING_POOL_TYPE => {
                    if self.value_pool.is_none() {
                        self.value_pool = Some(
                            BinaryStringPool::parse(child.data)
                                .ok_or_else(|| anyhow!("corrupt string pool in ResTable"))?,
                        );
                    }
                    // else: "unexpected string pool in ResTable" (warn only).
                }
                RES_TABLE_PACKAGE_TYPE => self.parse_package(&child)?,
                _ => {
                    // "unexpected chunk type" — skip (warn only in C++).
                }
            }
        }
        if let Some(err) = children.error {
            bail!("corrupt resource table: {err}");
        }
        Ok(())
    }

    /// Port of `BinaryResourceParser::ParsePackage`.
    fn parse_package(&mut self, chunk: &Chunk) -> Result<()> {
        if chunk.header_size < PACKAGE_MIN_HEADER_SIZE {
            bail!("corrupt ResTable_package chunk");
        }
        let package_id = read_u32(chunk.data, 8).ok_or_else(|| anyhow!("corrupt package id"))?;
        if package_id > u8::MAX as u32 {
            bail!("package ID is too big ({package_id})");
        }
        let package_id = package_id as u8;
        let package_name = read_utf16_fixed(chunk.data, 12, 128);
        self.table.find_or_create_package(&package_name);

        // Type and key pools are per-package (cleared between packages).
        let mut type_pool: Option<BinaryStringPool> = None;
        let mut key_pool: Option<BinaryStringPool> = None;

        let mut children = chunk.children();
        while let Some(child) = children.next() {
            match child.type_id {
                RES_STRING_POOL_TYPE => {
                    if type_pool.is_none() {
                        type_pool = Some(BinaryStringPool::parse(child.data).ok_or_else(|| {
                            anyhow!("corrupt type string pool in ResTable_package")
                        })?);
                    } else if key_pool.is_none() {
                        key_pool = Some(BinaryStringPool::parse(child.data).ok_or_else(|| {
                            anyhow!("corrupt key string pool in ResTable_package")
                        })?);
                    }
                    // else: "unexpected string pool" — warn only.
                }
                RES_TABLE_TYPE_SPEC_TYPE => {
                    if type_pool.is_none() {
                        bail!("missing type string pool");
                    }
                    self.parse_type_spec(&child, package_id)?;
                }
                RES_TABLE_TYPE_TYPE => {
                    let (Some(types), Some(keys)) = (type_pool.as_ref(), key_pool.as_ref()) else {
                        bail!("missing type or key string pool");
                    };
                    self.parse_type(&child, &package_name, package_id, types, keys)?;
                }
                RES_TABLE_LIBRARY_TYPE => self.parse_library(&child)?,
                RES_TABLE_OVERLAYABLE_TYPE => self.parse_overlayable(&child)?,
                RES_TABLE_STAGED_ALIAS_TYPE => self.parse_staged_aliases(&child)?,
                _ => {
                    // "unexpected chunk type" — skip.
                }
            }
        }
        if let Some(err) = children.error {
            bail!("corrupt ResTable_package: {err}");
        }
        Ok(())
    }

    /// Port of `BinaryResourceParser::ParseTypeSpec`: records the per-entry
    /// spec flags so public/staged entries can be marked when the type
    /// chunks are parsed.
    fn parse_type_spec(&mut self, chunk: &Chunk, package_id: u8) -> Result<()> {
        let type_id =
            read_u8(chunk.data, 8).ok_or_else(|| anyhow!("corrupt ResTable_typeSpec chunk"))?;
        if type_id == 0 {
            bail!("ResTable_typeSpec has invalid id: 0");
        }
        let entry_count =
            read_u32(chunk.data, 12).ok_or_else(|| anyhow!("corrupt ResTable_typeSpec chunk"))?
                as usize;
        if entry_count > u16::MAX as usize {
            bail!("ResTable_typeSpec has too many entries ({entry_count})");
        }
        let data_size = chunk.payload().len();
        if entry_count * 4 > data_size {
            bail!("ResTable_typeSpec too small to hold entries");
        }
        let base = chunk.header_size as usize;
        for i in 0..entry_count {
            let Some(flags) = read_u32(chunk.data, base + i * 4) else { break };
            let id = ResourceId::new(package_id, type_id, i as u16);
            self.entry_type_spec_flags.insert(id, flags);
        }
        Ok(())
    }

    /// Port of `BinaryResourceParser::ParseType` plus the entry iteration
    /// of `TypeVariant` (sparse / 16-bit / 32-bit offset arrays).
    fn parse_type(
        &mut self,
        chunk: &Chunk,
        package_name: &str,
        package_id: u8,
        type_pool: &BinaryStringPool,
        key_pool: &BinaryStringPool,
    ) -> Result<()> {
        let d = chunk.data;
        if (chunk.header_size as usize) < TYPE_HEADER_MIN_SIZE {
            bail!("corrupt ResTable_type chunk");
        }
        let type_id = read_u8(d, 8).ok_or_else(|| anyhow!("corrupt ResTable_type chunk"))?;
        if type_id == 0 {
            bail!("ResTable_type has invalid id: 0");
        }
        let flags = read_u8(d, 9).unwrap_or(0);
        let entry_count =
            read_u32(d, 12).ok_or_else(|| anyhow!("corrupt ResTable_type chunk"))? as usize;
        let entries_start =
            read_u32(d, 16).ok_or_else(|| anyhow!("corrupt ResTable_type chunk"))? as usize;

        let config_bytes = d
            .get(TYPE_HEADER_PREFIX_SIZE..)
            .ok_or_else(|| anyhow!("corrupt ResTable_type chunk"))?;
        let config = ConfigDescription::from_bytes(config_bytes)
            .ok_or_else(|| anyhow!("corrupt ResTable_config in ResTable_type chunk"))?;

        // The type name lives in the type string pool at index id - 1, and
        // may carry a suffix like "^attr-private" or "string.v2".
        let type_str = type_pool.get((type_id - 1) as usize).unwrap_or_default();
        let Some(parsed_type) = ResourceNamedType::parse(&type_str) else {
            // "invalid type name … for type with ID …" — warn and skip.
            return Ok(());
        };

        for (entry_idx, entry_offset) in
            entry_offsets(d, chunk.header_size as usize, flags, entry_count)
        {
            let Some(parsed) = parse_entry_at(d, entries_start, entry_offset) else {
                // Malformed/out-of-bounds entry — skipped, like TypeVariant
                // returning NULL for the index.
                continue;
            };

            let key_str = key_pool.get(parsed.key as usize).unwrap_or_default();
            let name =
                ResourceName::with_named_type(package_name, parsed_type.clone(), key_str);
            let res_id = ResourceId::new(package_id, type_id, entry_idx);

            let mut value = match &parsed.body {
                EntryBody::Simple(res_value) => {
                    let item = self.parse_value(parsed_type.ty, res_value)?;
                    Value::item(item)
                }
                EntryBody::Map { parent, rows } => {
                    self.parse_map_entry(&name, *parent, rows).map_err(|e| {
                        anyhow!(
                            "failed to parse value for resource {name} ({res_id}) \
                             with configuration '{config}': {e}"
                        )
                    })?
                }
            };
            // C++ only surfaces FLAG_WEAK for attributes (`Attribute::SetWeak`);
            // we record it for every value so that weakness survives a
            // flatten → parse round trip.
            value.meta.weak = parsed.flags & ENTRY_FLAG_WEAK != 0;

            // Skip entries that were cloned under a staged alias ID and
            // already re-added under their finalized ID.
            if self.staged_entries_to_remove.remove(&(name.clone(), res_id)) {
                continue;
            }

            let mut res = NewResource::with_name(name.clone())
                .value(value)
                .config(config.clone())
                .id_with_conflict(res_id, OnIdConflict::CreateEntry)
                .allow_mangled(true);

            if parsed.flags & ENTRY_FLAG_PUBLIC != 0 {
                let mut visibility = Visibility {
                    level: VisibilityLevel::Public,
                    ..Default::default()
                };
                if let Some(spec_flags) = self.entry_type_spec_flags.get(&res_id) {
                    if spec_flags & SPEC_STAGED_API != 0 {
                        visibility.staged_api = true;
                    }
                }
                res = res.visibility(visibility);
                // Processed once; don't mark the same symbol again.
                self.entry_type_spec_flags.remove(&res_id);
            }

            self.id_index.entry(res_id).or_insert_with(|| name.clone());

            self.table
                .add_resource_overlay(res)
                .map_err(|e| anyhow!("{e}"))?;
        }
        Ok(())
    }

    /// Port of `ResourceUtils::ParseBinaryResValue`: a simple `Res_value`
    /// becomes an [`Item`]. String values resolve through the global value
    /// pool; styled strings keep their spans; non-`string` resources whose
    /// string starts with `res/` are reconstructed as file references.
    fn parse_value(&self, ty: ResourceType, value: &ResValue) -> Result<Item> {
        use res_value_type::*;

        if ty == ResourceType::Id
            && value.data_type != TYPE_REFERENCE
            && value.data_type != TYPE_DYNAMIC_REFERENCE
        {
            // Plain "id" resources are encoded as unused values (aapt1 used
            // an empty string, aapt2 a false boolean).
            return Ok(Item::Id);
        }

        match value.data_type {
            TYPE_STRING => {
                let pool = self
                    .value_pool
                    .as_ref()
                    .ok_or_else(|| anyhow!("string value without a global string pool"))?;
                let idx = value.data as usize;
                let s = pool.get(idx).unwrap_or_default();
                let spans = pool.spans(idx);
                if !spans.is_empty() {
                    let spans = spans
                        .into_iter()
                        .map(|(name_idx, first, last)| Span {
                            name: pool.get(name_idx as usize).unwrap_or_default(),
                            first_char: first,
                            last_char: last,
                        })
                        .collect();
                    return Ok(Item::StyledString {
                        value: s,
                        spans,
                        untranslatable_sections: Vec::new(),
                    });
                }
                if ty != ResourceType::String && s.starts_with("res/") {
                    // This must be a FileReference.
                    let file_type = if ty == ResourceType::Raw {
                        FileType::Unknown
                    } else if s.ends_with(".xml") {
                        FileType::BinaryXml
                    } else if s.ends_with(".png") {
                        FileType::Png
                    } else {
                        FileType::Unknown
                    };
                    return Ok(Item::FileReference(FileReference {
                        path: s,
                        file_type,
                        file_contents: None,
                    }));
                }
                Ok(Item::String {
                    value: s,
                    untranslatable_sections: Vec::new(),
                })
            }
            TYPE_REFERENCE | TYPE_ATTRIBUTE | TYPE_DYNAMIC_REFERENCE | TYPE_DYNAMIC_ATTRIBUTE => {
                if value.data == 0 {
                    // A reference of 0 must be the magic @null reference.
                    return Ok(Item::Reference(Reference::default()));
                }
                let reference_type =
                    if value.data_type == TYPE_ATTRIBUTE || value.data_type == TYPE_DYNAMIC_ATTRIBUTE
                    {
                        ReferenceType::Attribute
                    } else {
                        ReferenceType::Resource
                    };
                let is_dynamic = value.data_type == TYPE_DYNAMIC_REFERENCE
                    || value.data_type == TYPE_DYNAMIC_ATTRIBUTE;
                Ok(Item::Reference(Reference {
                    id: Some(ResourceId(value.data)),
                    reference_type,
                    is_dynamic,
                    ..Default::default()
                }))
            }
            _ => Ok(Item::BinaryPrimitive(*value)),
        }
    }

    /// Port of `BinaryResourceParser::ParseMapEntry`: dispatches a complex
    /// (bag) entry by resource type.
    fn parse_map_entry(&self, name: &ResourceName, parent: u32, rows: &[MapRow]) -> Result<Value> {
        match name.ty.ty {
            // configVarying is a legacy thing used in CTS tests.
            ResourceType::Style | ResourceType::ConfigVarying => self.parse_style(parent, rows),
            ResourceType::Attr | ResourceType::AttrPrivate => self.parse_attr(rows),
            ResourceType::Array => self.parse_array(rows),
            ResourceType::Plurals => self.parse_plural(rows),
            // Special case: some apps define the auto-generated IDs that come
            // from declaring an enum value in an attribute as an empty map.
            ResourceType::Id => Ok(Value::item(Item::Id)),
            _ => bail!("illegal map type '{}'", name.ty),
        }
    }

    /// Port of `BinaryResourceParser::ParseStyle`.
    fn parse_style(&self, parent: u32, rows: &[MapRow]) -> Result<Value> {
        let mut style = Style::default();
        if parent != 0 {
            style.parent = Some(Reference::from_id(ResourceId(parent)));
        }
        for &(ident, data_type, data) in rows {
            if is_internal_id(ident) {
                continue;
            }
            let item = self.parse_value(ResourceType::Style, &ResValue::new(data_type, data))?;
            style.entries.push(StyleEntry {
                key: Reference::from_id(ResourceId(ident)),
                value: ItemValue::new(item),
                source: Default::default(),
                comment: String::new(),
            });
        }
        Ok(Value::new(ValueKind::Style(style)))
    }

    /// Port of `BinaryResourceParser::ParseAttr`. The weak flag is applied
    /// by the caller from the entry flags.
    fn parse_attr(&self, rows: &[MapRow]) -> Result<Value> {
        // C++ Attribute's default type_mask is 0; ATTR_TYPE overrides it.
        let mut attr = Attribute::new(0);
        if let Some(&(_, _, data)) = rows.iter().find(|&&(ident, _, _)| ident == ATTR_TYPE) {
            attr.type_mask = data;
        }
        for &(ident, data_type, data) in rows {
            if is_internal_id(ident) {
                match ident {
                    ATTR_MIN => attr.min_int = data as i32,
                    ATTR_MAX => attr.max_int = data as i32,
                    _ => {}
                }
                continue;
            }
            if attr.type_mask & (crate::res::value::format::ENUM | crate::res::value::format::FLAGS)
                != 0
            {
                attr.symbols.push(AttributeSymbol {
                    symbol: Reference::from_id(ResourceId(ident)),
                    source: Default::default(),
                    comment: String::new(),
                    value: data,
                    data_type,
                });
            }
        }
        Ok(Value::new(ValueKind::Attribute(attr)))
    }

    /// Port of `BinaryResourceParser::ParseArray`.
    fn parse_array(&self, rows: &[MapRow]) -> Result<Value> {
        let mut array = Array::default();
        for &(_, data_type, data) in rows {
            let item = self.parse_value(ResourceType::Array, &ResValue::new(data_type, data))?;
            array.elements.push(ItemValue::new(item));
        }
        Ok(Value::new(ValueKind::Array(array)))
    }

    /// Port of `BinaryResourceParser::ParsePlural`.
    fn parse_plural(&self, rows: &[MapRow]) -> Result<Value> {
        let mut plural = Plural::default();
        for &(ident, data_type, data) in rows {
            let item = self.parse_value(ResourceType::Plurals, &ResValue::new(data_type, data))?;
            let slot = match ident {
                ATTR_ZERO => PLURAL_ZERO,
                ATTR_ONE => PLURAL_ONE,
                ATTR_TWO => PLURAL_TWO,
                ATTR_FEW => PLURAL_FEW,
                ATTR_MANY => PLURAL_MANY,
                ATTR_OTHER => PLURAL_OTHER,
                _ => continue,
            };
            plural.values[slot] = Some(ItemValue::new(item));
        }
        Ok(Value::new(ValueKind::Plural(plural)))
    }

    /// Port of `BinaryResourceParser::ParseLibrary` (`DynamicRefTable::load`):
    /// `ResTable_lib_header { count }` followed by `count` entries of
    /// `{ packageId: u32, packageName: u16[128] }`.
    fn parse_library(&mut self, chunk: &Chunk) -> Result<()> {
        let count =
            read_u32(chunk.data, 8).ok_or_else(|| anyhow!("corrupt ResTable_lib_header chunk"))?
                as usize;
        let base = chunk.header_size as usize;
        for i in 0..count {
            let off = base + i * LIB_ENTRY_SIZE;
            let Some(package_id) = read_u32(chunk.data, off) else { break };
            if package_id > u8::MAX as u32 {
                continue;
            }
            let name = read_utf16_fixed(chunk.data, off + 4, 128);
            let package_id = package_id as u8;
            // Last mapping for a package ID wins (KeyedVector::replaceValueFor).
            if let Some(existing) = self
                .table
                .included_packages
                .iter_mut()
                .find(|(id, _)| *id == package_id)
            {
                existing.1 = name;
            } else {
                self.table.included_packages.push((package_id, name));
            }
        }
        Ok(())
    }

    /// Port of `BinaryResourceParser::ParseOverlayable`.
    fn parse_overlayable(&mut self, chunk: &Chunk) -> Result<()> {
        if chunk.header_size < OVERLAYABLE_HEADER_SIZE {
            bail!("corrupt ResTable_overlayable_header chunk");
        }
        let overlayable = Overlayable {
            name: read_utf16_fixed(chunk.data, 8, 256),
            actor: read_utf16_fixed(chunk.data, 8 + 512, 256),
            source: Default::default(),
        };
        self.table.overlayables.push(overlayable);
        let overlayable_index = self.table.overlayables.len() - 1;

        let mut children = chunk.children();
        while let Some(child) = children.next() {
            if child.type_id != RES_TABLE_OVERLAYABLE_POLICY_TYPE {
                continue;
            }
            let policies = read_u32(child.data, 8)
                .ok_or_else(|| anyhow!("corrupt ResTable_overlayable_policy_header chunk"))?;
            let entry_count = read_u32(child.data, 12)
                .ok_or_else(|| anyhow!("corrupt ResTable_overlayable_policy_header chunk"))?
                as usize;
            let base = child.header_size as usize;
            for i in 0..entry_count {
                let Some(ident) = read_u32(child.data, base + i * 4) else { break };
                let res_id = ResourceId(ident);
                // If the overlayable chunk came before the type chunks the
                // id → name pairing would not exist; that is an error.
                let name = self
                    .id_index
                    .get(&res_id)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!("failed to find resource name for overlayable resource {res_id}")
                    })?;
                let item = OverlayableItem {
                    overlayable_index,
                    policies,
                    ..Default::default()
                };
                self.table
                    .add_resource_overlay(
                        NewResource::with_name(name)
                            .id_with_conflict(res_id, OnIdConflict::CreateEntry)
                            .overlayable(item)
                            .allow_mangled(true),
                    )
                    .map_err(|e| anyhow!("{e}"))?;
            }
        }
        Ok(())
    }

    /// Port of `BinaryResourceParser::ParseStagedAliases`: each alias maps a
    /// staged (pre-finalization) resource ID to its finalized ID. The
    /// finalized entry gets a [`StagedId`]; the cloned entry that was
    /// flattened under the staged ID is removed (or recorded to be skipped
    /// when its type chunk is parsed later).
    fn parse_staged_aliases(&mut self, chunk: &Chunk) -> Result<()> {
        let count = read_u32(chunk.data, 8)
            .ok_or_else(|| anyhow!("corrupt ResTable_staged_alias_header chunk"))?
            as usize;
        let base = chunk.header_size as usize;
        for i in 0..count {
            let off = base + i * STAGED_ALIAS_ENTRY_SIZE;
            let Some(staged) = read_u32(chunk.data, off) else { break };
            let Some(finalized) = read_u32(chunk.data, off + 4) else { break };
            let staged_id = ResourceId(staged);
            let finalized_id = ResourceId(finalized);

            let name = self.id_index.get(&finalized_id).cloned().ok_or_else(|| {
                anyhow!("failed to find resource name for finalized resource ID {finalized_id}")
            })?;

            self.table
                .add_resource_overlay(
                    NewResource::with_name(name.clone())
                        .id_with_conflict(finalized_id, OnIdConflict::CreateEntry)
                        .staged_id(StagedId {
                            id: staged_id,
                            ..Default::default()
                        })
                        .allow_mangled(true),
                )
                .map_err(|e| anyhow!("{e}"))?;

            // The finalized entry was cloned under the staged resource ID;
            // remove the clone (or remember to skip it while parsing).
            if !remove_resource(&mut self.table, &name, staged_id) {
                self.staged_entries_to_remove.insert((name, staged_id));
            }
        }
        Ok(())
    }

    /// Port of `ReferenceIdToNameVisitor`: fills in the symbolic name of
    /// every reference whose ID belongs to this table.
    fn fix_reference_names(&mut self) {
        let mapping = &self.id_index;
        let fix = |reference: &mut Reference| {
            let Some(id) = reference.id else { return };
            if !id.is_valid() {
                return;
            }
            if let Some(name) = mapping.get(&id) {
                reference.name = Some(name.clone());
            }
        };
        let fix_item = |item: &mut Item| {
            if let Item::Reference(reference) = item {
                fix(reference);
            }
        };
        for package in &mut self.table.packages {
            for ty in &mut package.types {
                for entry in &mut ty.entries {
                    for config_value in entry
                        .values
                        .iter_mut()
                        .chain(entry.flag_disabled_values.iter_mut())
                    {
                        let Some(value) = config_value.value.as_mut() else { continue };
                        match &mut value.kind {
                            ValueKind::Item(item) => fix_item(item),
                            ValueKind::Attribute(attr) => {
                                for symbol in &mut attr.symbols {
                                    fix(&mut symbol.symbol);
                                }
                            }
                            ValueKind::Style(style) => {
                                if let Some(parent) = style.parent.as_mut() {
                                    fix(parent);
                                }
                                for entry in &mut style.entries {
                                    fix(&mut entry.key);
                                    fix_item(&mut entry.value.item);
                                }
                            }
                            ValueKind::Styleable(styleable) => {
                                for reference in &mut styleable.entries {
                                    fix(reference);
                                }
                            }
                            ValueKind::Array(array) => {
                                for element in &mut array.elements {
                                    fix_item(&mut element.item);
                                }
                            }
                            ValueKind::Plural(plural) => {
                                for slot in plural.values.iter_mut().flatten() {
                                    fix_item(&mut slot.item);
                                }
                            }
                            ValueKind::Macro(_) => {}
                        }
                    }
                }
            }
        }
    }
}

/// Removes the entry called `name` whose assigned ID is exactly `id`.
/// Port of `ResourceTable::RemoveResource`.
fn remove_resource(table: &mut ResourceTable, name: &ResourceName, id: ResourceId) -> bool {
    let Some(package) = table.find_package_mut(&name.package) else {
        return false;
    };
    let Some(ty) = package.find_type_mut(&name.ty) else {
        return false;
    };
    if let Some(pos) = ty
        .entries
        .iter()
        .position(|e| e.name == name.entry && e.id == Some(id))
    {
        ty.entries.remove(pos);
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Entry iteration and decoding (port of TypeWrappers.cpp + ResTable_entry).
// ---------------------------------------------------------------------------

/// Yields `(entry_index, entry_offset)` pairs for a `ResTable_type` chunk,
/// handling the three offset encodings:
///
/// * `FLAG_SPARSE`: `entryCount` `ResTable_sparseTypeEntry` values, each
///   `{ idx: u16, offset: u16 }` with the real offset being `offset * 4`.
/// * `FLAG_OFFSET16`: `entryCount` u16 offsets in 4-byte units with
///   `0xffff` ([`NO_ENTRY16`]) meaning "no entry".
/// * default: `entryCount` u32 offsets with [`NO_ENTRY`] meaning "no entry".
///
/// An `entryCount` that overruns the chunk is clamped (the C++ TypeVariant
/// logs and bails; we degrade gracefully as required for real-world files).
fn entry_offsets(d: &[u8], header_size: usize, flags: u8, entry_count: usize) -> Vec<(u16, u32)> {
    let mut out = Vec::new();
    let avail = d.len().saturating_sub(header_size);
    if flags & FLAG_SPARSE != 0 {
        let count = entry_count.min(avail / 4);
        out.reserve(count);
        for i in 0..count {
            let Some(packed) = read_u32(d, header_size + i * 4) else { break };
            let idx = (packed & 0xFFFF) as u16;
            let offset = (packed >> 16) * 4;
            out.push((idx, offset));
        }
    } else if flags & FLAG_OFFSET16 != 0 {
        let count = entry_count.min(avail / 2).min(u16::MAX as usize + 1);
        out.reserve(count);
        for i in 0..count {
            let Some(off16) = read_u16(d, header_size + i * 2) else { break };
            if off16 == NO_ENTRY16 {
                continue;
            }
            out.push((i as u16, off16 as u32 * 4));
        }
    } else {
        let count = entry_count.min(avail / 4).min(u16::MAX as usize + 1);
        out.reserve(count);
        for i in 0..count {
            let Some(off) = read_u32(d, header_size + i * 4) else { break };
            if off == NO_ENTRY {
                continue;
            }
            out.push((i as u16, off));
        }
    }
    out
}

/// A decoded `ResTable_entry` (full, compact, or map form).
struct ParsedEntry {
    /// Key-pool index of the entry name.
    key: u32,
    /// `ResTable_entry` flags (for compact entries the data type lives in
    /// the high 8 bits and is already extracted into the body).
    flags: u16,
    body: EntryBody,
}

enum EntryBody {
    Simple(ResValue),
    Map { parent: u32, rows: Vec<MapRow> },
}

/// Decodes the entry at `entries_start + offset` within a `ResTable_type`
/// chunk. Returns `None` for out-of-bounds or malformed entries (which the
/// caller skips). Port of `ResTable_entry`'s accessors plus the validation
/// in `TypeVariant::iterator::operator*`.
fn parse_entry_at(d: &[u8], entries_start: usize, offset: u32) -> Option<ParsedEntry> {
    if offset == NO_ENTRY || offset & 0x3 != 0 {
        return None;
    }
    let pos = entries_start.checked_add(offset as usize)?;
    let flags = read_u16(d, pos + 2)?;

    if flags & ENTRY_FLAG_COMPACT != 0 {
        // Compact entry: { key: u16, flags: u16, data: u32 } with the data
        // type in the high 8 bits of flags. Compact entries are never
        // complex.
        let key = read_u16(d, pos)? as u32;
        let data = read_u32(d, pos + 4)?;
        let data_type = (flags >> 8) as u8;
        return Some(ParsedEntry {
            key,
            flags,
            body: EntryBody::Simple(ResValue::new(data_type, data)),
        });
    }

    let size = read_u16(d, pos)? as usize;
    if size < 8 || pos.checked_add(size)? > d.len() {
        return None;
    }
    let key = read_u32(d, pos + 4)?;

    if flags & ENTRY_FLAG_COMPLEX != 0 {
        // ResTable_map_entry: Full { size, flags, key } + parent + count,
        // followed (at pos + size) by `count` ResTable_map rows.
        let parent = read_u32(d, pos + 8)?;
        let count = read_u32(d, pos + 12)? as usize;
        let rows_base = pos + size;
        // Tolerate counts that overrun the chunk (the C++ has a TODO to
        // validate this); clamp to the available data.
        let max_rows = d.len().saturating_sub(rows_base) / 12;
        let mut rows = Vec::with_capacity(count.min(max_rows));
        for i in 0..count.min(max_rows) {
            let row = rows_base + i * 12;
            let ident = read_u32(d, row)?;
            let data_type = read_u8(d, row + 7)?;
            let data = read_u32(d, row + 8)?;
            rows.push((ident, data_type, data));
        }
        return Some(ParsedEntry {
            key,
            flags,
            body: EntryBody::Map { parent, rows },
        });
    }

    // Simple entry: Res_value follows at pos + size.
    let vpos = pos + size;
    let data_type = read_u8(d, vpos + 3)?;
    let data = read_u32(d, vpos + 4)?;
    Some(ParsedEntry {
        key,
        flags,
        body: EntryBody::Simple(ResValue::new(data_type, data)),
    })
}

// ---------------------------------------------------------------------------
// Tests: build → flatten → parse round trips + the real framework table.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::arsc_flattener::{flatten_table, TableFlattenerOptions};
    use super::*;
    use crate::res::table::ResourceEntry;
    use crate::res::value::format;

    fn config_de() -> ConfigDescription {
        ConfigDescription {
            language: *b"de",
            ..Default::default()
        }
    }

    fn config_v21() -> ConfigDescription {
        ConfigDescription {
            sdk_version: 21,
            ..Default::default()
        }
    }

    fn config_hdpi() -> ConfigDescription {
        ConfigDescription {
            density: 240,
            ..Default::default()
        }
    }

    fn string_item(s: &str) -> Item {
        Item::String {
            value: s.to_string(),
            untranslatable_sections: Vec::new(),
        }
    }

    fn primitive(data_type: u8, data: u32) -> Item {
        Item::BinaryPrimitive(ResValue::new(data_type, data))
    }

    fn add(
        table: &mut ResourceTable,
        name: &str,
        id: u32,
        config: ConfigDescription,
        value: Value,
    ) {
        let (name, _) = crate::res::parse_resource_name(name).expect("valid name");
        table
            .add_resource_overlay(
                NewResource::with_name(name)
                    .config(config)
                    .value(value)
                    .id_with_conflict(ResourceId(id), OnIdConflict::CreateEntry)
                    .allow_mangled(true),
            )
            .expect("add resource");
    }

    /// Builds the test table required by the round-trip test: strings with
    /// non-default configs and a styled string, an attr with enum symbols,
    /// a style with a parent, an array, plurals, a public entry, an id, and
    /// a file reference.
    fn build_test_table() -> ResourceTable {
        let mut table = ResourceTable::new_unvalidated();

        // array/nums (type 0x01)
        let mut array = Array::default();
        array
            .elements
            .push(ItemValue::new(primitive(res_value_type::TYPE_INT_DEC, 1)));
        array
            .elements
            .push(ItemValue::new(primitive(res_value_type::TYPE_INT_DEC, 2)));
        array.elements.push(ItemValue::new(string_item("three")));
        add(
            &mut table,
            "com.app:array/nums",
            0x7f01_0000,
            Default::default(),
            Value::new(ValueKind::Array(array)),
        );

        // attr/myAttr (type 0x02): enum attribute with two symbols.
        let mut attr = Attribute::new(format::ENUM | format::INTEGER);
        attr.symbols.push(AttributeSymbol {
            symbol: Reference::from_id(ResourceId(0x7f04_0000)),
            source: Default::default(),
            comment: String::new(),
            value: 1,
            data_type: res_value_type::TYPE_INT_DEC,
        });
        attr.symbols.push(AttributeSymbol {
            symbol: Reference::from_id(ResourceId(0x7f04_0001)),
            source: Default::default(),
            comment: String::new(),
            value: 2,
            data_type: res_value_type::TYPE_INT_DEC,
        });
        let mut attr_value = Value::new(ValueKind::Attribute(attr));
        attr_value.meta.weak = true; // attribute USE-style decl is weak
        add(
            &mut table,
            "com.app:attr/myAttr",
            0x7f02_0000,
            Default::default(),
            attr_value,
        );

        // drawable/icon (type 0x03): file reference.
        add(
            &mut table,
            "com.app:drawable/icon",
            0x7f03_0000,
            Default::default(),
            Value::item(Item::FileReference(FileReference {
                path: "res/drawable/icon.png".to_string(),
                file_type: FileType::Png,
                file_contents: None,
            })),
        );

        // id/one + id/two (type 0x04); id/one is public.
        add(
            &mut table,
            "com.app:id/one",
            0x7f04_0000,
            Default::default(),
            Value::item(Item::Id),
        );
        add(
            &mut table,
            "com.app:id/two",
            0x7f04_0001,
            Default::default(),
            Value::item(Item::Id),
        );
        let (id_one, _) = crate::res::parse_resource_name("com.app:id/one").unwrap();
        table
            .add_resource_overlay(
                NewResource::with_name(id_one)
                    .id_with_conflict(ResourceId(0x7f04_0000), OnIdConflict::CreateEntry)
                    .visibility(Visibility {
                        level: VisibilityLevel::Public,
                        ..Default::default()
                    })
                    .allow_mangled(true),
            )
            .unwrap();

        // plurals/count (type 0x05).
        let mut plural = Plural::default();
        plural.values[PLURAL_ONE] = Some(ItemValue::new(string_item("one item")));
        plural.values[PLURAL_OTHER] = Some(ItemValue::new(string_item("%d items")));
        add(
            &mut table,
            "com.app:plurals/count",
            0x7f05_0000,
            Default::default(),
            Value::new(ValueKind::Plural(plural)),
        );

        // string/title (type 0x06) in 4 configs, plus a styled string.
        add(
            &mut table,
            "com.app:string/title",
            0x7f06_0000,
            Default::default(),
            Value::item(string_item("Hello")),
        );
        add(
            &mut table,
            "com.app:string/title",
            0x7f06_0000,
            config_de(),
            Value::item(string_item("Hallo")),
        );
        add(
            &mut table,
            "com.app:string/title",
            0x7f06_0000,
            config_v21(),
            Value::item(string_item("Hello v21")),
        );
        add(
            &mut table,
            "com.app:string/title",
            0x7f06_0000,
            config_hdpi(),
            Value::item(string_item("Hello hdpi")),
        );
        add(
            &mut table,
            "com.app:string/styled",
            0x7f06_0001,
            Default::default(),
            Value::item(Item::StyledString {
                value: "Bold and italic".to_string(),
                spans: vec![
                    Span {
                        name: "b".to_string(),
                        first_char: 0,
                        last_char: 3,
                    },
                    Span {
                        name: "i".to_string(),
                        first_char: 9,
                        last_char: 14,
                    },
                ],
                untranslatable_sections: Vec::new(),
            }),
        );

        // style/AppTheme (type 0x07) with a parent and sorted-on-write
        // entries: a framework key (sorts first) and an app key.
        let mut style = Style::default();
        style.parent = Some(Reference::from_id(ResourceId(0x7f07_0001)));
        style.entries.push(StyleEntry {
            key: Reference::from_id(ResourceId(0x7f02_0000)),
            value: ItemValue::new(primitive(res_value_type::TYPE_INT_DEC, 1)),
            source: Default::default(),
            comment: String::new(),
        });
        style.entries.push(StyleEntry {
            key: Reference::from_id(ResourceId(0x0101_0001)),
            value: ItemValue::new(string_item("framework value")),
            source: Default::default(),
            comment: String::new(),
        });
        add(
            &mut table,
            "com.app:style/AppTheme",
            0x7f07_0000,
            Default::default(),
            Value::new(ValueKind::Style(style)),
        );
        add(
            &mut table,
            "com.app:style/Base",
            0x7f07_0001,
            Default::default(),
            Value::new(ValueKind::Style(Style::default())),
        );

        table
    }

    fn items_logically_equal(a: &Item, b: &Item) -> bool {
        match (a, b) {
            (Item::Reference(x), Item::Reference(y)) => {
                // Names may have been resolved from the id index on one
                // side; compare the binary-relevant fields only.
                x.id == y.id
                    && x.reference_type == y.reference_type
                    && x.is_dynamic == y.is_dynamic
            }
            (Item::Id, Item::Id) => true,
            (
                Item::String { value: x, .. },
                Item::String { value: y, .. },
            ) => x == y,
            (
                Item::StyledString {
                    value: x, spans: xs, ..
                },
                Item::StyledString {
                    value: y, spans: ys, ..
                },
            ) => x == y && xs == ys,
            (Item::FileReference(x), Item::FileReference(y)) => {
                x.path == y.path && x.file_type == y.file_type
            }
            (Item::BinaryPrimitive(x), Item::BinaryPrimitive(y)) => x == y,
            _ => false,
        }
    }

    fn values_logically_equal(a: &Value, b: &Value) -> bool {
        if a.meta.weak != b.meta.weak {
            return false;
        }
        match (&a.kind, &b.kind) {
            (ValueKind::Item(x), ValueKind::Item(y)) => items_logically_equal(x, y),
            (ValueKind::Attribute(x), ValueKind::Attribute(y)) => {
                x.type_mask == y.type_mask
                    && x.min_int == y.min_int
                    && x.max_int == y.max_int
                    && x.symbols.len() == y.symbols.len()
                    && x.symbols.iter().zip(&y.symbols).all(|(s, t)| {
                        s.symbol.id == t.symbol.id
                            && s.value == t.value
                            && s.data_type == t.data_type
                    })
            }
            (ValueKind::Style(x), ValueKind::Style(y)) => {
                let parent_eq = match (&x.parent, &y.parent) {
                    (Some(p), Some(q)) => p.id == q.id,
                    (None, None) => true,
                    _ => false,
                };
                // The flattener sorts style entries by key ID; compare as
                // sorted sets.
                let mut xs: Vec<_> = x.entries.iter().collect();
                let mut ys: Vec<_> = y.entries.iter().collect();
                xs.sort_by_key(|e| e.key.id.map(|id| id.0));
                ys.sort_by_key(|e| e.key.id.map(|id| id.0));
                parent_eq
                    && xs.len() == ys.len()
                    && xs.iter().zip(&ys).all(|(e, f)| {
                        e.key.id == f.key.id && items_logically_equal(&e.value.item, &f.value.item)
                    })
            }
            (ValueKind::Array(x), ValueKind::Array(y)) => {
                x.elements.len() == y.elements.len()
                    && x.elements
                        .iter()
                        .zip(&y.elements)
                        .all(|(e, f)| items_logically_equal(&e.item, &f.item))
            }
            (ValueKind::Plural(x), ValueKind::Plural(y)) => {
                x.values.iter().zip(&y.values).all(|(e, f)| match (e, f) {
                    (Some(e), Some(f)) => items_logically_equal(&e.item, &f.item),
                    (None, None) => true,
                    _ => false,
                })
            }
            _ => false,
        }
    }

    /// Asserts that every (name, id, config, value, visibility) of `orig`
    /// is present and logically identical in `parsed`.
    fn assert_logical_subset(orig: &ResourceTable, parsed: &ResourceTable) {
        for package in &orig.packages {
            for ty in &package.types {
                for entry in &ty.entries {
                    let name = ResourceName::with_named_type(
                        package.name.clone(),
                        ty.named_type.clone(),
                        entry.name.clone(),
                    );
                    let found = parsed
                        .find_resource(&name)
                        .unwrap_or_else(|| panic!("missing {name} after round trip"));
                    let found: &ResourceEntry = found.entry;
                    assert_eq!(found.id, entry.id, "id mismatch for {name}");
                    assert_eq!(
                        found.visibility.level, entry.visibility.level,
                        "visibility mismatch for {name}"
                    );
                    assert_eq!(
                        found.visibility.staged_api, entry.visibility.staged_api,
                        "staged_api mismatch for {name}"
                    );
                    for config_value in &entry.values {
                        let Some(value) = config_value.value.as_ref() else { continue };
                        let found_value = found
                            .find_value(&config_value.config, &config_value.product)
                            .and_then(|cv| cv.value.as_ref())
                            .unwrap_or_else(|| {
                                panic!(
                                    "missing value for {name} config '{}'",
                                    config_value.config
                                )
                            });
                        assert!(
                            values_logically_equal(value, found_value),
                            "value mismatch for {name} config '{}':\n  orig: {value:?}\n  parsed: {found_value:?}",
                            config_value.config
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn flatten_parse_round_trip() {
        let table = build_test_table();
        let bytes =
            flatten_table(&table, &TableFlattenerOptions::default()).expect("flatten table");
        assert_eq!(bytes.len() % 4, 0, "output must be 4-byte aligned");

        let parsed = parse_table(&bytes).expect("parse flattened table");
        assert_eq!(parsed.packages.len(), 1);
        assert_eq!(parsed.packages[0].name, "com.app");
        assert_logical_subset(&table, &parsed);

        // Spot-check the styled string's spans survived intact.
        let (styled_name, _) = crate::res::parse_resource_name("com.app:string/styled").unwrap();
        let styled = parsed.find_resource(&styled_name).unwrap();
        match &styled.entry.values[0].value.as_ref().unwrap().kind {
            ValueKind::Item(Item::StyledString { value, spans, .. }) => {
                assert_eq!(value, "Bold and italic");
                assert_eq!(spans.len(), 2);
                assert_eq!(spans[0].name, "b");
                assert_eq!((spans[0].first_char, spans[0].last_char), (0, 3));
            }
            other => panic!("unexpected styled value {other:?}"),
        }
    }

    #[test]
    fn compact_entries_round_trip() {
        let table = build_test_table();
        let options = TableFlattenerOptions {
            use_compact_entries: true,
            ..Default::default()
        };
        let bytes = flatten_table(&table, &options).expect("flatten table (compact)");
        let parsed = parse_table(&bytes).expect("parse compact table");
        assert_logical_subset(&table, &parsed);

        let default_bytes = flatten_table(&table, &TableFlattenerOptions::default()).unwrap();
        assert!(
            bytes.len() < default_bytes.len(),
            "compact entries should shrink the table ({} vs {})",
            bytes.len(),
            default_bytes.len()
        );
    }

    #[test]
    fn sparse_entries_round_trip() {
        // A mostly-empty type: 2 populated entries out of 10 (20% < the 60%
        // sparse-encoding threshold). minSdk gating is ignored at this layer.
        let mut table = ResourceTable::new_unvalidated();
        add(
            &mut table,
            "com.app:string/first",
            0x7f01_0000,
            Default::default(),
            Value::item(string_item("first")),
        );
        add(
            &mut table,
            "com.app:string/last",
            0x7f01_0009,
            Default::default(),
            Value::item(string_item("last")),
        );

        let sparse_options = TableFlattenerOptions {
            use_sparse_entries: true,
            ..Default::default()
        };
        let sparse_bytes = flatten_table(&table, &sparse_options).expect("flatten sparse");
        let dense_bytes =
            flatten_table(&table, &TableFlattenerOptions::default()).expect("flatten dense");
        assert!(
            sparse_bytes.len() < dense_bytes.len(),
            "sparse encoding should be smaller ({} vs {})",
            sparse_bytes.len(),
            dense_bytes.len()
        );

        for bytes in [&sparse_bytes, &dense_bytes] {
            let parsed = parse_table(bytes).expect("parse");
            assert_logical_subset(&table, &parsed);
            let (last, _) = crate::res::parse_resource_name("com.app:string/last").unwrap();
            let entry = parsed.find_resource(&last).expect("entry present");
            assert_eq!(entry.entry.id, Some(ResourceId(0x7f01_0009)));
        }
    }

    /// Parses the real Android framework table (40 MB, ~150k entries) out
    /// of the android-33.jar used by aapt2's integration tests. Skipped
    /// when the file is not present (e.g. in CI).
    #[test]
    fn parse_framework_resources_arsc() {
        use std::io::Read as _;

        let path = std::path::Path::new(
            "/opt/src/github/skotlang/android/base/tools/aapt2/integration-tests/CommandTests/android-33.jar",
        );
        if !path.exists() {
            return;
        }

        let file = std::fs::File::open(path).expect("open android-33.jar");
        let mut archive = zip::ZipArchive::new(file).expect("read jar as zip");
        let mut arsc = Vec::new();
        archive
            .by_name("resources.arsc")
            .expect("resources.arsc in jar")
            .read_to_end(&mut arsc)
            .expect("extract resources.arsc");

        let table = parse_table(&arsc).expect("parse framework table");

        // Exactly one package: "android", ID 0x01.
        assert_eq!(table.packages.len(), 1);
        let package = table.find_package("android").expect("android package");
        assert!(
            package.types.len() > 10,
            "expected many types, got {}",
            package.types.len()
        );

        let entry_count: usize = package.types.iter().map(|t| t.entries.len()).sum();
        assert!(
            entry_count > 10_000,
            "expected >10000 entries, got {entry_count}"
        );

        // Spot-check well-known resources and their IDs.
        for name in ["android:string/ok", "android:attr/label", "android:id/background"] {
            let (resource_name, _) = crate::res::parse_resource_name(name).unwrap();
            let found = table
                .find_resource(&resource_name)
                .unwrap_or_else(|| panic!("{name} missing from framework table"));
            let id = found.entry.id.unwrap_or_else(|| panic!("{name} has no ID"));
            assert!(id.is_valid_static(), "{name} has invalid id {id}");
            assert_eq!(id.package_id(), 0x01, "{name} not in package 0x01: {id}");
        }
    }
}
