//! Minimal DEX reader: header + map parsing, used by the validator and the
//! structural normalizer, and (later) for `.dex` inputs and merging.

use anyhow::{bail, Result};

/// A parsed DEX header and map.
#[derive(Debug, Clone)]
pub struct DexHeader {
    pub file_size: u32,
    pub string_ids_size: u32,
    pub string_ids_off: u32,
    pub type_ids_size: u32,
    pub type_ids_off: u32,
    pub proto_ids_size: u32,
    pub proto_ids_off: u32,
    pub field_ids_size: u32,
    pub field_ids_off: u32,
    pub method_ids_size: u32,
    pub method_ids_off: u32,
    pub class_defs_size: u32,
    pub class_defs_off: u32,
    pub data_size: u32,
    pub data_off: u32,
    pub map_off: u32,
}

fn u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
fn u16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

pub fn parse_header(d: &[u8]) -> Result<DexHeader> {
    if d.len() < 0x70 || &d[0..4] != b"dex\n" {
        bail!("not a DEX file");
    }
    Ok(DexHeader {
        file_size: u32(d, 0x20),
        map_off: u32(d, 0x34),
        string_ids_size: u32(d, 0x38),
        string_ids_off: u32(d, 0x3c),
        type_ids_size: u32(d, 0x40),
        type_ids_off: u32(d, 0x44),
        proto_ids_size: u32(d, 0x48),
        proto_ids_off: u32(d, 0x4c),
        field_ids_size: u32(d, 0x50),
        field_ids_off: u32(d, 0x54),
        method_ids_size: u32(d, 0x58),
        method_ids_off: u32(d, 0x5c),
        class_defs_size: u32(d, 0x60),
        class_defs_off: u32(d, 0x64),
        data_size: u32(d, 0x68),
        data_off: u32(d, 0x6c),
    })
}

/// One map-list entry: `(type, size, offset)`.
pub fn parse_map(d: &[u8], map_off: u32) -> Vec<(u16, u32, u32)> {
    let base = map_off as usize;
    let n = u32(d, base) as usize;
    let mut out = Vec::with_capacity(n);
    let mut o = base + 4;
    for _ in 0..n {
        out.push((u16(d, o), u32(d, o + 4), u32(d, o + 8)));
        o += 12;
    }
    out
}
