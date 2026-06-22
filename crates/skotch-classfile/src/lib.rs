//! JVM `.class` file reader for the native `skotch d8` dexer.
//!
//! Parses the constant pool, class/field/method structure, the `Code`
//! attribute (bytecode + exception table), and debug attributes
//! (`SourceFile`, `LineNumberTable`, `LocalVariableTable`). Independent of
//! Skotch's compiler backends.

pub mod constant_pool;
pub mod model;
pub mod reader;

pub use model::ClassFile;
pub use reader::parse_class;

use anyhow::{bail, Context, Result};
use std::io::Read;

/// Reads all `.class` entries from a ZIP-format archive (`.jar`/`.zip`/`.apk`),
/// returning each parsed class. Non-`.class` entries are skipped.
pub fn parse_archive(bytes: &[u8]) -> Result<Vec<ClassFile>> {
    let mut classes = Vec::new();
    for (name, data) in zip_entries(bytes)? {
        if name.ends_with(".class") {
            classes.push(parse_class(&data).with_context(|| format!("parsing {name}"))?);
        }
    }
    Ok(classes)
}

/// Reads a single `.class` file from disk.
pub fn parse_class_file(path: &std::path::Path) -> Result<ClassFile> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    parse_class(&bytes)
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Returns `(name, uncompressed_bytes)` for every entry in a ZIP archive.
pub fn zip_entries(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    if bytes.len() < 22 {
        bail!("not a ZIP archive");
    }
    let eocd = (0..bytes.len() - 22)
        .rev()
        .find(|&i| bytes[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .context("ZIP EOCD not found")?;
    let count = u16le(bytes, eocd + 10) as usize;
    let mut cd = u32le(bytes, eocd + 16) as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if bytes[cd..cd + 4] != [0x50, 0x4b, 0x01, 0x02] {
            bail!("bad central directory header");
        }
        let method = u16le(bytes, cd + 10);
        let comp_size = u32le(bytes, cd + 20) as usize;
        let uncomp_size = u32le(bytes, cd + 24) as usize;
        let name_len = u16le(bytes, cd + 28) as usize;
        let extra_len = u16le(bytes, cd + 30) as usize;
        let comment_len = u16le(bytes, cd + 32) as usize;
        let lho = u32le(bytes, cd + 42) as usize;
        let name = String::from_utf8_lossy(&bytes[cd + 46..cd + 46 + name_len]).into_owned();
        // Local header: data offset = lho + 30 + lh_name_len + lh_extra_len.
        let lh_name = u16le(bytes, lho + 26) as usize;
        let lh_extra = u16le(bytes, lho + 28) as usize;
        let data_off = lho + 30 + lh_name + lh_extra;
        let comp = &bytes[data_off..data_off + comp_size];
        let data = match method {
            0 => comp.to_vec(),
            8 => {
                let mut out = Vec::with_capacity(uncomp_size);
                flate2::read::DeflateDecoder::new(comp).read_to_end(&mut out)?;
                out
            }
            m => bail!("unsupported ZIP compression method {m} for {name}"),
        };
        out.push((name, data));
        cd += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}
