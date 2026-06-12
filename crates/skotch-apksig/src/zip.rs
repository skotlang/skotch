//! Minimal ZIP structure handling, ported from apksig's `internal/zip` and
//! `ApkUtils`/`ZipUtils`.
//!
//! This deliberately does NOT use a general-purpose zip crate: signing must
//! copy input entries byte-for-byte (preserving headers, flags, timestamps,
//! data descriptors and even misalignment) and only patch what apksig
//! patches, so we operate directly on offsets into the raw archive.

use anyhow::{bail, Context, Result};

pub const EOCD_SIG: u32 = 0x0605_4b50;
pub const CD_RECORD_SIG: u32 = 0x0201_4b50;
pub const LFH_SIG: u32 = 0x0403_4b50;
pub const DATA_DESCRIPTOR_SIG: u32 = 0x0807_4b50;

/// General-purpose flag: data descriptor used.
pub const GP_FLAG_DATA_DESCRIPTOR_USED: u16 = 0x0008;
/// General-purpose flag: UTF-8 entry names.
pub const GP_FLAG_EFS: u16 = 0x0800;
/// General-purpose flag: encrypted.
pub const GP_FLAG_ENCRYPTED: u16 = 0x0001;

pub const COMPRESSION_STORED: u16 = 0;
pub const COMPRESSION_DEFLATED: u16 = 8;

pub fn u16le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

pub fn u32le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

pub fn u64le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

/// The three logical sections of an APK relative to signing, plus EOCD info.
#[derive(Debug, Clone)]
pub struct ZipSections {
    /// Offset of the ZIP Central Directory.
    pub cd_offset: usize,
    /// Size of the Central Directory in bytes.
    pub cd_size: usize,
    /// Number of records in the Central Directory.
    pub cd_record_count: usize,
    /// Offset of the End of Central Directory record.
    pub eocd_offset: usize,
}

impl ZipSections {
    pub fn eocd<'a>(&self, apk: &'a [u8]) -> &'a [u8] {
        &apk[self.eocd_offset..]
    }
}

/// Locates the EOCD record exactly as `ZipUtils.findZipEndOfCentralDirectoryRecord`:
/// for each possible comment length (0..=65535), check that the signature is
/// present AND the comment-length field matches.
pub fn find_eocd(apk: &[u8]) -> Option<usize> {
    const EOCD_MIN: usize = 22;
    if apk.len() < EOCD_MIN {
        return None;
    }
    let max_comment = (apk.len() - EOCD_MIN).min(0xffff);
    for comment_len in 0..=max_comment {
        let eocd_start = apk.len() - EOCD_MIN - comment_len;
        if u32le(apk, eocd_start) == EOCD_SIG
            && u16le(apk, eocd_start + 20) as usize == comment_len
        {
            return Some(eocd_start);
        }
    }
    None
}

/// Port of `ApkUtils.findZipSections`.
pub fn find_zip_sections(apk: &[u8]) -> Result<ZipSections> {
    let eocd_offset = find_eocd(apk).context("ZIP End of Central Directory record not found")?;
    let cd_offset = u32le(apk, eocd_offset + 16) as usize;
    let cd_size = u32le(apk, eocd_offset + 12) as usize;
    let cd_record_count = u16le(apk, eocd_offset + 10) as usize;
    if cd_offset > eocd_offset {
        bail!(
            "ZIP Central Directory start offset out of range: {cd_offset}. ZIP End of Central Directory offset: {eocd_offset}"
        );
    }
    if cd_offset + cd_size != eocd_offset {
        bail!("ZIP Central Directory is not immediately followed by End of Central Directory");
    }
    Ok(ZipSections {
        cd_offset,
        cd_size,
        cd_record_count,
        eocd_offset,
    })
}

pub const APK_SIGNING_BLOCK_MAGIC: &[u8; 16] = b"APK Sig Block 42";
pub const APK_SIGNING_BLOCK_MIN_SIZE: usize = 32;

/// Result of locating an APK Signing Block.
#[derive(Debug, Clone)]
pub struct ApkSigningBlockInfo {
    /// Offset of the start of the APK Signing Block.
    pub start_offset: usize,
    /// The whole signing block, including size fields and magic.
    pub size: usize,
}

/// Port of `ApkUtils.findApkSigningBlock`. Returns `Ok(None)` if the APK has
/// no signing block, `Err` if a malformed block is present.
pub fn find_apk_signing_block(apk: &[u8], sections: &ZipSections) -> Result<Option<ApkSigningBlockInfo>> {
    let cd_offset = sections.cd_offset;
    if cd_offset < APK_SIGNING_BLOCK_MIN_SIZE {
        return Ok(None);
    }
    let footer_offset = cd_offset - 24;
    if &apk[footer_offset + 8..footer_offset + 24] != APK_SIGNING_BLOCK_MAGIC {
        return Ok(None);
    }
    let block_size = u64le(apk, footer_offset);
    if block_size < 24 || block_size > (i32::MAX as u64 - 8) {
        bail!("APK Signing Block size out of range: {block_size}");
    }
    let total_size = (block_size + 8) as usize;
    if total_size > cd_offset {
        bail!("APK Signing Block offset out of range");
    }
    let start_offset = cd_offset - total_size;
    let header_size = u64le(apk, start_offset);
    if header_size != block_size {
        bail!(
            "APK Signing Block sizes in header and footer do not match: {header_size} vs {block_size}"
        );
    }
    Ok(Some(ApkSigningBlockInfo {
        start_offset,
        size: total_size,
    }))
}

/// One parsed Central Directory record (`CentralDirectoryRecord.java`).
#[derive(Debug, Clone)]
pub struct CdRecord {
    /// Raw bytes of the whole CD record (header + name + extra + comment).
    pub raw: Vec<u8>,
    pub gp_flags: u16,
    pub compression_method: u16,
    pub last_modified_time: u16,
    pub last_modified_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub lfh_offset: u32,
    pub name: String,
}

const CD_HEADER_SIZE: usize = 46;
/// Offset of the LFH-offset field within a CD record.
const CD_LFH_OFFSET_OFFSET: usize = 42;

impl CdRecord {
    pub fn uses_data_descriptor(&self) -> bool {
        self.gp_flags & GP_FLAG_DATA_DESCRIPTOR_USED != 0
    }

    /// Re-emits this record with a different Local File Header offset
    /// (`createWithModifiedLocalFileHeaderOffset`).
    pub fn with_lfh_offset(&self, new_offset: u32) -> CdRecord {
        let mut raw = self.raw.clone();
        raw[CD_LFH_OFFSET_OFFSET..CD_LFH_OFFSET_OFFSET + 4]
            .copy_from_slice(&new_offset.to_le_bytes());
        CdRecord {
            raw,
            lfh_offset: new_offset,
            ..self.clone()
        }
    }

    /// Builds a CD record for a newly generated deflate-compressed entry
    /// (`createWithDeflateCompressedData`).
    pub fn new_deflated(
        name: &str,
        last_modified_time: u16,
        last_modified_date: u16,
        crc32: u32,
        compressed_size: u32,
        uncompressed_size: u32,
        lfh_offset: u32,
    ) -> CdRecord {
        let name_bytes = name.as_bytes();
        let mut raw = Vec::with_capacity(CD_HEADER_SIZE + name_bytes.len());
        raw.extend_from_slice(&CD_RECORD_SIG.to_le_bytes());
        raw.extend_from_slice(&0x14u16.to_le_bytes()); // version made by
        raw.extend_from_slice(&0x14u16.to_le_bytes()); // version needed to extract
        raw.extend_from_slice(&GP_FLAG_EFS.to_le_bytes());
        raw.extend_from_slice(&COMPRESSION_DEFLATED.to_le_bytes());
        raw.extend_from_slice(&last_modified_time.to_le_bytes());
        raw.extend_from_slice(&last_modified_date.to_le_bytes());
        raw.extend_from_slice(&crc32.to_le_bytes());
        raw.extend_from_slice(&compressed_size.to_le_bytes());
        raw.extend_from_slice(&uncompressed_size.to_le_bytes());
        raw.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        raw.extend_from_slice(&0u16.to_le_bytes()); // extra length
        raw.extend_from_slice(&0u16.to_le_bytes()); // comment length
        raw.extend_from_slice(&0u16.to_le_bytes()); // disk number
        raw.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        raw.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        raw.extend_from_slice(&lfh_offset.to_le_bytes());
        raw.extend_from_slice(name_bytes);
        CdRecord {
            raw,
            gp_flags: GP_FLAG_EFS,
            compression_method: COMPRESSION_DEFLATED,
            last_modified_time,
            last_modified_date,
            crc32,
            compressed_size,
            uncompressed_size,
            lfh_offset,
            name: name.to_string(),
        }
    }
}

/// Parses all Central Directory records, in central-directory order.
pub fn parse_central_directory(apk: &[u8], sections: &ZipSections) -> Result<Vec<CdRecord>> {
    let cd = &apk[sections.cd_offset..sections.cd_offset + sections.cd_size];
    let mut records = Vec::with_capacity(sections.cd_record_count);
    let mut pos = 0usize;
    for i in 0..sections.cd_record_count {
        if cd.len() - pos < CD_HEADER_SIZE {
            bail!("Malformed ZIP CD record #{}: too short", i + 1);
        }
        if u32le(cd, pos) != CD_RECORD_SIG {
            bail!("Malformed ZIP CD record #{}: bad signature", i + 1);
        }
        let gp_flags = u16le(cd, pos + 8);
        let compression_method = u16le(cd, pos + 10);
        let last_modified_time = u16le(cd, pos + 12);
        let last_modified_date = u16le(cd, pos + 14);
        let crc32 = u32le(cd, pos + 16);
        let compressed_size = u32le(cd, pos + 20);
        let uncompressed_size = u32le(cd, pos + 24);
        let name_len = u16le(cd, pos + 28) as usize;
        let extra_len = u16le(cd, pos + 30) as usize;
        let comment_len = u16le(cd, pos + 32) as usize;
        let lfh_offset = u32le(cd, pos + 42);
        let record_size = CD_HEADER_SIZE + name_len + extra_len + comment_len;
        if cd.len() - pos < record_size {
            bail!("Malformed ZIP CD record #{}: extends past CD", i + 1);
        }
        let name = String::from_utf8_lossy(&cd[pos + CD_HEADER_SIZE..pos + CD_HEADER_SIZE + name_len])
            .into_owned();
        records.push(CdRecord {
            raw: cd[pos..pos + record_size].to_vec(),
            gp_flags,
            compression_method,
            last_modified_time,
            last_modified_date,
            crc32,
            compressed_size,
            uncompressed_size,
            lfh_offset,
            name,
        });
        pos += record_size;
    }
    Ok(records)
}

/// One parsed Local File Header record (`LocalFileRecord.java`), referencing
/// the LFH section of the source archive by offset.
#[derive(Debug, Clone)]
pub struct LocalFileRecord {
    /// Offset of the record within the LFH section.
    pub start_offset: usize,
    /// Total size of the record: header + name + extra + data (+ descriptor).
    pub size: usize,
    /// Offset of the entry data, relative to `start_offset`.
    pub data_start_offset: usize,
    /// Compressed data size in bytes.
    pub data_size: usize,
    /// Whether the entry data is compressed (method != STORED).
    pub data_compressed: bool,
    /// Uncompressed size as recorded in the CD.
    pub uncompressed_size: usize,
    /// Extra field bytes from the LFH.
    pub extra: Vec<u8>,
    pub name: String,
}

const LFH_HEADER_SIZE: usize = 30;
/// Offset of the extra-length field within a LFH.
pub const LFH_EXTRA_LENGTH_OFFSET: usize = 28;

/// Port of `LocalFileRecord.getRecord` (without data-sink streaming): parses
/// the LFH referenced by `cd` inside `lfh_section` and computes the record's
/// total span, including any data descriptor.
pub fn parse_local_file_record(
    lfh_section: &[u8],
    cd: &CdRecord,
) -> Result<LocalFileRecord> {
    let offset = cd.lfh_offset as usize;
    if offset + LFH_HEADER_SIZE > lfh_section.len() {
        bail!(
            "Local File Header of entry \"{}\" extends past the archive",
            cd.name
        );
    }
    if u32le(lfh_section, offset) != LFH_SIG {
        bail!(
            "Not a Local File Header record for entry \"{}\" at offset {offset}",
            cd.name
        );
    }
    let gp_flags = u16le(lfh_section, offset + 6);
    if gp_flags & GP_FLAG_ENCRYPTED != 0 {
        bail!("Entry \"{}\" is encrypted", cd.name);
    }
    let data_descriptor_used = gp_flags & GP_FLAG_DATA_DESCRIPTOR_USED != 0;
    let name_len = u16le(lfh_section, offset + 26) as usize;
    let extra_len = u16le(lfh_section, offset + LFH_EXTRA_LENGTH_OFFSET) as usize;
    let data_start = LFH_HEADER_SIZE + name_len + extra_len;
    let compressed_size = cd.compressed_size as usize;
    let data_compressed = cd.compression_method != COMPRESSION_STORED;
    let data_size = if data_compressed {
        compressed_size
    } else {
        cd.uncompressed_size as usize
    };
    let mut record_size = data_start + data_size;
    if data_descriptor_used {
        let descriptor_offset = offset + record_size;
        let mut descriptor_size = 12;
        if descriptor_offset + 4 <= lfh_section.len()
            && u32le(lfh_section, descriptor_offset) == DATA_DESCRIPTOR_SIG
        {
            descriptor_size += 4;
        }
        record_size += descriptor_size;
    }
    if offset + record_size > lfh_section.len() {
        bail!("Entry \"{}\" data extends past the archive", cd.name);
    }
    let extra = lfh_section[offset + LFH_HEADER_SIZE + name_len..offset + data_start].to_vec();
    Ok(LocalFileRecord {
        start_offset: offset,
        size: record_size,
        data_start_offset: data_start,
        data_size,
        data_compressed,
        uncompressed_size: cd.uncompressed_size as usize,
        extra,
        name: cd.name.clone(),
    })
}

impl LocalFileRecord {
    /// Returns the entry's uncompressed data, inflating if necessary.
    pub fn uncompressed_data(&self, lfh_section: &[u8]) -> Result<Vec<u8>> {
        let data_start = self.start_offset + self.data_start_offset;
        let data = &lfh_section[data_start..data_start + self.data_size];
        if !self.data_compressed {
            return Ok(data.to_vec());
        }
        inflate_raw(data, self.uncompressed_size)
            .with_context(|| format!("inflating entry \"{}\"", self.name))
    }
}

/// Raw-deflate decompression (no zlib wrapper).
pub fn inflate_raw(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::with_capacity(size_hint);
    let mut decoder = flate2::read::DeflateDecoder::new(data);
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Raw-deflate compression at level 9, matching `java.util.zip.Deflater(9, true)`
/// byte-for-byte (flate2 is built with the C zlib backend).
pub fn deflate_level9(data: &[u8]) -> (Vec<u8>, u32) {
    use std::io::Write;
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
    encoder.write_all(data).expect("in-memory deflate");
    let compressed = encoder.finish().expect("in-memory deflate");
    let mut crc = flate2::Crc::new();
    crc.update(data);
    (compressed, crc.sum())
}

/// Builds a LFH for a newly generated deflate-compressed entry
/// (`outputRecordWithDeflateCompressedData`). Returns the full record bytes
/// (header + name + compressed data).
pub fn lfh_record_with_deflate_data(
    name: &str,
    last_modified_time: u16,
    last_modified_date: u16,
    compressed_data: &[u8],
    crc32: u32,
    uncompressed_size: u32,
) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let mut out = Vec::with_capacity(LFH_HEADER_SIZE + name_bytes.len() + compressed_data.len());
    out.extend_from_slice(&LFH_SIG.to_le_bytes());
    out.extend_from_slice(&0x14u16.to_le_bytes()); // version needed to extract
    out.extend_from_slice(&GP_FLAG_EFS.to_le_bytes());
    out.extend_from_slice(&COMPRESSION_DEFLATED.to_le_bytes());
    out.extend_from_slice(&last_modified_time.to_le_bytes());
    out.extend_from_slice(&last_modified_date.to_le_bytes());
    out.extend_from_slice(&crc32.to_le_bytes());
    out.extend_from_slice(&(compressed_data.len() as u32).to_le_bytes());
    out.extend_from_slice(&uncompressed_size.to_le_bytes());
    out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra length
    out.extend_from_slice(name_bytes);
    out.extend_from_slice(compressed_data);
    out
}

/// EOCD helpers (`EocdRecord` / `ZipUtils`).
pub mod eocd {
    /// Returns a copy with patched record counts, CD size and CD offset
    /// (`createWithModifiedCentralDirectoryInfo`).
    pub fn with_modified_cd_info(
        eocd: &[u8],
        record_count: u16,
        cd_size: u32,
        cd_offset: u32,
    ) -> Vec<u8> {
        let mut out = eocd.to_vec();
        out[8..10].copy_from_slice(&record_count.to_le_bytes());
        out[10..12].copy_from_slice(&record_count.to_le_bytes());
        out[12..16].copy_from_slice(&cd_size.to_le_bytes());
        out[16..20].copy_from_slice(&cd_offset.to_le_bytes());
        out
    }

    /// Appends `padding` zero bytes as (additional) EOCD comment
    /// (`createWithPaddedComment`).
    pub fn with_padded_comment(eocd: &[u8], padding: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(eocd.len() + padding);
        out.extend_from_slice(eocd);
        out.resize(eocd.len() + padding, 0);
        let comment_len = (out.len() - 22) as u16;
        out[20..22].copy_from_slice(&comment_len.to_le_bytes());
        out
    }

    /// Patches the central-directory offset field.
    pub fn set_cd_offset(eocd: &mut [u8], cd_offset: u32) {
        eocd[16..20].copy_from_slice(&cd_offset.to_le_bytes());
    }
}
