//! Normalize a `.dex` file into a stable text form for golden diffing.
//!
//! ## What we keep
//!
//! - DEX version (`035`)
//! - Counts of each index table (strings, types, protos, fields,
//!   methods, classes)
//! - Sorted string list (one per line)
//! - Sorted type list (descriptor → string)
//! - Sorted method list (`class.name(params)return`)
//! - Per-class summary: descriptor, super, source file, method names
//!   with `(registers, ins, outs, insns_size)` shape
//!
//! ## What we strip
//!
//! - **SHA-1 signature** (header bytes 12..32) — depends on body
//!   layout, useless for diffing
//! - **Adler32 checksum** (header bytes 8..12) — same
//! - **All section offsets** — those depend on the layout choices
//!   the writer made and naturally diverge between independent
//!   compilers
//! - **Source file line numbers** — debug info
//! - **Map list** — derivable from the section sizes/offsets in the
//!   header anyway
//!
//! ## Out of scope (for now)
//!
//! Per-instruction disassembly. We just record `insns_size` per
//! method. PR #3.5 will add a real instruction decoder so the diffs
//! catch lowering changes.

use byteorder::{LittleEndian, ReadBytesExt};
use std::fmt::Write as _;
use std::io::{Cursor, Read, Seek};

/// Normalize a DEX file's bytes into a diff-friendly text blob.
pub fn normalize_default(bytes: &[u8]) -> Result<String, String> {
    let mut p = DexParser::new(bytes)?;
    let dex = p.parse()?;
    Ok(render(&dex))
}

#[derive(Debug)]
struct DexFile {
    version: String,
    strings: Vec<String>,
    type_descriptors: Vec<String>,
    /// `(class_descriptor, name, return_descriptor, [param_descriptors])`
    methods: Vec<(String, String, String, Vec<String>)>,
    classes: Vec<ClassSummary>,
}

#[derive(Debug)]
struct ClassSummary {
    descriptor: String,
    super_descriptor: String,
    source_file: Option<String>,
    direct_methods: Vec<MethodSummary>,
    virtual_methods: Vec<MethodSummary>,
}

#[derive(Debug)]
struct MethodSummary {
    name: String,
    /// Reconstructed signature like `(Ljava/lang/String;)V`.
    descriptor: String,
    access_flags: u32,
    registers_size: u16,
    ins_size: u16,
    outs_size: u16,
    insns_size: u32,
}

struct DexParser<'a> {
    bytes: &'a [u8],
}

impl<'a> DexParser<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < 0x70 {
            return Err("dex file too short".into());
        }
        if &bytes[0..4] != b"dex\n" {
            return Err("missing dex magic".into());
        }
        Ok(DexParser { bytes })
    }

    fn parse(&mut self) -> Result<DexFile, String> {
        let version = String::from_utf8_lossy(&self.bytes[4..7]).into_owned();

        // Header offsets — see DEX spec.
        let mut hdr = Cursor::new(&self.bytes[..0x70]);
        hdr.set_position(0x38); // string_ids_size
        let string_ids_size = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let string_ids_off = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let type_ids_size = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let type_ids_off = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let proto_ids_size = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let proto_ids_off = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let _field_ids_size = hdr.read_u32::<LittleEndian>().map_err(e)?;
        let _field_ids_off = hdr.read_u32::<LittleEndian>().map_err(e)?;
        let method_ids_size = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let method_ids_off = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let class_defs_size = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;
        let class_defs_off = hdr.read_u32::<LittleEndian>().map_err(e)? as usize;

        // Read strings.
        let mut strings = Vec::with_capacity(string_ids_size);
        for i in 0..string_ids_size {
            let id_off = string_ids_off + i * 4;
            let mut c = Cursor::new(&self.bytes[id_off..id_off + 4]);
            let data_off = c.read_u32::<LittleEndian>().map_err(e)? as usize;
            // string_data_item: uleb128 utf16_size + MUTF-8 bytes + 0x00.
            let (_size, bytes_consumed) = read_uleb128(&self.bytes[data_off..])?;
            let s_start = data_off + bytes_consumed;
            // Find the null terminator.
            let mut s_end = s_start;
            while s_end < self.bytes.len() && self.bytes[s_end] != 0 {
                s_end += 1;
            }
            let s = String::from_utf8_lossy(&self.bytes[s_start..s_end]).into_owned();
            strings.push(s);
        }

        // Read type descriptors via type_ids.
        let mut type_descriptors = Vec::with_capacity(type_ids_size);
        for i in 0..type_ids_size {
            let id_off = type_ids_off + i * 4;
            let mut c = Cursor::new(&self.bytes[id_off..id_off + 4]);
            let str_idx = c.read_u32::<LittleEndian>().map_err(e)? as usize;
            type_descriptors.push(strings[str_idx].clone());
        }

        // Read protos: (shorty_idx, return_type_idx, parameters_off).
        let mut protos: Vec<(String, Vec<String>)> = Vec::with_capacity(proto_ids_size);
        for i in 0..proto_ids_size {
            let off = proto_ids_off + i * 12;
            let mut c = Cursor::new(&self.bytes[off..off + 12]);
            let _shorty = c.read_u32::<LittleEndian>().map_err(e)?;
            let ret_idx = c.read_u32::<LittleEndian>().map_err(e)? as usize;
            let parameters_off = c.read_u32::<LittleEndian>().map_err(e)? as usize;
            let return_desc = type_descriptors[ret_idx].clone();
            let params = if parameters_off == 0 {
                Vec::new()
            } else {
                read_type_list(self.bytes, parameters_off, &type_descriptors)?
            };
            protos.push((return_desc, params));
        }

        // Read methods: (class_idx, proto_idx, name_idx).
        let mut methods = Vec::with_capacity(method_ids_size);
        for i in 0..method_ids_size {
            let off = method_ids_off + i * 8;
            let mut c = Cursor::new(&self.bytes[off..off + 8]);
            let class_idx = c.read_u16::<LittleEndian>().map_err(e)? as usize;
            let proto_idx = c.read_u16::<LittleEndian>().map_err(e)? as usize;
            let name_idx = c.read_u32::<LittleEndian>().map_err(e)? as usize;
            let class_desc = type_descriptors[class_idx].clone();
            let name = strings[name_idx].clone();
            let (ret, params) = protos[proto_idx].clone();
            methods.push((class_desc, name, ret, params));
        }

        // Read class defs (32 bytes each).
        let mut classes = Vec::with_capacity(class_defs_size);
        for i in 0..class_defs_size {
            let off = class_defs_off + i * 32;
            let mut c = Cursor::new(&self.bytes[off..off + 32]);
            let class_idx = c.read_u32::<LittleEndian>().map_err(e)? as usize;
            let _access_flags = c.read_u32::<LittleEndian>().map_err(e)?;
            let super_idx = c.read_u32::<LittleEndian>().map_err(e)? as usize;
            let _interfaces_off = c.read_u32::<LittleEndian>().map_err(e)?;
            let source_file_idx = c.read_u32::<LittleEndian>().map_err(e)?;
            let _annotations_off = c.read_u32::<LittleEndian>().map_err(e)?;
            let class_data_off = c.read_u32::<LittleEndian>().map_err(e)? as usize;
            let _static_values_off = c.read_u32::<LittleEndian>().map_err(e)?;

            let descriptor = type_descriptors[class_idx].clone();
            let super_descriptor = if super_idx < type_descriptors.len() {
                type_descriptors[super_idx].clone()
            } else {
                "<none>".to_string()
            };
            let source_file = if source_file_idx == u32::MAX {
                None
            } else {
                Some(strings[source_file_idx as usize].clone())
            };
            let (direct_methods, virtual_methods) = if class_data_off == 0 {
                (Vec::new(), Vec::new())
            } else {
                read_class_data(self.bytes, class_data_off, &methods)?
            };
            classes.push(ClassSummary {
                descriptor,
                super_descriptor,
                source_file,
                direct_methods,
                virtual_methods,
            });
        }

        // Don't return the proto list directly; we just kept it long
        // enough to reconstruct method signatures.
        let _ = protos;

        Ok(DexFile {
            version,
            strings,
            type_descriptors,
            methods,
            classes,
        })
    }
}

fn read_type_list(bytes: &[u8], off: usize, types: &[String]) -> Result<Vec<String>, String> {
    let mut c = Cursor::new(&bytes[off..]);
    let size = c.read_u32::<LittleEndian>().map_err(e)? as usize;
    let mut out = Vec::with_capacity(size);
    for _ in 0..size {
        let idx = c.read_u16::<LittleEndian>().map_err(e)? as usize;
        out.push(types[idx].clone());
    }
    Ok(out)
}

fn read_class_data(
    bytes: &[u8],
    off: usize,
    methods: &[(String, String, String, Vec<String>)],
) -> Result<(Vec<MethodSummary>, Vec<MethodSummary>), String> {
    let (static_fields_size, n1) = read_uleb128(&bytes[off..])?;
    let (instance_fields_size, n2) = read_uleb128(&bytes[off + n1..])?;
    let (direct_size, n3) = read_uleb128(&bytes[off + n1 + n2..])?;
    let (virtual_size, n4) = read_uleb128(&bytes[off + n1 + n2 + n3..])?;
    let mut cursor = off + n1 + n2 + n3 + n4;

    // Skip encoded_field entries: 2 uleb128s each (field_idx_diff,
    // access_flags). PR #3 doesn't emit any.
    for _ in 0..(static_fields_size + instance_fields_size) {
        let (_, a) = read_uleb128(&bytes[cursor..])?;
        cursor += a;
        let (_, b) = read_uleb128(&bytes[cursor..])?;
        cursor += b;
    }

    let mut direct = Vec::with_capacity(direct_size as usize);
    let mut prev_method_idx: u32 = 0;
    for i in 0..direct_size {
        let (diff, a) = read_uleb128(&bytes[cursor..])?;
        cursor += a;
        let (access, b) = read_uleb128(&bytes[cursor..])?;
        cursor += b;
        let (code_off, c) = read_uleb128(&bytes[cursor..])?;
        cursor += c;
        let method_idx = if i == 0 { diff } else { prev_method_idx + diff };
        prev_method_idx = method_idx;
        let m = method_summary(bytes, methods, method_idx, access, code_off)?;
        direct.push(m);
    }
    let mut virt = Vec::with_capacity(virtual_size as usize);
    let mut prev_v: u32 = 0;
    for i in 0..virtual_size {
        let (diff, a) = read_uleb128(&bytes[cursor..])?;
        cursor += a;
        let (access, b) = read_uleb128(&bytes[cursor..])?;
        cursor += b;
        let (code_off, c) = read_uleb128(&bytes[cursor..])?;
        cursor += c;
        let method_idx = if i == 0 { diff } else { prev_v + diff };
        prev_v = method_idx;
        let m = method_summary(bytes, methods, method_idx, access, code_off)?;
        virt.push(m);
    }
    Ok((direct, virt))
}

fn method_summary(
    bytes: &[u8],
    methods: &[(String, String, String, Vec<String>)],
    method_idx: u32,
    access_flags: u32,
    code_off: u32,
) -> Result<MethodSummary, String> {
    let (_class, name, ret, params) = &methods[method_idx as usize];
    let mut desc = String::from("(");
    for p in params {
        desc.push_str(p);
    }
    desc.push(')');
    desc.push_str(ret);

    let (registers_size, ins_size, outs_size, insns_size) = if code_off == 0 {
        (0, 0, 0, 0)
    } else {
        let mut c = Cursor::new(&bytes[code_off as usize..code_off as usize + 16]);
        let r = c.read_u16::<LittleEndian>().map_err(e)?;
        let i = c.read_u16::<LittleEndian>().map_err(e)?;
        let o = c.read_u16::<LittleEndian>().map_err(e)?;
        let _tries = c.read_u16::<LittleEndian>().map_err(e)?;
        let _debug = c.read_u32::<LittleEndian>().map_err(e)?;
        let isz = c.read_u32::<LittleEndian>().map_err(e)?;
        (r, i, o, isz)
    };

    Ok(MethodSummary {
        name: name.clone(),
        descriptor: desc,
        access_flags,
        registers_size,
        ins_size,
        outs_size,
        insns_size,
    })
}

fn read_uleb128(bytes: &[u8]) -> Result<(u32, usize), String> {
    let mut result: u32 = 0;
    let mut shift = 0;
    for (i, &b) in bytes.iter().enumerate() {
        result |= ((b & 0x7f) as u32) << shift;
        if b & 0x80 == 0 {
            return Ok((result, i + 1));
        }
        shift += 7;
        if shift >= 32 {
            return Err("uleb128 overflow".into());
        }
    }
    Err("uleb128 truncated".into())
}

fn render(dex: &DexFile) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "dex_version   {}", dex.version);
    let _ = writeln!(out, "strings       {}", dex.strings.len());
    let _ = writeln!(out, "types         {}", dex.type_descriptors.len());
    let _ = writeln!(out, "methods       {}", dex.methods.len());
    let _ = writeln!(out, "classes       {}", dex.classes.len());

    let _ = writeln!(out, "\n--- strings ---");
    let mut sorted_strings = dex.strings.clone();
    sorted_strings.sort();
    for s in &sorted_strings {
        let _ = writeln!(out, "  {s:?}");
    }

    let _ = writeln!(out, "\n--- types ---");
    let mut sorted_types = dex.type_descriptors.clone();
    sorted_types.sort();
    for t in &sorted_types {
        let _ = writeln!(out, "  {t}");
    }

    let _ = writeln!(out, "\n--- methods ---");
    let mut sorted_methods: Vec<String> = dex
        .methods
        .iter()
        .map(|(c, n, r, p)| {
            let mut s = format!("  {c}.{n}(");
            for x in p {
                s.push_str(x);
            }
            s.push(')');
            s.push_str(r);
            s
        })
        .collect();
    sorted_methods.sort();
    for m in &sorted_methods {
        let _ = writeln!(out, "{m}");
    }

    for (i, cls) in dex.classes.iter().enumerate() {
        let _ = writeln!(out, "\n--- class[{i}] ---");
        let _ = writeln!(out, "  descriptor   {}", cls.descriptor);
        let _ = writeln!(out, "  super        {}", cls.super_descriptor);
        if let Some(sf) = &cls.source_file {
            let _ = writeln!(out, "  source_file  {sf}");
        }
        let mut all_methods: Vec<&MethodSummary> = cls
            .direct_methods
            .iter()
            .chain(cls.virtual_methods.iter())
            .collect();
        all_methods.sort_by(|a, b| (&a.name, &a.descriptor).cmp(&(&b.name, &b.descriptor)));
        for m in &all_methods {
            let _ = writeln!(
                out,
                "  method       {}{} flags=0x{:04X} regs={} ins={} outs={} insns={}",
                m.name,
                m.descriptor,
                m.access_flags,
                m.registers_size,
                m.ins_size,
                m.outs_size,
                m.insns_size,
            );
        }
    }
    out
}

fn e(err: std::io::Error) -> String {
    format!("dex parse error: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_input() {
        assert!(normalize_default(&[]).is_err());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = vec![0u8; 0x70];
        bytes[0..4].copy_from_slice(b"NOPE");
        assert!(normalize_default(&bytes).is_err());
    }

    #[test]
    fn uleb128_short_values() {
        assert_eq!(read_uleb128(&[0x00]).unwrap(), (0, 1));
        assert_eq!(read_uleb128(&[0x7f]).unwrap(), (127, 1));
        assert_eq!(read_uleb128(&[0x80, 0x01]).unwrap(), (128, 2));
        assert_eq!(read_uleb128(&[0xac, 0x02]).unwrap(), (300, 2));
    }
}

// Suppress unused warnings on the `Seek` import (only used for
// hypothetical future code paths).
#[allow(dead_code)]
fn _force_seek<R: Seek + Read>(_: R) {}
