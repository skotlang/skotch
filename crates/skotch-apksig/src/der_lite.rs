//! A minimal DER reader (tag/length/value walking) shared by the keystore
//! and PKCS#7/PKCS#12 parsers. Definite-length only, which is all these
//! structures use.

use anyhow::{bail, Context, Result};

/// One TLV: its tag byte, content slice, and total encoded length.
#[derive(Clone, Copy)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub total_len: usize,
}

/// Reads the TLV starting at `offset`.
pub fn read<'a>(data: &'a [u8], offset: usize) -> Result<Tlv<'a>> {
    if offset + 2 > data.len() {
        bail!("truncated DER at offset {offset}");
    }
    let tag = data[offset];
    let len_byte = data[offset + 1];
    let (content_start, length) = if len_byte & 0x80 == 0 {
        (offset + 2, len_byte as usize)
    } else {
        let num = (len_byte & 0x7f) as usize;
        if num == 0 || num > 4 || offset + 2 + num > data.len() {
            bail!("invalid DER length at offset {offset}");
        }
        let mut len = 0usize;
        for i in 0..num {
            len = (len << 8) | data[offset + 2 + i] as usize;
        }
        (offset + 2 + num, len)
    };
    if content_start + length > data.len() {
        bail!("DER content exceeds buffer at offset {offset}");
    }
    Ok(Tlv {
        tag,
        content: &data[content_start..content_start + length],
        total_len: (content_start - offset) + length,
    })
}

/// Reads a SEQUENCE (or context-constructed) TLV at `offset`.
pub fn sequence<'a>(data: &'a [u8], offset: usize) -> Result<Tlv<'a>> {
    let tlv = read(data, offset)?;
    if tlv.tag != 0x30 && tlv.tag & 0xa0 != 0xa0 {
        bail!("expected SEQUENCE, got tag {:#x}", tlv.tag);
    }
    Ok(tlv)
}

/// Returns the inner bytes of an OCTET STRING whose content begins at the
/// start of `data` (i.e. `data` is the OCTET STRING's content already wrapping
/// another structure). Used for `[0] EXPLICIT content` whose inner value is an
/// OCTET STRING.
pub fn octet_string_inner(data: &[u8]) -> Result<&[u8]> {
    let tlv = read(data, 0)?;
    if tlv.tag != 0x04 {
        bail!("expected OCTET STRING, got tag {:#x}", tlv.tag);
    }
    Ok(tlv.content)
}

/// Returns the content of an OCTET STRING TLV.
pub fn octet_string_inner_tlv<'a>(tlv: Tlv<'a>) -> Result<&'a [u8]> {
    if tlv.tag != 0x04 {
        bail!("expected OCTET STRING, got tag {:#x}", tlv.tag);
    }
    Ok(tlv.content)
}

/// Decodes an OBJECT IDENTIFIER TLV to dotted-decimal form.
pub fn oid_string(tlv: Tlv) -> Result<String> {
    if tlv.tag != 0x06 {
        bail!("expected OBJECT IDENTIFIER, got tag {:#x}", tlv.tag);
    }
    let b = tlv.content;
    if b.is_empty() {
        bail!("empty OID");
    }
    let mut parts = Vec::new();
    let first = b[0] as u32;
    parts.push((first / 40).to_string());
    parts.push((first % 40).to_string());
    let mut value: u64 = 0;
    for &byte in &b[1..] {
        value = (value << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            parts.push(value.to_string());
            value = 0;
        }
    }
    Ok(parts.join("."))
}

/// Reads an INTEGER TLV as a `u32` (small magnitudes only).
pub fn integer_u32(tlv: Tlv) -> Result<u32> {
    if tlv.tag != 0x02 {
        bail!("expected INTEGER, got tag {:#x}", tlv.tag);
    }
    let mut v: u32 = 0;
    for &b in tlv.content {
        v = (v << 8) | b as u32;
    }
    Ok(v)
}

/// A forward cursor over a sequence of TLVs.
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Cursor<'a> {
        Cursor { data, pos: 0 }
    }

    /// Reads the next TLV, advancing past it.
    pub fn tlv(&mut self) -> Result<Tlv<'a>> {
        self.try_tlv()?.context("unexpected end of DER sequence")
    }

    /// Reads the next TLV if any remain.
    pub fn try_tlv(&mut self) -> Result<Option<Tlv<'a>>> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }
        let tlv = read(self.data, self.pos)?;
        self.pos += tlv.total_len;
        Ok(Some(tlv))
    }
}
