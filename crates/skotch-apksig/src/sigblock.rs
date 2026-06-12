//! APK Signing Block assembly and parsing plus the little-endian
//! length-prefixed encoding helpers shared by the v2/v3 wire formats
//! (`ApkSigningBlockUtils.java`).

use crate::zip::{u32le, u64le};
use anyhow::{bail, Result};

pub const ANDROID_COMMON_PAGE_ALIGNMENT: usize = 4096;
pub const VERITY_PADDING_BLOCK_ID: u32 = 0x4272_6577;

pub const V2_BLOCK_ID: u32 = 0x7109_871a;
pub const V3_BLOCK_ID: u32 = 0xf053_68c0;
pub const V31_BLOCK_ID: u32 = 0x1b93_ad61;
pub const V1_SOURCE_STAMP_BLOCK_ID: u32 = 0x2b09_189e;
pub const V2_SOURCE_STAMP_BLOCK_ID: u32 = 0x6dff_800d;

pub const STRIPPING_PROTECTION_ATTR_ID: u32 = 0xbeef_f00d;
pub const PROOF_OF_ROTATION_ATTR_ID: u32 = 0x3ba0_6f8c;
pub const ROTATION_MIN_SDK_VERSION_ATTR_ID: u32 = 0x559f_8b02;
pub const ROTATION_ON_DEV_RELEASE_ATTR_ID: u32 = 0xc2a6_b3ba;

/// Appends `data` with a u32 LE length prefix.
pub fn put_length_prefixed(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

/// `encodeAsLengthPrefixedElement`.
pub fn length_prefixed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    put_length_prefixed(&mut out, data);
    out
}

/// `encodeAsSequenceOfLengthPrefixedElements`.
pub fn sequence_of_length_prefixed<T: AsRef<[u8]>>(elements: &[T]) -> Vec<u8> {
    let total: usize = elements.iter().map(|e| 4 + e.as_ref().len()).sum();
    let mut out = Vec::with_capacity(total);
    for e in elements {
        put_length_prefixed(&mut out, e.as_ref());
    }
    out
}

/// `encodeAsSequenceOfLengthPrefixedPairsOfIntAndLengthPrefixedBytes`.
pub fn sequence_of_id_value_pairs(pairs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (id, value) in pairs {
        let mut pair = Vec::with_capacity(8 + value.len());
        pair.extend_from_slice(&id.to_le_bytes());
        put_length_prefixed(&mut pair, value);
        put_length_prefixed(&mut out, &pair);
    }
    out
}

/// A little-endian cursor over a byte slice for parsing the wire formats.
#[derive(Debug, Clone, Copy)]
pub struct Slice<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Slice<'a> {
    pub fn new(data: &'a [u8]) -> Slice<'a> {
        Slice { data, pos: 0 }
    }

    pub fn has_remaining(&self) -> bool {
        self.pos < self.data.len()
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn get_u32(&mut self) -> Result<u32> {
        if self.remaining() < 4 {
            bail!("Remaining buffer too short to contain uint32");
        }
        let v = u32le(self.data, self.pos);
        self.pos += 4;
        Ok(v)
    }

    pub fn get_u64(&mut self) -> Result<u64> {
        if self.remaining() < 8 {
            bail!("Remaining buffer too short to contain uint64");
        }
        let v = u64le(self.data, self.pos);
        self.pos += 8;
        Ok(v)
    }

    pub fn get_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.remaining() < len {
            bail!("Remaining buffer too short: need {len}, have {}", self.remaining());
        }
        let v = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(v)
    }

    /// `getLengthPrefixedSlice`.
    pub fn get_length_prefixed_slice(&mut self) -> Result<Slice<'a>> {
        let len = self.get_u32()? as usize;
        Ok(Slice::new(self.get_bytes(len)?))
    }

    /// `readLengthPrefixedByteArray`.
    pub fn get_length_prefixed_bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.get_u32()? as usize;
        self.get_bytes(len)
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

/// Builds the complete APK Signing Block from (block bytes, block id) pairs,
/// inserting the verity padding pair so the whole block is a multiple of
/// 4096 bytes (`generateApkSigningBlock`).
pub fn generate_apk_signing_block(scheme_blocks: &[(Vec<u8>, u32)]) -> Vec<u8> {
    let blocks_size: usize = scheme_blocks.iter().map(|(b, _)| 8 + 4 + b.len()).sum();
    let mut result_size = 8 + blocks_size + 8 + 16;
    let mut padding_pair: Option<Vec<u8>> = None;
    if result_size % ANDROID_COMMON_PAGE_ALIGNMENT != 0 {
        let mut padding = ANDROID_COMMON_PAGE_ALIGNMENT - (result_size % ANDROID_COMMON_PAGE_ALIGNMENT);
        if padding < 12 {
            padding += ANDROID_COMMON_PAGE_ALIGNMENT;
        }
        let mut pair = vec![0u8; padding];
        pair[0..8].copy_from_slice(&((padding - 8) as u64).to_le_bytes());
        pair[8..12].copy_from_slice(&VERITY_PADDING_BLOCK_ID.to_le_bytes());
        padding_pair = Some(pair);
        result_size += padding;
    }

    let mut out = Vec::with_capacity(result_size);
    let block_size_field = (result_size - 8) as u64;
    out.extend_from_slice(&block_size_field.to_le_bytes());
    for (block, id) in scheme_blocks {
        out.extend_from_slice(&((4 + block.len()) as u64).to_le_bytes());
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(block);
    }
    if let Some(pair) = padding_pair {
        out.extend_from_slice(&pair);
    }
    out.extend_from_slice(&block_size_field.to_le_bytes());
    out.extend_from_slice(crate::zip::APK_SIGNING_BLOCK_MAGIC);
    debug_assert_eq!(out.len(), result_size);
    out
}

/// Parses the ID-value pairs of an existing APK Signing Block
/// (`getApkSignatureBlocks`). `block` is the whole signing block including
/// the leading size field and trailing size+magic.
pub fn parse_apk_signing_block(block: &[u8]) -> Result<Vec<(u32, Vec<u8>)>> {
    if block.len() < 32 {
        bail!("APK Signing Block too small: {}", block.len());
    }
    let mut pairs = Vec::new();
    let mut cursor = Slice::new(&block[8..block.len() - 24]);
    while cursor.has_remaining() {
        let pair_size = cursor.get_u64()? as usize;
        if pair_size < 4 || pair_size > cursor.remaining() {
            bail!("APK Signing Block pair size out of range: {pair_size}");
        }
        let id = cursor.get_u32()?;
        let value = cursor.get_bytes(pair_size - 4)?;
        pairs.push((id, value.to_vec()));
    }
    Ok(pairs)
}

/// Finds the first block with the given id.
pub fn find_block(block: &[u8], block_id: u32) -> Result<Option<Vec<u8>>> {
    Ok(parse_apk_signing_block(block)?
        .into_iter()
        .find(|(id, _)| *id == block_id)
        .map(|(_, v)| v))
}

/// Pads `before_cd_len` up to a page boundary, returning the number of
/// padding bytes to append (`generateApkSigningBlockPadding`).
pub fn signing_block_padding(before_cd_len: usize) -> usize {
    if before_cd_len % ANDROID_COMMON_PAGE_ALIGNMENT != 0 {
        ANDROID_COMMON_PAGE_ALIGNMENT - (before_cd_len % ANDROID_COMMON_PAGE_ALIGNMENT)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_block_is_page_aligned_and_parses() {
        let block = generate_apk_signing_block(&[(vec![1, 2, 3], V2_BLOCK_ID)]);
        assert_eq!(block.len() % ANDROID_COMMON_PAGE_ALIGNMENT, 0);
        assert_eq!(&block[block.len() - 16..], crate::zip::APK_SIGNING_BLOCK_MAGIC);
        let pairs = parse_apk_signing_block(&block).unwrap();
        assert_eq!(pairs.len(), 2); // v2 + padding
        assert_eq!(pairs[0], (V2_BLOCK_ID, vec![1, 2, 3]));
        assert_eq!(pairs[1].0, VERITY_PADDING_BLOCK_ID);
    }

    #[test]
    fn length_prefixed_roundtrip() {
        let encoded = sequence_of_id_value_pairs(&[(0x0103, vec![9, 9])]);
        let mut s = Slice::new(&encoded);
        let mut pair = s.get_length_prefixed_slice().unwrap();
        assert_eq!(pair.get_u32().unwrap(), 0x0103);
        assert_eq!(pair.get_length_prefixed_bytes().unwrap(), &[9, 9]);
    }
}
