//! Conversion between the in-memory resource model and the
//! `aapt.pb.*` protobuf messages.
//!
//! Port of `format/proto/ProtoSerialize.cpp` and
//! `format/proto/ProtoDeserialize.cpp`, except the protobuf messages
//! are never materialized: the model encodes straight to wire bytes
//! (fields written in ascending field-number order, matching the C++
//! protobuf serializer) and decodes straight from them.

pub mod wire;

use crate::res::config::ConfigDescription;
use crate::res::string_pool::{BinaryStringPool, StringPool};
use crate::res::table::{
    AllowNew, NewResource, OnIdConflict, Overlayable, OverlayableItem, ResourceTable, StagedId,
    Visibility, VisibilityLevel,
};
use crate::res::value::{
    res_value_type, Array, Attribute, AttributeSymbol, FileReference, FileType, Item, ItemValue,
    Macro, MacroNamespace, Plural, Reference, ReferenceType, ResValue, Span, Style, StyleEntry,
    StyleString, StyleStringSpan, Styleable, UntranslatableSection, Value, ValueKind, ValueMeta,
};
use crate::res::{
    parse_resource_name, FeatureFlagAttribute, FlagStatus, ResourceFile, ResourceId, ResourceName,
    Source, SourcedResourceName,
};
use anyhow::{anyhow, Context, Result};
use wire::{Reader, Writer, WIRE_LEN};

// Re-export for elsewhere in the crate.
pub use crate::res::value as value_model;

/// The tool fingerprint written into serialized tables.
pub fn tool_fingerprint() -> (String, String) {
    ("Android Asset Packaging Tool (aapt)".to_string(), format!("skotch-{}", env!("CARGO_PKG_VERSION")))
}

// ───────────────────────── source pool ─────────────────────────

/// Builds the `source_pool` (a binary `ResStringPool` of source paths)
/// while encoding values.
#[derive(Default)]
struct SourcePathPool {
    pool: StringPool,
}

impl SourcePathPool {
    fn index_of(&mut self, path: &str) -> u32 {
        // The pool is never sorted, so the final index equals the
        // insertion index and can be resolved immediately.
        let reference = self.pool.make_ref(
            path,
            crate::res::string_pool::Context::with_priority(
                crate::res::string_pool::Context::NORMAL_PRIORITY,
            ),
        );
        self.pool.resolve(reference) as u32
    }

    fn flatten(&self) -> Vec<u8> {
        self.pool.flatten_utf8()
    }
}

fn encode_source(w: &mut Writer, field: u32, source: &Source, pool: &mut SourcePathPool) {
    let path_idx = pool.index_of(&source.path);
    let line = source.line.unwrap_or(0);
    if path_idx == 0 && line == 0 {
        // An all-default Source message still marks presence in the C++
        // serializer (mutable_source()), so write an empty message.
        w.bytes_always(field, &[]);
        return;
    }
    w.message(field, |sw| {
        sw.varint(1, path_idx as u64);
        if line != 0 {
            sw.message(2, |pw| {
                pw.varint(1, line as u64);
            });
        }
    });
}

fn decode_source(data: &[u8], pool: Option<&BinaryStringPool>) -> Source {
    let mut source = Source::default();
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, _) => {
                if let Some(pool) = pool {
                    if let Some(path) = pool.get(field.value as usize) {
                        source.path = path;
                    }
                }
            }
            (2, WIRE_LEN) => {
                let mut sub = Reader::new(field.data);
                while let Some(pos) = sub.next_field() {
                    if pos.number == 1 && pos.value != 0 {
                        source.line = Some(pos.value as usize);
                    }
                }
            }
            _ => {}
        }
    }
    source
}

// ───────────────────────── configuration ─────────────────────────

/// Encodes a `ConfigDescription` (+ product) as an `aapt.pb.Configuration`
/// message body. Port of `SerializeConfig`.
pub fn encode_config(w: &mut Writer, config: &ConfigDescription, product: &str) {
    use crate::res::config::ConfigDescription as C;

    w.varint(1, config.mcc as u64);
    w.varint(2, config.mnc as u64);
    w.string(3, &config.get_bcp47_locale(false));

    // LayoutDirection: LTR=1, RTL=2.
    w.varint(
        4,
        match config.screen_layout & C::MASK_LAYOUTDIR {
            C::LAYOUTDIR_LTR => 1,
            C::LAYOUTDIR_RTL => 2,
            _ => 0,
        },
    );

    w.varint(5, config.screen_width as u64);
    w.varint(6, config.screen_height as u64);
    w.varint(7, config.screen_width_dp as u64);
    w.varint(8, config.screen_height_dp as u64);
    w.varint(9, config.smallest_screen_width_dp as u64);

    // ScreenLayoutSize: SMALL=1..XLARGE=4 (binary values already 1..4).
    w.varint(10, (config.screen_layout & C::MASK_SCREENSIZE) as u64);
    // ScreenLayoutLong: LONG=1, NOTLONG=2.
    w.varint(
        11,
        match config.screen_layout & C::MASK_SCREENLONG {
            C::SCREENLONG_YES => 1,
            C::SCREENLONG_NO => 2,
            _ => 0,
        },
    );
    // ScreenRound: ROUND=1, NOTROUND=2.
    w.varint(
        12,
        match config.screen_layout2 & C::MASK_SCREENROUND {
            C::SCREENROUND_YES => 1,
            C::SCREENROUND_NO => 2,
            _ => 0,
        },
    );
    // WideColorGamut: WIDECG=1, NOWIDECG=2.
    w.varint(
        13,
        match config.color_mode & C::MASK_WIDE_COLOR_GAMUT {
            C::WIDE_COLOR_GAMUT_YES => 1,
            C::WIDE_COLOR_GAMUT_NO => 2,
            _ => 0,
        },
    );
    // Hdr: HIGHDR=1, LOWDR=2.
    w.varint(
        14,
        match config.color_mode & C::MASK_HDR {
            C::HDR_YES => 1,
            C::HDR_NO => 2,
            _ => 0,
        },
    );
    // Orientation: PORT=1, LAND=2, SQUARE=3 (same values).
    w.varint(15, config.orientation as u64);
    // UiModeType: 1..7 (same values).
    w.varint(16, (config.ui_mode & C::MASK_UI_MODE_TYPE) as u64);
    // UiModeNight: NIGHT=1, NOTNIGHT=2.
    w.varint(
        17,
        match config.ui_mode & C::MASK_UI_MODE_NIGHT {
            C::UI_MODE_NIGHT_YES => 1,
            C::UI_MODE_NIGHT_NO => 2,
            _ => 0,
        },
    );
    w.varint(18, config.density as u64);
    // Touchscreen: NOTOUCH=1, STYLUS=2, FINGER=3 (same values).
    w.varint(19, config.touchscreen as u64);
    // KeysHidden: KEYSEXPOSED=1, KEYSHIDDEN=2, KEYSSOFT=3 (same values).
    w.varint(20, (config.input_flags & C::MASK_KEYSHIDDEN) as u64);
    // Keyboard: NOKEYS=1, QWERTY=2, TWELVEKEY=3 (same values).
    w.varint(21, config.keyboard as u64);
    // NavHidden: NAVEXPOSED=1, NAVHIDDEN=2.
    w.varint(
        22,
        match config.input_flags & C::MASK_NAVHIDDEN {
            C::NAVHIDDEN_NO => 1,
            C::NAVHIDDEN_YES => 2,
            _ => 0,
        },
    );
    // Navigation: NONAV=1..WHEEL=4 (same values).
    w.varint(23, config.navigation as u64);
    w.varint(24, config.sdk_version as u64);
    w.string(25, product);
    // GrammaticalGender: constant values match between the structs.
    w.varint(26, config.grammatical_inflection as u64);
}

/// Decodes an `aapt.pb.Configuration` message body into a
/// `ConfigDescription` plus product string. Port of
/// `DeserializeConfigFromPb`.
pub fn decode_config(data: &[u8]) -> Result<(ConfigDescription, String)> {
    use crate::res::config::ConfigDescription as C;

    let mut config = ConfigDescription::default();
    let mut product = String::new();
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match field.number {
            1 => config.mcc = field.value as u16,
            2 => config.mnc = field.value as u16,
            3 => {
                if !field.data.is_empty() {
                    config.set_bcp47_locale(field.as_str());
                }
            }
            4 => {
                config.screen_layout = (config.screen_layout & !C::MASK_LAYOUTDIR)
                    | match field.value {
                        1 => C::LAYOUTDIR_LTR,
                        2 => C::LAYOUTDIR_RTL,
                        _ => 0,
                    }
            }
            5 => config.screen_width = field.value as u16,
            6 => config.screen_height = field.value as u16,
            7 => config.screen_width_dp = field.value as u16,
            8 => config.screen_height_dp = field.value as u16,
            9 => config.smallest_screen_width_dp = field.value as u16,
            10 => {
                config.screen_layout = (config.screen_layout & !C::MASK_SCREENSIZE)
                    | ((field.value as u8) & C::MASK_SCREENSIZE)
            }
            11 => {
                config.screen_layout = (config.screen_layout & !C::MASK_SCREENLONG)
                    | match field.value {
                        1 => C::SCREENLONG_YES,
                        2 => C::SCREENLONG_NO,
                        _ => 0,
                    }
            }
            12 => {
                config.screen_layout2 = (config.screen_layout2 & !C::MASK_SCREENROUND)
                    | match field.value {
                        1 => C::SCREENROUND_YES,
                        2 => C::SCREENROUND_NO,
                        _ => 0,
                    }
            }
            13 => {
                config.color_mode = (config.color_mode & !C::MASK_WIDE_COLOR_GAMUT)
                    | match field.value {
                        1 => C::WIDE_COLOR_GAMUT_YES,
                        2 => C::WIDE_COLOR_GAMUT_NO,
                        _ => 0,
                    }
            }
            14 => {
                config.color_mode = (config.color_mode & !C::MASK_HDR)
                    | match field.value {
                        1 => C::HDR_YES,
                        2 => C::HDR_NO,
                        _ => 0,
                    }
            }
            15 => config.orientation = field.value as u8,
            16 => {
                config.ui_mode = (config.ui_mode & !C::MASK_UI_MODE_TYPE)
                    | ((field.value as u8) & C::MASK_UI_MODE_TYPE)
            }
            17 => {
                config.ui_mode = (config.ui_mode & !C::MASK_UI_MODE_NIGHT)
                    | match field.value {
                        1 => C::UI_MODE_NIGHT_YES,
                        2 => C::UI_MODE_NIGHT_NO,
                        _ => 0,
                    }
            }
            18 => config.density = field.value as u16,
            19 => config.touchscreen = field.value as u8,
            20 => {
                config.input_flags = (config.input_flags & !C::MASK_KEYSHIDDEN)
                    | ((field.value as u8) & C::MASK_KEYSHIDDEN)
            }
            21 => config.keyboard = field.value as u8,
            22 => {
                config.input_flags = (config.input_flags & !C::MASK_NAVHIDDEN)
                    | match field.value {
                        1 => C::NAVHIDDEN_NO,
                        2 => C::NAVHIDDEN_YES,
                        _ => 0,
                    }
            }
            23 => config.navigation = field.value as u8,
            24 => config.sdk_version = field.value as u16,
            25 => product = field.as_string(),
            26 => config.grammatical_inflection = field.value as u8,
            _ => {}
        }
    }
    Ok((config, product))
}

// ───────────────────────── references / items ─────────────────────────

fn encode_reference(w: &mut Writer, reference: &Reference) {
    // pb::Reference: type=1, id=2, name=3, private=4, is_dynamic=5,
    // type_flags=6, allow_raw=7.
    if reference.reference_type == ReferenceType::Attribute {
        w.varint(1, 1);
    }
    if let Some(id) = reference.id {
        w.varint(2, id.0 as u64);
    }
    if let Some(name) = &reference.name {
        w.string(3, &name.to_string());
    }
    w.bool(4, reference.private_reference);
    if reference.is_dynamic {
        w.message(5, |bw| bw.bool(1, true));
    }
    if let Some(flags) = reference.type_flags {
        w.varint(6, flags as u64);
    }
    w.bool(7, reference.allow_raw);
}

fn decode_reference(data: &[u8]) -> Result<Reference> {
    let mut reference = Reference::default();
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match field.number {
            1 => {
                reference.reference_type = if field.value == 1 {
                    ReferenceType::Attribute
                } else {
                    ReferenceType::Resource
                }
            }
            2 => {
                if field.value != 0 {
                    reference.id = Some(ResourceId(field.value as u32));
                }
            }
            3 => {
                if !field.data.is_empty() {
                    let raw = field.as_str();
                    let (name, _) = parse_resource_name(raw)
                        .ok_or_else(|| anyhow!("invalid reference name '{raw}'"))?;
                    reference.name = Some(name);
                }
            }
            4 => reference.private_reference = field.as_bool(),
            5 => {
                let mut sub = Reader::new(field.data);
                while let Some(b) = sub.next_field() {
                    if b.number == 1 {
                        reference.is_dynamic = b.as_bool();
                    }
                }
            }
            6 => reference.type_flags = Some(field.value as u32),
            7 => reference.allow_raw = field.as_bool(),
            _ => {}
        }
    }
    Ok(reference)
}

/// Encodes an [`Item`] as the body of an `aapt.pb.Item` message
/// (without flag fields). Used for style entries, arrays, plurals, and
/// compiled XML attribute values.
pub fn encode_item(w: &mut Writer, item: &Item) {
    match item {
        Item::Reference(reference) => w.message(1, |rw| encode_reference(rw, reference)),
        Item::String { value, .. } => w.message(2, |sw| sw.string(1, value)),
        Item::RawString(value) => w.message(3, |sw| sw.string(1, value)),
        Item::StyledString { value, spans, .. } => w.message(4, |sw| {
            sw.string(1, value);
            for span in spans {
                sw.message(2, |pw| {
                    pw.string(1, &span.name);
                    pw.varint(2, span.first_char as u64);
                    pw.varint(3, span.last_char as u64);
                });
            }
        }),
        Item::FileReference(file) => w.message(5, |fw| {
            fw.string(1, &file.path);
            fw.varint(
                2,
                match file.file_type {
                    FileType::Unknown => 0,
                    FileType::Png => 1,
                    FileType::BinaryXml => 2,
                    FileType::ProtoXml => 3,
                },
            );
        }),
        Item::Id => w.bytes_always(6, &[]),
        Item::BinaryPrimitive(value) => w.message(7, |pw| encode_primitive(pw, value)),
    }
}

fn encode_item_with_flags(w: &mut Writer, item: &Item, meta: Option<&ValueMeta>) {
    encode_item(w, item);
    if let Some(meta) = meta {
        w.varint(8, meta.flag_status as u64);
        if let Some(flag) = &meta.flag {
            w.bool(9, flag.negated);
            w.string(10, &flag.name);
        }
    }
}

fn encode_primitive(w: &mut Writer, value: &ResValue) {
    use res_value_type::*;
    match value.data_type {
        TYPE_NULL => {
            if value.data == crate::res::value::DATA_NULL_EMPTY {
                w.bytes_always(2, &[]);
            } else {
                w.bytes_always(1, &[]);
            }
        }
        TYPE_FLOAT => w.float_always(3, f32::from_bits(value.data)),
        TYPE_DIMENSION => w.varint_always(13, value.data as u64),
        TYPE_FRACTION => w.varint_always(14, value.data as u64),
        TYPE_INT_DEC => w.int32_always(6, value.data as i32),
        TYPE_INT_HEX => w.varint_always(7, value.data as u64),
        TYPE_INT_BOOLEAN => w.bool_always(8, value.data != 0),
        TYPE_INT_COLOR_ARGB8 => w.varint_always(9, value.data as u64),
        TYPE_INT_COLOR_RGB8 => w.varint_always(10, value.data as u64),
        TYPE_INT_COLOR_ARGB4 => w.varint_always(11, value.data as u64),
        TYPE_INT_COLOR_RGB4 => w.varint_always(12, value.data as u64),
        other => {
            // Unexpected types round-trip as hex ints to avoid data loss.
            debug_assert!(false, "unexpected BinaryPrimitive type 0x{other:02x}");
            w.varint_always(7, value.data as u64);
        }
    }
}

fn decode_primitive(data: &[u8]) -> Result<ResValue> {
    use res_value_type::*;
    let mut value = ResValue::new(TYPE_NULL, crate::res::value::DATA_NULL_UNDEFINED);
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        value = match field.number {
            1 => ResValue::new(TYPE_NULL, crate::res::value::DATA_NULL_UNDEFINED),
            2 => ResValue::new(TYPE_NULL, crate::res::value::DATA_NULL_EMPTY),
            3 => ResValue::new(TYPE_FLOAT, field.as_f32().to_bits()),
            13 => ResValue::new(TYPE_DIMENSION, field.value as u32),
            14 => ResValue::new(TYPE_FRACTION, field.value as u32),
            6 => ResValue::new(TYPE_INT_DEC, field.as_i32() as u32),
            7 => ResValue::new(TYPE_INT_HEX, field.value as u32),
            8 => ResValue::new(TYPE_INT_BOOLEAN, if field.as_bool() { 1 } else { 0 }),
            9 => ResValue::new(TYPE_INT_COLOR_ARGB8, field.value as u32),
            10 => ResValue::new(TYPE_INT_COLOR_RGB8, field.value as u32),
            11 => ResValue::new(TYPE_INT_COLOR_ARGB4, field.value as u32),
            12 => ResValue::new(TYPE_INT_COLOR_RGB4, field.value as u32),
            // Deprecated float dimension/fraction encodings.
            4 => ResValue::new(TYPE_DIMENSION, field.as_f32().to_bits()),
            5 => ResValue::new(TYPE_FRACTION, field.as_f32().to_bits()),
            _ => value,
        };
    }
    Ok(value)
}

/// Decodes an `aapt.pb.Item` message body. Returns `None` when no
/// `oneof` member was present.
pub fn decode_item(data: &[u8]) -> Result<Option<Item>> {
    Ok(decode_item_with_flags(data)?.map(|iv| iv.item))
}

/// Decodes an `aapt.pb.Item` body including its flag metadata.
pub fn decode_item_with_flags(data: &[u8]) -> Result<Option<ItemValue>> {
    let mut item: Option<Item> = None;
    let mut meta = ValueMeta::new();
    let mut flag_name = String::new();
    let mut flag_negated = false;
    let mut has_flag = false;

    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => item = Some(Item::Reference(decode_reference(field.data)?)),
            (2, WIRE_LEN) => {
                let mut value = String::new();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if f.number == 1 {
                        value = f.as_string();
                    }
                }
                item = Some(Item::String { value, untranslatable_sections: vec![] });
            }
            (3, WIRE_LEN) => {
                let mut value = String::new();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if f.number == 1 {
                        value = f.as_string();
                    }
                }
                item = Some(Item::RawString(value));
            }
            (4, WIRE_LEN) => {
                let mut value = String::new();
                let mut spans = Vec::new();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, WIRE_LEN) => value = f.as_string(),
                        (2, WIRE_LEN) => {
                            let mut span = Span { name: String::new(), first_char: 0, last_char: 0 };
                            let mut span_reader = Reader::new(f.data);
                            while let Some(sf) = span_reader.next_field() {
                                match sf.number {
                                    1 => span.name = sf.as_string(),
                                    2 => span.first_char = sf.value as u32,
                                    3 => span.last_char = sf.value as u32,
                                    _ => {}
                                }
                            }
                            spans.push(span);
                        }
                        _ => {}
                    }
                }
                item = Some(Item::StyledString { value, spans, untranslatable_sections: vec![] });
            }
            (5, WIRE_LEN) => {
                let mut file = FileReference::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match f.number {
                        1 => file.path = f.as_string(),
                        2 => {
                            file.file_type = match f.value {
                                1 => FileType::Png,
                                2 => FileType::BinaryXml,
                                3 => FileType::ProtoXml,
                                _ => FileType::Unknown,
                            }
                        }
                        _ => {}
                    }
                }
                item = Some(Item::FileReference(file));
            }
            (6, WIRE_LEN) => item = Some(Item::Id),
            (7, WIRE_LEN) => item = Some(Item::BinaryPrimitive(decode_primitive(field.data)?)),
            (8, _) => meta.flag_status = FlagStatus::from_u32(field.value as u32),
            (9, _) => {
                flag_negated = field.as_bool();
                has_flag = true;
            }
            (10, WIRE_LEN) => {
                flag_name = field.as_string();
                has_flag = true;
            }
            _ => {}
        }
    }
    if has_flag {
        meta.flag = Some(FeatureFlagAttribute { name: flag_name, negated: flag_negated });
    }
    Ok(item.map(|item| ItemValue { item, meta }))
}

// ───────────────────────── values ─────────────────────────

fn encode_value(w: &mut Writer, value: &Value, pool: Option<&mut SourcePathPool>) {
    // pb::Value: source=1, comment=2, weak=3, item=4, compound_value=5.
    if let Some(pool) = pool {
        encode_source(w, 1, &value.meta.source, pool);
    }
    w.string(2, &value.meta.comment);
    w.bool(3, value.meta.weak);

    match &value.kind {
        ValueKind::Item(item) => {
            w.message(4, |iw| encode_item_with_flags(iw, item, Some(&value.meta)));
        }
        compound => {
            w.message(5, |cw| {
                match compound {
                    ValueKind::Attribute(attr) => cw.message(1, |aw| {
                        aw.varint(1, attr.type_mask as u64);
                        aw.int32(2, attr.min_int);
                        aw.int32(3, attr.max_int);
                        for symbol in &attr.symbols {
                            aw.message(4, |sw| {
                                // Symbol: source=1, comment=2, name=3, value=4, type=5
                                sw.string(2, &symbol.comment);
                                sw.message(3, |rw| encode_reference(rw, &symbol.symbol));
                                sw.varint(4, symbol.value as u64);
                                sw.varint(5, symbol.data_type as u64);
                            });
                        }
                    }),
                    ValueKind::Style(style) => cw.message(2, |stw| {
                        if let Some(parent) = &style.parent {
                            stw.message(1, |rw| encode_reference(rw, parent));
                        }
                        for entry in &style.entries {
                            stw.message(3, |ew| {
                                // Entry: source=1, comment=2, key=3, item=4
                                ew.string(2, &entry.comment);
                                ew.message(3, |rw| encode_reference(rw, &entry.key));
                                ew.message(4, |iw| {
                                    encode_item_with_flags(iw, &entry.value.item, Some(&entry.value.meta))
                                });
                            });
                        }
                    }),
                    ValueKind::Styleable(styleable) => cw.message(3, |sw| {
                        for entry in &styleable.entries {
                            sw.message(1, |ew| {
                                ew.message(3, |rw| encode_reference(rw, entry));
                            });
                        }
                    }),
                    ValueKind::Array(array) => cw.message(4, |aw| {
                        for element in &array.elements {
                            aw.message(1, |ew| {
                                ew.string(2, &element.meta.comment);
                                ew.message(3, |iw| {
                                    encode_item_with_flags(iw, &element.item, Some(&element.meta))
                                });
                            });
                        }
                    }),
                    ValueKind::Plural(plural) => cw.message(5, |pw| {
                        for (index, slot) in plural.values.iter().enumerate() {
                            let Some(item_value) = slot else { continue };
                            pw.message(1, |ew| {
                                // Entry: source=1, comment=2, arity=3, item=4
                                ew.string(2, &item_value.meta.comment);
                                ew.varint(3, index as u64);
                                ew.message(4, |iw| {
                                    encode_item_with_flags(iw, &item_value.item, Some(&item_value.meta))
                                });
                            });
                        }
                    }),
                    ValueKind::Macro(macro_value) => cw.message(6, |mw| {
                        mw.string(1, &macro_value.raw_value);
                        mw.message(2, |sw| {
                            sw.string(1, &macro_value.style_string.str);
                            for span in &macro_value.style_string.spans {
                                sw.message(2, |spw| {
                                    spw.string(1, &span.name);
                                    spw.varint(2, span.first_char as u64);
                                    spw.varint(3, span.last_char as u64);
                                });
                            }
                        });
                        for section in &macro_value.untranslatable_sections {
                            mw.message(3, |uw| {
                                uw.varint(1, section.start as u64);
                                uw.varint(2, section.end as u64);
                            });
                        }
                        for ns in &macro_value.alias_namespaces {
                            mw.message(4, |nw| {
                                nw.string(1, &ns.alias);
                                nw.string(2, &ns.package_name);
                                nw.bool(3, ns.is_private);
                            });
                        }
                    }),
                    ValueKind::Item(_) => unreachable!(),
                }
                // CompoundValue flag fields: 7=status, 8=negated, 9=name.
                cw.varint(7, value.meta.flag_status as u64);
                if let Some(flag) = &value.meta.flag {
                    cw.bool(8, flag.negated);
                    cw.string(9, &flag.name);
                }
            });
        }
    }
}

fn decode_value(data: &[u8], pool: Option<&BinaryStringPool>) -> Result<Value> {
    let mut meta = ValueMeta::new();
    let mut kind: Option<ValueKind> = None;

    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => meta.source = decode_source(field.data, pool),
            (2, WIRE_LEN) => meta.comment = field.as_string(),
            (3, _) => meta.weak = field.as_bool(),
            (4, WIRE_LEN) => {
                let item_value = decode_item_with_flags(field.data)?
                    .ok_or_else(|| anyhow!("pb.Value.item has no value set"))?;
                meta.flag = item_value.meta.flag.clone();
                meta.flag_status = item_value.meta.flag_status;
                kind = Some(ValueKind::Item(item_value.item));
            }
            (5, WIRE_LEN) => {
                kind = Some(decode_compound_value(field.data, pool, &mut meta)?);
            }
            _ => {}
        }
    }
    Ok(Value { kind: kind.ok_or_else(|| anyhow!("pb.Value has no value set"))?, meta })
}

fn decode_compound_value(
    data: &[u8],
    pool: Option<&BinaryStringPool>,
    meta: &mut ValueMeta,
) -> Result<ValueKind> {
    let mut kind: Option<ValueKind> = None;
    let mut flag_name = String::new();
    let mut flag_negated = false;
    let mut has_flag = false;

    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => {
                let mut attr = Attribute::new(0);
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, _) => attr.type_mask = f.value as u32,
                        (2, _) => attr.min_int = f.as_i32(),
                        (3, _) => attr.max_int = f.as_i32(),
                        (4, WIRE_LEN) => {
                            let mut symbol = AttributeSymbol {
                                symbol: Reference::default(),
                                source: Source::default(),
                                comment: String::new(),
                                value: 0,
                                data_type: res_value_type::TYPE_INT_DEC,
                            };
                            let mut symbol_reader = Reader::new(f.data);
                            while let Some(sf) = symbol_reader.next_field() {
                                match (sf.number, sf.wire_type) {
                                    (1, WIRE_LEN) => symbol.source = decode_source(sf.data, pool),
                                    (2, WIRE_LEN) => symbol.comment = sf.as_string(),
                                    (3, WIRE_LEN) => symbol.symbol = decode_reference(sf.data)?,
                                    (4, _) => symbol.value = sf.value as u32,
                                    (5, _) => symbol.data_type = sf.value as u8,
                                    _ => {}
                                }
                            }
                            attr.symbols.push(symbol);
                        }
                        _ => {}
                    }
                }
                if attr.min_int == 0 && attr.max_int == 0 {
                    // proto3 cannot distinguish "absent" from 0; aapt2's
                    // deserializer restores full range when both are 0.
                    attr.min_int = i32::MIN;
                    attr.max_int = i32::MAX;
                }
                kind = Some(ValueKind::Attribute(attr));
            }
            (2, WIRE_LEN) => {
                let mut style = Style::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, WIRE_LEN) => style.parent = Some(decode_reference(f.data)?),
                        (2, WIRE_LEN) => {
                            style.parent_source = decode_source(f.data, pool);
                        }
                        (3, WIRE_LEN) => {
                            let mut entry = StyleEntry {
                                key: Reference::default(),
                                value: ItemValue::new(Item::Id),
                                source: Source::default(),
                                comment: String::new(),
                            };
                            let mut entry_reader = Reader::new(f.data);
                            let mut item = None;
                            while let Some(ef) = entry_reader.next_field() {
                                match (ef.number, ef.wire_type) {
                                    (1, WIRE_LEN) => entry.source = decode_source(ef.data, pool),
                                    (2, WIRE_LEN) => entry.comment = ef.as_string(),
                                    (3, WIRE_LEN) => entry.key = decode_reference(ef.data)?,
                                    (4, WIRE_LEN) => item = decode_item_with_flags(ef.data)?,
                                    _ => {}
                                }
                            }
                            entry.value = item
                                .ok_or_else(|| anyhow!("pb.Style.Entry has no item"))?;
                            style.entries.push(entry);
                        }
                        _ => {}
                    }
                }
                kind = Some(ValueKind::Style(style));
            }
            (3, WIRE_LEN) => {
                let mut styleable = Styleable::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if let (1, WIRE_LEN) = (f.number, f.wire_type) {
                        let mut entry_reader = Reader::new(f.data);
                        while let Some(ef) = entry_reader.next_field() {
                            if let (3, WIRE_LEN) = (ef.number, ef.wire_type) {
                                styleable.entries.push(decode_reference(ef.data)?);
                            }
                        }
                    }
                }
                kind = Some(ValueKind::Styleable(styleable));
            }
            (4, WIRE_LEN) => {
                let mut array = Array::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if let (1, WIRE_LEN) = (f.number, f.wire_type) {
                        let mut element_reader = Reader::new(f.data);
                        let mut item = None;
                        let mut comment = String::new();
                        let mut source = Source::default();
                        while let Some(ef) = element_reader.next_field() {
                            match (ef.number, ef.wire_type) {
                                (1, WIRE_LEN) => source = decode_source(ef.data, pool),
                                (2, WIRE_LEN) => comment = ef.as_string(),
                                (3, WIRE_LEN) => item = decode_item_with_flags(ef.data)?,
                                _ => {}
                            }
                        }
                        let mut item =
                            item.ok_or_else(|| anyhow!("pb.Array.Element has no item"))?;
                        item.meta.comment = comment;
                        item.meta.source = source;
                        array.elements.push(item);
                    }
                }
                kind = Some(ValueKind::Array(array));
            }
            (5, WIRE_LEN) => {
                let mut plural = Plural::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if let (1, WIRE_LEN) = (f.number, f.wire_type) {
                        let mut entry_reader = Reader::new(f.data);
                        let mut arity = crate::res::value::PLURAL_OTHER;
                        let mut item = None;
                        let mut comment = String::new();
                        let mut source = Source::default();
                        while let Some(ef) = entry_reader.next_field() {
                            match (ef.number, ef.wire_type) {
                                (1, WIRE_LEN) => source = decode_source(ef.data, pool),
                                (2, WIRE_LEN) => comment = ef.as_string(),
                                (3, _) => arity = (ef.value as usize).min(crate::res::value::PLURAL_OTHER),
                                (4, WIRE_LEN) => item = decode_item_with_flags(ef.data)?,
                                _ => {}
                            }
                        }
                        let mut item =
                            item.ok_or_else(|| anyhow!("pb.Plural.Entry has no item"))?;
                        item.meta.comment = comment;
                        item.meta.source = source;
                        plural.values[arity] = Some(item);
                    }
                }
                kind = Some(ValueKind::Plural(plural));
            }
            (6, WIRE_LEN) => {
                let mut macro_value = Macro::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, WIRE_LEN) => macro_value.raw_value = f.as_string(),
                        (2, WIRE_LEN) => {
                            let mut style_string = StyleString::default();
                            let mut ss_reader = Reader::new(f.data);
                            while let Some(sf) = ss_reader.next_field() {
                                match (sf.number, sf.wire_type) {
                                    (1, WIRE_LEN) => style_string.str = sf.as_string(),
                                    (2, WIRE_LEN) => {
                                        let mut span = StyleStringSpan::default();
                                        let mut span_reader = Reader::new(sf.data);
                                        while let Some(spf) = span_reader.next_field() {
                                            match spf.number {
                                                1 => span.name = spf.as_string(),
                                                2 => span.first_char = spf.value as u32,
                                                3 => span.last_char = spf.value as u32,
                                                _ => {}
                                            }
                                        }
                                        style_string.spans.push(span);
                                    }
                                    _ => {}
                                }
                            }
                            macro_value.style_string = style_string;
                        }
                        (3, WIRE_LEN) => {
                            let mut section = UntranslatableSection { start: 0, end: 0 };
                            let mut section_reader = Reader::new(f.data);
                            while let Some(sf) = section_reader.next_field() {
                                match sf.number {
                                    1 => section.start = sf.value as usize,
                                    2 => section.end = sf.value as usize,
                                    _ => {}
                                }
                            }
                            macro_value.untranslatable_sections.push(section);
                        }
                        (4, WIRE_LEN) => {
                            let mut ns = MacroNamespace::default();
                            let mut ns_reader = Reader::new(f.data);
                            while let Some(nf) = ns_reader.next_field() {
                                match (nf.number, nf.wire_type) {
                                    (1, WIRE_LEN) => ns.alias = nf.as_string(),
                                    (2, WIRE_LEN) => ns.package_name = nf.as_string(),
                                    (3, _) => ns.is_private = nf.as_bool(),
                                    _ => {}
                                }
                            }
                            macro_value.alias_namespaces.push(ns);
                        }
                        _ => {}
                    }
                }
                kind = Some(ValueKind::Macro(macro_value));
            }
            (7, _) => meta.flag_status = FlagStatus::from_u32(field.value as u32),
            (8, _) => {
                flag_negated = field.as_bool();
                has_flag = true;
            }
            (9, WIRE_LEN) => {
                flag_name = field.as_string();
                has_flag = true;
            }
            _ => {}
        }
    }
    if has_flag {
        meta.flag = Some(FeatureFlagAttribute { name: flag_name, negated: flag_negated });
    }
    kind.ok_or_else(|| anyhow!("pb.CompoundValue has no value set"))
}

// ───────────────────────── resource table ─────────────────────────

/// Options controlling table serialization.
#[derive(Debug, Clone, Copy, Default)]
pub struct SerializeTableOptions {
    /// Skip the source pool and source references (`--exclude-sources`).
    pub exclude_sources: bool,
}

/// Encodes a [`ResourceTable`] as a serialized `aapt.pb.ResourceTable`.
/// Port of `SerializeTableToPb`.
pub fn encode_table(table: &ResourceTable, options: &SerializeTableOptions) -> Vec<u8> {
    let mut pool = if options.exclude_sources { None } else { Some(SourcePathPool::default()) };

    // Encode the package list into a scratch buffer first: it fills the
    // source pool, which must be written as field 1.
    let mut packages_writer = Writer::new();
    let mut overlayables_written: Vec<usize> = Vec::new();
    let mut overlayables_writer = Writer::new();

    for (package, types) in table.sorted_view() {
        packages_writer.message(2, |pw| {
            // Package id is derived from the first assigned entry ID.
            let package_id = types
                .iter()
                .flat_map(|(_, entries)| entries.iter())
                .find_map(|e| e.id.map(|id| id.package_id()));
            if let Some(id) = package_id {
                pw.message(1, |iw| iw.varint(1, id as u64));
            }
            pw.string(2, &package.name);

            for (ty, entries) in &types {
                pw.message(3, |tw| {
                    let type_id = entries.iter().find_map(|e| e.id.map(|id| id.type_id()));
                    if let Some(id) = type_id {
                        tw.message(1, |iw| iw.varint(1, id as u64));
                    }
                    tw.string(2, &ty.named_type.name);

                    for entry in entries {
                        tw.message(3, |ew| {
                            if let Some(id) = entry.id {
                                ew.message(1, |iw| iw.varint(1, id.entry_id() as u64));
                            }
                            ew.string(2, &entry.name);

                            // Visibility is always present.
                            ew.message(3, |vw| {
                                vw.varint(
                                    1,
                                    match entry.visibility.level {
                                        VisibilityLevel::Undefined => 0,
                                        VisibilityLevel::Private => 1,
                                        VisibilityLevel::Public => 2,
                                    },
                                );
                                if let Some(pool) = pool.as_mut() {
                                    encode_source(vw, 2, &entry.visibility.source, pool);
                                }
                                vw.string(3, &entry.visibility.comment);
                                vw.bool(4, entry.visibility.staged_api);
                            });

                            if let Some(allow_new) = &entry.allow_new {
                                ew.message(4, |aw| {
                                    if let Some(pool) = pool.as_mut() {
                                        encode_source(aw, 1, &allow_new.source, pool);
                                    }
                                    aw.string(2, &allow_new.comment);
                                });
                            }

                            if let Some(item) = &entry.overlayable_item {
                                // Register the overlayable in the
                                // table-level list on first use.
                                let table_index = item.overlayable_index;
                                let serialized_index = match overlayables_written
                                    .iter()
                                    .position(|&i| i == table_index)
                                {
                                    Some(i) => i,
                                    None => {
                                        overlayables_written.push(table_index);
                                        if let Some(overlayable) =
                                            table.overlayables.get(table_index)
                                        {
                                            overlayables_writer.message(3, |ow| {
                                                ow.string(1, &overlayable.name);
                                                if let Some(pool) = pool.as_mut() {
                                                    encode_source(ow, 2, &overlayable.source, pool);
                                                }
                                                ow.string(3, &overlayable.actor);
                                            });
                                        }
                                        overlayables_written.len() - 1
                                    }
                                };
                                ew.message(5, |ow| {
                                    if let Some(pool) = pool.as_mut() {
                                        encode_source(ow, 1, &item.source, pool);
                                    }
                                    ow.string(2, &item.comment);
                                    // Policies: repeated enum, packed.
                                    let mut policy_writer = Writer::new();
                                    use crate::res::table::policy as p;
                                    for (bit, value) in [
                                        (p::PUBLIC, 1u64),
                                        (p::PRODUCT_PARTITION, 4),
                                        (p::SYSTEM_PARTITION, 2),
                                        (p::VENDOR_PARTITION, 3),
                                        (p::SIGNATURE, 5),
                                        (p::ODM_PARTITION, 6),
                                        (p::OEM_PARTITION, 7),
                                        (p::ACTOR_SIGNATURE, 8),
                                        (p::CONFIG_SIGNATURE, 9),
                                    ] {
                                        if item.policies & bit != 0 {
                                            policy_writer.write_raw_varint(value);
                                        }
                                    }
                                    if !policy_writer.buf.is_empty() {
                                        ow.bytes_always(3, &policy_writer.buf);
                                    }
                                    ow.varint(4, serialized_index as u64);
                                });
                            }

                            for config_value in &entry.values {
                                let Some(value) = &config_value.value else { continue };
                                ew.message(6, |cw| {
                                    cw.message(1, |conf| {
                                        encode_config(conf, &config_value.config, &config_value.product)
                                    });
                                    cw.message(2, |vw| encode_value(vw, value, pool.as_mut()));
                                });
                            }

                            if let Some(staged_id) = &entry.staged_id {
                                ew.message(7, |sw| {
                                    sw.varint(2, staged_id.id.0 as u64);
                                });
                            }

                            for config_value in &entry.flag_disabled_values {
                                let Some(value) = &config_value.value else { continue };
                                ew.message(8, |cw| {
                                    cw.message(1, |conf| {
                                        encode_config(conf, &config_value.config, &config_value.product)
                                    });
                                    cw.message(2, |vw| encode_value(vw, value, pool.as_mut()));
                                });
                            }
                        });
                    }
                });
            }
        });
    }

    // Assemble in field order: source_pool(1), package(2), overlayable(3),
    // tool_fingerprint(4), dynamic_ref_table(5).
    let mut writer = Writer::new();
    if let Some(pool) = &pool {
        let pool_data = pool.flatten();
        writer.message(1, |sw| sw.bytes(1, &pool_data));
    }
    writer.buf.extend_from_slice(&packages_writer.buf);
    writer.buf.extend_from_slice(&overlayables_writer.buf);
    let (tool, version) = tool_fingerprint();
    writer.message(4, |fw| {
        fw.string(1, &tool);
        fw.string(2, &version);
    });
    for (id, name) in &table.included_packages {
        writer.message(5, |dw| {
            dw.message(1, |iw| iw.varint(1, *id as u64));
            dw.string(2, name);
        });
    }
    writer.into_bytes()
}

/// Decodes a serialized `aapt.pb.ResourceTable` into a [`ResourceTable`].
/// Port of `DeserializeTableFromPb`.
pub fn decode_table(data: &[u8]) -> Result<ResourceTable> {
    let mut table = ResourceTable::new_unvalidated();
    let mut source_pool: Option<BinaryStringPool> = None;

    // First pass: pick up the source pool and overlayables so values can
    // reference them.
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => {
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if f.number == 1 && !f.data.is_empty() {
                        source_pool = BinaryStringPool::parse(f.data);
                    }
                }
            }
            (3, WIRE_LEN) => {
                let mut overlayable = Overlayable::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, WIRE_LEN) => overlayable.name = f.as_string(),
                        (3, WIRE_LEN) => overlayable.actor = f.as_string(),
                        _ => {}
                    }
                }
                table.overlayables.push(overlayable);
            }
            (5, WIRE_LEN) => {
                let mut id = 0u8;
                let mut name = String::new();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, WIRE_LEN) => {
                            let mut id_reader = Reader::new(f.data);
                            while let Some(idf) = id_reader.next_field() {
                                if idf.number == 1 {
                                    id = idf.value as u8;
                                }
                            }
                        }
                        (2, WIRE_LEN) => name = f.as_string(),
                        _ => {}
                    }
                }
                table.included_packages.push((id, name));
            }
            _ => {}
        }
    }

    // Second pass: packages.
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        if let (2, WIRE_LEN) = (field.number, field.wire_type) {
            decode_package(field.data, &mut table, source_pool.as_ref())
                .context("deserializing package")?;
        }
    }
    Ok(table)
}

fn decode_package(
    data: &[u8],
    table: &mut ResourceTable,
    pool: Option<&BinaryStringPool>,
) -> Result<()> {
    let mut package_id: Option<u8> = None;
    let mut package_name = String::new();

    // Collect the type payloads first so the package name is known.
    let mut type_payloads: Vec<&[u8]> = Vec::new();
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => {
                // Message presence implies an ID; the value field is
                // omitted when zero (proto3).
                let mut id = 0u8;
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if f.number == 1 {
                        id = f.value as u8;
                    }
                }
                package_id = Some(id);
            }
            (2, WIRE_LEN) => package_name = field.as_string(),
            (3, WIRE_LEN) => type_payloads.push(field.data),
            _ => {}
        }
    }

    for type_data in type_payloads {
        let mut type_id: Option<u8> = None;
        let mut type_name = String::new();
        let mut entry_payloads: Vec<&[u8]> = Vec::new();
        let mut reader = Reader::new(type_data);
        while let Some(field) = reader.next_field() {
            match (field.number, field.wire_type) {
                (1, WIRE_LEN) => {
                    let mut id = 0u8;
                    let mut sub = Reader::new(field.data);
                    while let Some(f) = sub.next_field() {
                        if f.number == 1 {
                            id = f.value as u8;
                        }
                    }
                    type_id = Some(id);
                }
                (2, WIRE_LEN) => type_name = field.as_string(),
                (3, WIRE_LEN) => entry_payloads.push(field.data),
                _ => {}
            }
        }

        let named_type = crate::res::ResourceNamedType::parse(&type_name)
            .ok_or_else(|| anyhow!("unknown type '{type_name}'"))?;

        for entry_data in entry_payloads {
            decode_entry(
                entry_data,
                table,
                pool,
                &package_name,
                package_id,
                &named_type,
                type_id,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_entry(
    data: &[u8],
    table: &mut ResourceTable,
    pool: Option<&BinaryStringPool>,
    package_name: &str,
    package_id: Option<u8>,
    named_type: &crate::res::ResourceNamedType,
    type_id: Option<u8>,
) -> Result<()> {
    let mut entry_id: Option<u16> = None;
    let mut entry_name = String::new();
    let mut visibility: Option<Visibility> = None;
    let mut allow_new: Option<AllowNew> = None;
    let mut overlayable_item: Option<OverlayableItem> = None;
    let mut staged_id: Option<StagedId> = None;
    let mut config_values: Vec<(&[u8], bool)> = Vec::new();

    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => {
                let mut id = 0u16;
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if f.number == 1 {
                        id = f.value as u16;
                    }
                }
                entry_id = Some(id);
            }
            (2, WIRE_LEN) => entry_name = field.as_string(),
            (3, WIRE_LEN) => {
                let mut vis = Visibility::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, _) => {
                            vis.level = match f.value {
                                1 => VisibilityLevel::Private,
                                2 => VisibilityLevel::Public,
                                _ => VisibilityLevel::Undefined,
                            }
                        }
                        (2, WIRE_LEN) => vis.source = decode_source(f.data, pool),
                        (3, WIRE_LEN) => vis.comment = f.as_string(),
                        (4, _) => vis.staged_api = f.as_bool(),
                        _ => {}
                    }
                }
                visibility = Some(vis);
            }
            (4, WIRE_LEN) => {
                let mut value = AllowNew::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, WIRE_LEN) => value.source = decode_source(f.data, pool),
                        (2, WIRE_LEN) => value.comment = f.as_string(),
                        _ => {}
                    }
                }
                allow_new = Some(value);
            }
            (5, WIRE_LEN) => {
                let mut item = OverlayableItem::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, WIRE_LEN) => item.source = decode_source(f.data, pool),
                        (2, WIRE_LEN) => item.comment = f.as_string(),
                        (3, WIRE_LEN) => {
                            // Packed repeated enum: raw varints.
                            let mut raw = f.data;
                            while !raw.is_empty() {
                                let mut value = 0u64;
                                let mut shift = 0;
                                let mut used = 0;
                                for (i, &b) in raw.iter().enumerate() {
                                    value |= ((b & 0x7f) as u64) << shift;
                                    shift += 7;
                                    if b & 0x80 == 0 {
                                        used = i + 1;
                                        break;
                                    }
                                }
                                if used == 0 {
                                    break;
                                }
                                raw = &raw[used..];
                                item.policies |= policy_from_pb(value as u32);
                            }
                        }
                        (3, _) => {
                            // Unpacked policy entry.
                            item.policies |= policy_from_pb(f.value as u32);
                        }
                        (4, _) => item.overlayable_index = f.value as usize,
                        _ => {}
                    }
                }
                overlayable_item = Some(item);
            }
            (6, WIRE_LEN) => config_values.push((field.data, false)),
            (7, WIRE_LEN) => {
                let mut value = StagedId::default();
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    if f.number == 2 {
                        value.id = ResourceId(f.value as u32);
                    }
                }
                staged_id = Some(value);
            }
            (8, WIRE_LEN) => config_values.push((field.data, true)),
            _ => {}
        }
    }

    let name = ResourceName::with_named_type(package_name, named_type.clone(), entry_name);
    let id = match (package_id, type_id, entry_id) {
        (Some(p), Some(t), Some(e)) => Some(ResourceId::new(p, t, e)),
        _ => None,
    };

    // An entry might carry no values (visibility-only); add it anyway.
    let mut added_anything = false;
    for (config_value_data, flag_disabled) in &config_values {
        let mut config = ConfigDescription::default();
        let mut product = String::new();
        let mut value: Option<Value> = None;
        let mut sub = Reader::new(config_value_data);
        while let Some(f) = sub.next_field() {
            match (f.number, f.wire_type) {
                (1, WIRE_LEN) => {
                    let (parsed, parsed_product) = decode_config(f.data)?;
                    config = parsed;
                    product = parsed_product;
                }
                (2, WIRE_LEN) => value = Some(decode_value(f.data, pool)?),
                _ => {}
            }
        }
        let Some(mut value) = value else { continue };
        if *flag_disabled {
            value.meta.flag_status = FlagStatus::Disabled;
        }
        let mut new_resource = NewResource::with_name(name.clone())
            .config(config)
            .product(product)
            .value(value)
            .allow_mangled(true);
        if let Some(id) = id {
            new_resource = new_resource.id_with_conflict(id, OnIdConflict::CreateEntry);
        }
        table
            .add_resource_overlay(new_resource)
            .map_err(|e| anyhow!("{e}"))?;
        added_anything = true;
    }

    let mut shell = NewResource::with_name(name).allow_mangled(true);
    if let Some(id) = id {
        shell = shell.id_with_conflict(id, OnIdConflict::CreateEntry);
    }
    if let Some(visibility) = visibility {
        shell = shell.visibility(visibility);
    }
    if let Some(allow_new) = allow_new {
        shell = shell.allow_new(allow_new);
    }
    if let Some(item) = overlayable_item {
        shell = shell.overlayable(item);
    }
    if let Some(staged) = staged_id {
        shell = shell.staged_id(staged);
    }
    if !added_anything || shell.visibility.is_some() || shell.overlayable.is_some() {
        table.add_resource_overlay(shell).map_err(|e| anyhow!("{e}"))?;
    }
    Ok(())
}

fn policy_from_pb(value: u32) -> u32 {
    use crate::res::table::policy as p;
    match value {
        1 => p::PUBLIC,
        2 => p::SYSTEM_PARTITION,
        3 => p::VENDOR_PARTITION,
        4 => p::PRODUCT_PARTITION,
        5 => p::SIGNATURE,
        6 => p::ODM_PARTITION,
        7 => p::OEM_PARTITION,
        8 => p::ACTOR_SIGNATURE,
        9 => p::CONFIG_SIGNATURE,
        _ => p::NONE,
    }
}

// ───────────────────────── compiled file ─────────────────────────

/// Encodes a `aapt.pb.internal.CompiledFile` message. Port of
/// `SerializeCompiledFileToPb`.
pub fn encode_compiled_file(file: &ResourceFile) -> Vec<u8> {
    let mut w = Writer::new();
    w.string(1, &file.name.to_string());
    w.message(2, |cw| encode_config(cw, &file.config, ""));
    w.varint(
        3,
        match file.file_type {
            FileType::Unknown => 0,
            FileType::Png => 1,
            FileType::BinaryXml => 2,
            FileType::ProtoXml => 3,
        },
    );
    w.string(4, &file.source.path);
    for symbol in &file.exported_symbols {
        w.message(5, |sw| {
            sw.string(1, &symbol.name.to_string());
            sw.message(2, |pw| pw.varint(1, symbol.line as u64));
        });
    }
    w.varint(6, file.flag_status as u64);
    if let Some(flag) = &file.flag {
        w.bool(7, flag.negated);
        w.string(8, &flag.name);
    }
    w.into_bytes()
}

/// Decodes a `aapt.pb.internal.CompiledFile` message. Port of
/// `DeserializeCompiledFileFromPb`.
pub fn decode_compiled_file(data: &[u8]) -> Result<ResourceFile> {
    let mut file = ResourceFile::default();
    let mut flag_name = String::new();
    let mut flag_negated = false;
    let mut has_flag = false;

    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => {
                let raw = field.as_str();
                let (name, _) = parse_resource_name(raw)
                    .ok_or_else(|| anyhow!("invalid resource name in compiled file: '{raw}'"))?;
                file.name = name;
            }
            (2, WIRE_LEN) => {
                let (config, _) = decode_config(field.data)?;
                file.config = config;
            }
            (3, _) => {
                file.file_type = match field.value {
                    1 => FileType::Png,
                    2 => FileType::BinaryXml,
                    3 => FileType::ProtoXml,
                    _ => FileType::Unknown,
                }
            }
            (4, WIRE_LEN) => file.source = Source::new(field.as_string()),
            (5, WIRE_LEN) => {
                let mut name = None;
                let mut line = 0usize;
                let mut sub = Reader::new(field.data);
                while let Some(f) = sub.next_field() {
                    match (f.number, f.wire_type) {
                        (1, WIRE_LEN) => {
                            name = parse_resource_name(f.as_str()).map(|(n, _)| n);
                        }
                        (2, WIRE_LEN) => {
                            let mut pos = Reader::new(f.data);
                            while let Some(pf) = pos.next_field() {
                                if pf.number == 1 {
                                    line = pf.value as usize;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(name) = name {
                    file.exported_symbols.push(SourcedResourceName { name, line });
                }
            }
            (6, _) => file.flag_status = FlagStatus::from_u32(field.value as u32),
            (7, _) => {
                flag_negated = field.as_bool();
                has_flag = true;
            }
            (8, WIRE_LEN) => {
                flag_name = field.as_string();
                has_flag = true;
            }
            _ => {}
        }
    }
    if has_flag {
        file.flag = Some(FeatureFlagAttribute { name: flag_name, negated: flag_negated });
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::res::ResourceType;

    fn make_table() -> ResourceTable {
        let mut table = ResourceTable::new();
        let mut value = Value::item(Item::String {
            value: "Hello".to_string(),
            untranslatable_sections: vec![],
        });
        value.meta.source = Source::with_line("res/values/strings.xml", 3);
        table
            .add_value(
                ResourceName::new("com.app", ResourceType::String, "hello"),
                ConfigDescription::default(),
                value,
            )
            .unwrap();

        let mut style = Style::default();
        style.parent = Some(Reference::from_name(ResourceName::new(
            "android",
            ResourceType::Style,
            "Theme",
        )));
        style.entries.push(StyleEntry {
            key: Reference::from_name(ResourceName::new("android", ResourceType::Attr, "background")),
            value: ItemValue::new(Item::BinaryPrimitive(ResValue::new(
                res_value_type::TYPE_INT_COLOR_ARGB8,
                0xff00ff00,
            ))),
            source: Source::default(),
            comment: String::new(),
        });
        table
            .add_value(
                ResourceName::new("com.app", ResourceType::Style, "AppTheme"),
                ConfigDescription::default(),
                Value::new(ValueKind::Style(style)),
            )
            .unwrap();
        table
    }

    #[test]
    fn table_round_trip() {
        let table = make_table();
        let encoded = encode_table(&table, &SerializeTableOptions::default());
        let decoded = decode_table(&encoded).unwrap();

        let name = ResourceName::new("com.app", ResourceType::String, "hello");
        let result = decoded.find_resource(&name).unwrap();
        let value = result.entry.values[0].value.as_ref().unwrap();
        match &value.kind {
            ValueKind::Item(Item::String { value, .. }) => assert_eq!(value, "Hello"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(value.meta.source.path, "res/values/strings.xml");
        assert_eq!(value.meta.source.line, Some(3));

        let style_name = ResourceName::new("com.app", ResourceType::Style, "AppTheme");
        let result = decoded.find_resource(&style_name).unwrap();
        match &result.entry.values[0].value.as_ref().unwrap().kind {
            ValueKind::Style(style) => {
                assert_eq!(
                    style.parent.as_ref().unwrap().name.as_ref().unwrap().entry,
                    "Theme"
                );
                assert_eq!(style.entries.len(), 1);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ids_round_trip() {
        let mut table = make_table();
        table
            .find_resource_mut(&ResourceName::new("com.app", ResourceType::String, "hello"))
            .unwrap()
            .id = Some(crate::res::ResourceId(0x7f020000));
        let encoded = encode_table(&table, &SerializeTableOptions::default());
        let decoded = decode_table(&encoded).unwrap();
        let entry = decoded
            .find_resource(&ResourceName::new("com.app", ResourceType::String, "hello"))
            .unwrap()
            .entry;
        assert_eq!(entry.id, Some(crate::res::ResourceId(0x7f020000)));
    }

    #[test]
    fn compiled_file_round_trip() {
        let file = ResourceFile {
            name: ResourceName::new("com.app", ResourceType::Layout, "main"),
            config: ConfigDescription::default(),
            file_type: FileType::ProtoXml,
            source: Source::new("res/layout/main.xml"),
            exported_symbols: vec![SourcedResourceName {
                name: ResourceName::new("com.app", ResourceType::Id, "button"),
                line: 12,
            }],
            flag_status: FlagStatus::NoFlag,
            flag: None,
        };
        let encoded = encode_compiled_file(&file);
        let decoded = decode_compiled_file(&encoded).unwrap();
        assert_eq!(decoded.name, file.name);
        assert_eq!(decoded.file_type, FileType::ProtoXml);
        assert_eq!(decoded.source.path, "res/layout/main.xml");
        assert_eq!(decoded.exported_symbols.len(), 1);
        assert_eq!(decoded.exported_symbols[0].line, 12);
    }

    #[test]
    fn primitive_round_trip() {
        use res_value_type::*;
        for (data_type, data) in [
            (TYPE_NULL, crate::res::value::DATA_NULL_UNDEFINED),
            (TYPE_NULL, crate::res::value::DATA_NULL_EMPTY),
            (TYPE_FLOAT, 1.25f32.to_bits()),
            (TYPE_DIMENSION, 0x2001),
            (TYPE_FRACTION, 0x3001),
            (TYPE_INT_DEC, (-5i32) as u32),
            (TYPE_INT_HEX, 0xff00ff00),
            (TYPE_INT_BOOLEAN, 1),
            (TYPE_INT_COLOR_ARGB8, 0x11223344),
            (TYPE_INT_COLOR_RGB8, 0x00112233),
            (TYPE_INT_COLOR_ARGB4, 0x1234),
            (TYPE_INT_COLOR_RGB4, 0x123),
        ] {
            let mut w = Writer::new();
            encode_primitive(&mut w, &ResValue::new(data_type, data));
            let decoded = decode_primitive(&w.buf).unwrap();
            assert_eq!(decoded, ResValue::new(data_type, data), "type 0x{data_type:02x}");
        }
    }
}
