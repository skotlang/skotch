//! `dump chunks`: port of `Debug::DumpChunks` (the `ChunkPrinter`),
//! a low-level walk over the chunks of a `resources.arsc` file.

use super::printer::Printer;
use super::values::pretty_print_item;
use crate::diag::Diagnostics;
use crate::res::config::ConfigDescription;
use crate::res::string_pool::BinaryStringPool;
use crate::res::value::{
    res_value_type, FileReference, FileType, Item, Reference, ReferenceType, ResValue, Span,
};
use crate::res::{ResourceId, ResourceType};

const RES_STRING_POOL_TYPE: u16 = 0x0001;
const RES_TABLE_TYPE: u16 = 0x0002;
const RES_TABLE_PACKAGE_TYPE: u16 = 0x0200;
const RES_TABLE_TYPE_TYPE: u16 = 0x0201;
const RES_TABLE_TYPE_SPEC_TYPE: u16 = 0x0202;
const RES_TABLE_LIBRARY_TYPE: u16 = 0x0203;

// ResTable_entry flags.
const FLAG_COMPLEX: u16 = 0x0001;
const FLAG_COMPACT: u16 = 0x0008;
// ResTable_type flags.
const FLAG_SPARSE: u8 = 0x01;
const FLAG_OFFSET16: u8 = 0x02;
// ResTable_typeSpec flags.
const SPEC_PUBLIC: u32 = 0x40000000;
const SPEC_STAGED_API: u32 = 0x20000000;

const NO_ENTRY: u32 = 0xffff_ffff;
const NO_ENTRY16: u16 = 0xffff;

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(offset..offset + 2)?.try_into().ok()?))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

struct ChunkPrinter<'a, 'p, 'd> {
    printer: &'p mut Printer<'a>,
    diag: &'d Diagnostics,
    value_pool: Option<(BinaryStringPool, usize)>,
    type_pool: Option<(BinaryStringPool, usize)>,
    key_pool: Option<(BinaryStringPool, usize)>,
}

fn pool_string(pool: &Option<(BinaryStringPool, usize)>, idx: u32) -> String {
    pool.as_ref()
        .and_then(|(p, _)| p.get(idx as usize))
        .unwrap_or_default()
}

impl ChunkPrinter<'_, '_, '_> {
    fn print_chunk_header(&mut self, chunk_type: u16, chunk_size: u32, header_size: u16) {
        let label = match chunk_type {
            RES_STRING_POOL_TYPE => "[RES_STRING_POOL_TYPE]",
            RES_TABLE_LIBRARY_TYPE => "[RES_TABLE_LIBRARY_TYPE]",
            RES_TABLE_TYPE => "[ResTable_header]",
            RES_TABLE_PACKAGE_TYPE => "[ResTable_package]",
            RES_TABLE_TYPE_TYPE => "[ResTable_type]",
            RES_TABLE_TYPE_SPEC_TYPE => "[RES_TABLE_TYPE_SPEC_TYPE]",
            _ => "",
        };
        self.printer.print(label);
        self.printer.print(format!(" chunkSize: {chunk_size}"));
        self.printer.print(format!(" headerSize: {header_size}"));
    }

    fn print_string_pool(&mut self, chunk: &[u8]) {
        // Initialize the pools in order: values, then types, then keys.
        let parsed = BinaryStringPool::parse(chunk).map(|p| (p, chunk.len()));
        let slot = if self.value_pool.is_none() {
            &mut self.value_pool
        } else if self.type_pool.is_none() {
            &mut self.type_pool
        } else if self.key_pool.is_none() {
            &mut self.key_pool
        } else {
            return;
        };
        *slot = parsed;
        let Some((pool, _)) = slot.as_ref() else {
            self.printer.print("\n");
            return;
        };

        self.printer.print(format!(
            " strings: {} styles {} flags: {}|{}\n",
            pool.len(),
            pool.style_count(),
            if pool.is_utf8() { "UTF-8" } else { "UTF-16" },
            if pool.is_sorted() { "SORTED" } else { "NON-SORTED" }
        ));

        let count = pool.len();
        let style_count = pool.style_count();
        let mut lines = String::new();
        for i in 0..count {
            lines.push_str(&format!("#{i} : {}\n", pool.get(i).unwrap_or_default()));
            if i < style_count {
                lines.push_str(" [Style] ");
                let spans = pool.spans(i);
                lines.push_str(&format!("({})", spans.len()));
                if !spans.is_empty() {
                    lines.push_str(" :");
                    for (name_idx, first, last) in &spans {
                        lines.push_str(&format!(
                            " {}:{},{}",
                            pool.get(*name_idx as usize).unwrap_or_default(),
                            first,
                            last
                        ));
                    }
                    lines.push('\n');
                }
            }
        }
        self.printer.print(lines);
    }

    /// Equivalent of `ResourceUtils::ParseBinaryResValue` for display.
    fn parse_res_value(&self, ty: ResourceType, value: &ResValue) -> Item {
        use res_value_type::*;
        match value.data_type {
            TYPE_STRING => {
                let idx = value.data as usize;
                let (s, spans) = match &self.value_pool {
                    Some((pool, _)) => (
                        pool.get(idx).unwrap_or_default(),
                        pool.spans(idx),
                    ),
                    None => (String::new(), Vec::new()),
                };
                if !spans.is_empty() {
                    let pool = self.value_pool.as_ref().map(|(p, _)| p);
                    return Item::StyledString {
                        value: s,
                        spans: spans
                            .into_iter()
                            .map(|(name_idx, first, last)| Span {
                                name: pool
                                    .and_then(|p| p.get(name_idx as usize))
                                    .unwrap_or_default(),
                                first_char: first,
                                last_char: last,
                            })
                            .collect(),
                        untranslatable_sections: Vec::new(),
                    };
                }
                if ty != ResourceType::String && s.starts_with("res/") {
                    let file_type = if ty == ResourceType::Raw {
                        FileType::Unknown
                    } else if s.ends_with(".xml") {
                        FileType::BinaryXml
                    } else if s.ends_with(".png") {
                        FileType::Png
                    } else {
                        FileType::Unknown
                    };
                    return Item::FileReference(FileReference {
                        path: s,
                        file_type,
                        file_contents: None,
                    });
                }
                Item::String { value: s, untranslatable_sections: Vec::new() }
            }
            TYPE_REFERENCE | TYPE_ATTRIBUTE | TYPE_DYNAMIC_REFERENCE | TYPE_DYNAMIC_ATTRIBUTE => {
                let reference_type = if value.data_type == TYPE_ATTRIBUTE
                    || value.data_type == TYPE_DYNAMIC_ATTRIBUTE
                {
                    ReferenceType::Attribute
                } else {
                    ReferenceType::Resource
                };
                Item::Reference(Reference {
                    id: if value.data != 0 { Some(ResourceId(value.data)) } else { None },
                    reference_type,
                    is_dynamic: matches!(
                        value.data_type,
                        TYPE_DYNAMIC_REFERENCE | TYPE_DYNAMIC_ATTRIBUTE
                    ),
                    ..Default::default()
                })
            }
            _ => Item::BinaryPrimitive(*value),
        }
    }

    fn print_res_value(&mut self, size: u16, value: &ResValue, ty: Option<ResourceType>) {
        self.printer.print("[Res_value]");
        self.printer.print(format!(" size: {size}"));
        self.printer.print(format!(" dataType: 0x{:02x}", value.data_type));
        self.printer.print(format!(" data: 0x{:08x}", value.data));
        if let Some(ty) = ty {
            let item = self.parse_res_value(ty, value);
            self.printer.print(" (");
            pretty_print_item(&item, self.printer);
            self.printer.print(")");
        }
        self.printer.print("\n");
    }

    fn print_qualifiers(&mut self, qualifiers: u32) {
        if qualifiers == 0 {
            self.printer.print("0");
            return;
        }
        self.printer.print(format!("0x{qualifiers:04x}: "));
        const VALUES: &[(u32, &str)] = &[
            (ConfigDescription::CONFIG_MCC, "mcc"),
            (ConfigDescription::CONFIG_MNC, "mnc"),
            (ConfigDescription::CONFIG_LOCALE, "locale"),
            (ConfigDescription::CONFIG_TOUCHSCREEN, "touchscreen"),
            (ConfigDescription::CONFIG_KEYBOARD, "keyboard"),
            (ConfigDescription::CONFIG_KEYBOARD_HIDDEN, "keyboard_hidden"),
            (ConfigDescription::CONFIG_NAVIGATION, "navigation"),
            (ConfigDescription::CONFIG_ORIENTATION, "orientation"),
            (ConfigDescription::CONFIG_DENSITY, "screen_density"),
            (ConfigDescription::CONFIG_SCREEN_SIZE, "screen_size"),
            (ConfigDescription::CONFIG_SMALLEST_SCREEN_SIZE, "screen_smallest_size"),
            (ConfigDescription::CONFIG_VERSION, "version"),
            (ConfigDescription::CONFIG_SCREEN_LAYOUT, "screen_layout"),
            (ConfigDescription::CONFIG_UI_MODE, "ui_mode"),
            (ConfigDescription::CONFIG_LAYOUTDIR, "layout_dir"),
            (ConfigDescription::CONFIG_SCREEN_ROUND, "screen_round"),
            (ConfigDescription::CONFIG_COLOR_MODE, "color_mode"),
            (ConfigDescription::CONFIG_GRAMMATICAL_GENDER, "grammatical_gender"),
        ];
        let mut delimiter = "";
        for (flag, name) in VALUES {
            if qualifiers & flag != 0 {
                self.printer.print(format!("{delimiter}{name}"));
                delimiter = "|";
            }
        }
    }

    fn print_type_spec(&mut self, chunk: &[u8], header_size: u16) -> bool {
        let id = chunk.get(8).copied().unwrap_or(0);
        let types_count = read_u16(chunk, 10).unwrap_or(0);
        let entry_count = read_u32(chunk, 12).unwrap_or(0);
        self.printer.print(format!(" id: 0x{id:02x}"));
        self.printer.print(format!(" types: {types_count}"));
        self.printer.print(format!(" entry configs: {entry_count}\n"));
        self.printer.print("Entry qualifier masks:\n");
        self.printer.indent();
        let data = &chunk[(header_size as usize).min(chunk.len())..];
        let mask_count = data.len() / 4;
        let mut non_empty_count = 0;
        for i in 0..mask_count {
            let mut mask = read_u32(data, i * 4).unwrap_or(0);
            if mask == 0 {
                continue;
            }
            non_empty_count += 1;
            self.printer.print(format!("#0x{i:02x} = "));
            if mask & SPEC_PUBLIC != 0 {
                mask &= !SPEC_PUBLIC;
                self.printer.print("(PUBLIC) ");
            }
            if mask & SPEC_STAGED_API != 0 {
                mask &= !SPEC_STAGED_API;
                self.printer.print("(STAGED) ");
            }
            self.print_qualifiers(mask);
            self.printer.print("\n");
        }
        if non_empty_count > 0 {
            self.printer.print("\n");
        } else {
            self.printer.print("(all empty)\n");
        }
        self.printer.undent();
        true
    }

    fn print_table_type(&mut self, chunk: &[u8], header_size: u16) -> bool {
        let id = chunk.get(8).copied().unwrap_or(0);
        let flags = chunk.get(9).copied().unwrap_or(0);
        let entry_count = read_u32(chunk, 12).unwrap_or(0) as usize;
        let entries_start = read_u32(chunk, 16).unwrap_or(0) as usize;
        let type_name = pool_string(&self.type_pool, id.wrapping_sub(1) as u32);

        self.printer.print(format!(" id: 0x{id:02x}"));
        self.printer.print(format!(" name: {type_name}"));
        self.printer.print(format!(" flags: 0x{flags:02x}"));
        self.printer.print(format!(" entryCount: {entry_count}"));
        self.printer.print(format!(" entryStart: {entries_start}"));

        let config = ConfigDescription::from_bytes(&chunk[20.min(chunk.len())..])
            .unwrap_or_default();
        self.printer.print(format!(" config: {config}\n"));

        let ty = ResourceType::parse(&type_name);

        self.printer.indent();

        // Iterate the entries (TypeVariant equivalent).
        let mut entries: Vec<(usize, usize)> = Vec::new(); // (entry index, offset)
        let offsets_start = header_size as usize;
        if flags & FLAG_SPARSE != 0 {
            // Sparse: pairs of (u16 idx, u16 offset/4).
            for i in 0..entry_count {
                let base = offsets_start + i * 4;
                let (Some(idx), Some(offset)) = (read_u16(chunk, base), read_u16(chunk, base + 2))
                else {
                    break;
                };
                entries.push((idx as usize, offset as usize * 4));
            }
        } else if flags & FLAG_OFFSET16 != 0 {
            for i in 0..entry_count {
                let Some(offset) = read_u16(chunk, offsets_start + i * 2) else { break };
                if offset != NO_ENTRY16 {
                    entries.push((i, offset as usize * 4));
                }
            }
        } else {
            for i in 0..entry_count {
                let Some(offset) = read_u32(chunk, offsets_start + i * 4) else { break };
                if offset != NO_ENTRY {
                    entries.push((i, offset as usize));
                }
            }
        }

        for (index, offset) in entries {
            let entry_offset = entries_start + offset;
            let Some(first) = read_u16(chunk, entry_offset) else { continue };
            let Some(entry_flags) = read_u16(chunk, entry_offset + 2) else { continue };
            let compact = entry_flags & FLAG_COMPACT != 0;
            let complex = !compact && (entry_flags & FLAG_COMPLEX != 0);

            let (size, key, printed_flags): (usize, u32, u16) = if compact {
                (8, first as u32, entry_flags)
            } else {
                (first as usize, read_u32(chunk, entry_offset + 4).unwrap_or(0), entry_flags)
            };

            if complex {
                self.printer.print("[ResTable_map_entry]");
            } else if compact {
                self.printer.print("[ResTable_entry_compact]");
            } else {
                self.printer.print("[ResTable_entry]");
            }
            self.printer.print(format!(" id: 0x{index:04x}"));
            self.printer
                .print(format!(" name: {}", pool_string(&self.key_pool, key)));
            self.printer.print(format!(" keyIndex: {key}"));
            self.printer.print(format!(" size: {size}"));
            self.printer.print(format!(" flags: 0x{printed_flags:04x}"));

            self.printer.indent();

            if complex {
                let parent = read_u32(chunk, entry_offset + 8).unwrap_or(0);
                let count = read_u32(chunk, entry_offset + 12).unwrap_or(0);
                self.printer.print(format!(" count: 0x{count:04x}"));
                self.printer.print(format!(" parent: 0x{parent:08x}\n"));

                // The name and value mappings.
                let maps_offset = entry_offset + size;
                for i in 0..count as usize {
                    let map_offset = maps_offset + i * 12;
                    let Some(name_ident) = read_u32(chunk, map_offset) else { break };
                    let value_size = read_u16(chunk, map_offset + 4).unwrap_or(0);
                    let data_type = chunk.get(map_offset + 7).copied().unwrap_or(0);
                    let data = read_u32(chunk, map_offset + 8).unwrap_or(0);
                    self.print_res_value(value_size, &ResValue::new(data_type, data), ty);
                    self.printer.print(format!(
                        " name: {} name-id:{}\n",
                        pool_string(&self.key_pool, name_ident),
                        name_ident
                    ));
                }
            } else {
                self.printer.print("\n");
                let (value_size, data_type, data) = if compact {
                    (8u16, (entry_flags >> 8) as u8, read_u32(chunk, entry_offset + 4).unwrap_or(0))
                } else {
                    let value_offset = entry_offset + size;
                    (
                        read_u16(chunk, value_offset).unwrap_or(0),
                        chunk.get(value_offset + 3).copied().unwrap_or(0),
                        read_u32(chunk, value_offset + 4).unwrap_or(0),
                    )
                };
                self.print_res_value(value_size, &ResValue::new(data_type, data), ty);
            }

            self.printer.undent();
        }

        self.printer.undent();
        true
    }

    fn print_package(&mut self, chunk: &[u8], header_size: u16) -> bool {
        let id = read_u32(chunk, 8).unwrap_or(0);
        self.printer.print(format!(" id: 0x{id:02x}"));

        // Package name: NUL-terminated UTF-16, 128 code units at offset 12.
        let mut name = String::new();
        for i in 0..128usize {
            let Some(c) = read_u16(chunk, 12 + i * 2) else { break };
            if c == 0 {
                break;
            }
            name.extend(char::decode_utf16([c]).map(|r| r.unwrap_or('\u{fffd}')));
        }

        self.printer.print(format!("name: {name}"));
        self.printer
            .print(format!(" typeStrings: {}", read_u32(chunk, 268).unwrap_or(0)));
        self.printer
            .print(format!(" lastPublicType: {}", read_u32(chunk, 272).unwrap_or(0)));
        self.printer
            .print(format!(" keyStrings: {}", read_u32(chunk, 276).unwrap_or(0)));
        self.printer
            .print(format!(" lastPublicKey: {}", read_u32(chunk, 280).unwrap_or(0)));
        self.printer
            .print(format!(" typeIdOffset: {}\n", read_u32(chunk, 284).unwrap_or(0)));

        // The chunks contained within the package.
        self.printer.indent();
        let success = self.print_chunks(&chunk[(header_size as usize).min(chunk.len())..]);
        self.printer.undent();
        success
    }

    fn print_table(&mut self, chunk: &[u8], header_size: u16) -> bool {
        let package_count = read_u32(chunk, 8).unwrap_or(0);
        self.printer.print(format!(" Package count: {package_count}\n"));
        self.printer.indent();
        let success = self.print_chunks(&chunk[(header_size as usize).min(chunk.len())..]);
        self.printer.undent();
        success
    }

    fn print_chunks(&mut self, data: &[u8]) -> bool {
        let mut offset = 0usize;
        while offset + 8 <= data.len() {
            let chunk_type = read_u16(data, offset).unwrap_or(0);
            let header_size = read_u16(data, offset + 2).unwrap_or(0);
            let chunk_size = read_u32(data, offset + 4).unwrap_or(0) as usize;
            if chunk_size < 8 || offset + chunk_size > data.len() || (header_size as usize) > chunk_size
            {
                self.diag.error("corrupt resource table: chunk sizes are corrupt");
                return false;
            }
            let chunk = &data[offset..offset + chunk_size];

            self.print_chunk_header(chunk_type, chunk_size as u32, header_size);
            match chunk_type {
                RES_STRING_POOL_TYPE => self.print_string_pool(chunk),
                RES_TABLE_TYPE => {
                    self.print_table(chunk, header_size);
                }
                RES_TABLE_PACKAGE_TYPE => {
                    self.type_pool = None;
                    self.key_pool = None;
                    self.print_package(chunk, header_size);
                }
                RES_TABLE_TYPE_TYPE => {
                    self.print_table_type(chunk, header_size);
                }
                RES_TABLE_TYPE_SPEC_TYPE => {
                    self.print_type_spec(chunk, header_size);
                }
                _ => {
                    self.printer.print("\n");
                }
            }
            offset += chunk_size;
        }
        true
    }
}

/// Port of `Debug::DumpChunks`.
pub fn dump_chunks(data: &[u8], printer: &mut Printer, diag: &Diagnostics) {
    let mut chunk_printer = ChunkPrinter {
        printer,
        diag,
        value_pool: None,
        type_pool: None,
        key_pool: None,
    };
    chunk_printer.print_chunks(data);
    chunk_printer.printer.print("[End]\n");
}
