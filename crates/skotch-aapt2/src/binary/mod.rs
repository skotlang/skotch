//! Binary `resources.arsc` table format: chunk plumbing shared by the
//! parser and the flattener.
//!
//! Port of the chunk-level pieces of aapt2's `format/binary/`:
//! `ResChunkPullParser.{h,cpp}` (read side, [`ChunkIterator`]),
//! `ChunkWriter.h` (write side, [`ChunkWriter`]), and the chunk-type /
//! flag constants from `androidfw/ResourceTypes.h` and
//! `format/binary/ResourceTypeExtensions.h`.
//!
//! Every chunk starts with a `ResChunk_header`:
//!
//! ```text
//! type:       u16  -- chunk type (RES_* constants below)
//! headerSize: u16  -- size of the chunk header, data follows it
//! size:       u32  -- total chunk size including header and data
//! ```

pub mod arsc_flattener;
pub mod arsc_parser;

use byteorder::{ByteOrder, LittleEndian};

// ---------------------------------------------------------------------------
// Chunk type constants (androidfw/ResourceTypes.h).
// ---------------------------------------------------------------------------

/// `RES_NULL_TYPE`.
pub const RES_NULL_TYPE: u16 = 0x0000;
/// `RES_STRING_POOL_TYPE` — a `ResStringPool_header` chunk.
pub const RES_STRING_POOL_TYPE: u16 = 0x0001;
/// `RES_TABLE_TYPE` — the top-level `ResTable_header` chunk.
pub const RES_TABLE_TYPE: u16 = 0x0002;
/// `RES_TABLE_PACKAGE_TYPE` — a `ResTable_package` chunk.
pub const RES_TABLE_PACKAGE_TYPE: u16 = 0x0200;
/// `RES_TABLE_TYPE_TYPE` — a `ResTable_type` chunk (entries for one config).
pub const RES_TABLE_TYPE_TYPE: u16 = 0x0201;
/// `RES_TABLE_TYPE_SPEC_TYPE` — a `ResTable_typeSpec` chunk.
pub const RES_TABLE_TYPE_SPEC_TYPE: u16 = 0x0202;
/// `RES_TABLE_LIBRARY_TYPE` — a `ResTable_lib_header` chunk.
pub const RES_TABLE_LIBRARY_TYPE: u16 = 0x0203;
/// `RES_TABLE_OVERLAYABLE_TYPE` — a `ResTable_overlayable_header` chunk.
pub const RES_TABLE_OVERLAYABLE_TYPE: u16 = 0x0204;
/// `RES_TABLE_OVERLAYABLE_POLICY_TYPE` — a `ResTable_overlayable_policy_header` chunk.
pub const RES_TABLE_OVERLAYABLE_POLICY_TYPE: u16 = 0x0205;
/// `RES_TABLE_STAGED_ALIAS_TYPE` — a `ResTable_staged_alias_header` chunk.
pub const RES_TABLE_STAGED_ALIAS_TYPE: u16 = 0x0206;

// ---------------------------------------------------------------------------
// ResTable_typeSpec flags.
// ---------------------------------------------------------------------------

/// `ResTable_typeSpec::SPEC_PUBLIC` — the entry is public.
pub const SPEC_PUBLIC: u32 = 0x4000_0000;
/// `ResTable_typeSpec::SPEC_STAGED_API` — the entry's ID may change in a
/// future build (implies public).
pub const SPEC_STAGED_API: u32 = 0x2000_0000;

// ---------------------------------------------------------------------------
// ResTable_type flags and sentinels.
// ---------------------------------------------------------------------------

/// `ResTable_type::FLAG_SPARSE` — entries are `(idx, offset/4)` u16 pairs,
/// sorted by `idx` for binary search.
pub const FLAG_SPARSE: u8 = 0x01;
/// `ResTable_type::FLAG_OFFSET16` — entry offsets are u16s in 4-byte units;
/// `0xffff` means [`NO_ENTRY16`].
pub const FLAG_OFFSET16: u8 = 0x02;
/// `ResTable_type::NO_ENTRY` — a 32-bit offset slot with no entry.
pub const NO_ENTRY: u32 = 0xFFFF_FFFF;
/// 16-bit `NO_ENTRY` sentinel used with [`FLAG_OFFSET16`].
pub const NO_ENTRY16: u16 = 0xFFFF;

// ---------------------------------------------------------------------------
// ResTable_entry flags.
// ---------------------------------------------------------------------------

/// `ResTable_entry::FLAG_COMPLEX` — a map entry (`ResTable_map_entry`).
pub const ENTRY_FLAG_COMPLEX: u16 = 0x0001;
/// `ResTable_entry::FLAG_PUBLIC` — declared `<public>`.
pub const ENTRY_FLAG_PUBLIC: u16 = 0x0002;
/// `ResTable_entry::FLAG_WEAK` — may be overridden by strong definitions.
pub const ENTRY_FLAG_WEAK: u16 = 0x0004;
/// `ResTable_entry::FLAG_COMPACT` — compact encoding: 16-bit key, data type
/// in the high 8 bits of `flags`, data inline.
pub const ENTRY_FLAG_COMPACT: u16 = 0x0008;

// ---------------------------------------------------------------------------
// ResTable_map special keys (Res_MAKEINTERNAL).
// ---------------------------------------------------------------------------

/// `Res_MAKEINTERNAL(entry)` — internal attribute meta-data key.
pub const fn res_make_internal(entry: u16) -> u32 {
    0x0100_0000 | entry as u32
}

/// `ResTable_map::ATTR_TYPE` — the attribute's allowed-format bit mask.
pub const ATTR_TYPE: u32 = res_make_internal(0);
/// `ResTable_map::ATTR_MIN` — minimum integer value.
pub const ATTR_MIN: u32 = res_make_internal(1);
/// `ResTable_map::ATTR_MAX` — maximum integer value.
pub const ATTR_MAX: u32 = res_make_internal(2);
/// `ResTable_map::ATTR_L10N` — localization mode.
pub const ATTR_L10N: u32 = res_make_internal(3);
/// `ResTable_map::ATTR_OTHER` — plural arity keys.
pub const ATTR_OTHER: u32 = res_make_internal(4);
pub const ATTR_ZERO: u32 = res_make_internal(5);
pub const ATTR_ONE: u32 = res_make_internal(6);
pub const ATTR_TWO: u32 = res_make_internal(7);
pub const ATTR_FEW: u32 = res_make_internal(8);
pub const ATTR_MANY: u32 = res_make_internal(9);

/// `Res_INTERNALID(resid)` — true for the `ATTR_*` meta-data keys above.
pub const fn is_internal_id(resid: u32) -> bool {
    (resid & 0xFFFF_0000) != 0 && (resid & 0x00FF_0000) == 0
}

// ---------------------------------------------------------------------------
// Fixed header sizes (struct sizes from ResourceTypes.h).
// ---------------------------------------------------------------------------

/// `sizeof(ResChunk_header)`.
pub const CHUNK_HEADER_SIZE: usize = 8;
/// `sizeof(ResTable_header)`.
pub const RES_TABLE_HEADER_SIZE: u16 = 12;
/// `sizeof(ResTable_package)`: header + id + name\[128\] + 5 u32 fields.
pub const PACKAGE_HEADER_SIZE: u16 = 288;
/// `ResTable_package` minimum size (without the trailing `typeIdOffset`),
/// `kMinPackageSize` in BinaryResourceParser.cpp.
pub const PACKAGE_MIN_HEADER_SIZE: u16 = 284;
/// `sizeof(ResTable_typeSpec)`.
pub const TYPE_SPEC_HEADER_SIZE: u16 = 16;
/// `ResTable_type` fixed prefix before the embedded `ResTable_config`.
pub const TYPE_HEADER_PREFIX_SIZE: usize = 20;
/// `kResTableTypeMinSize`: the fixed prefix plus the config's own `size`
/// field — the minimum readable `ResTable_type` header.
pub const TYPE_HEADER_MIN_SIZE: usize = TYPE_HEADER_PREFIX_SIZE + 4;
/// `sizeof(ResTable_lib_header)`.
pub const LIB_HEADER_SIZE: u16 = 12;
/// `sizeof(ResTable_lib_entry)`: packageId + packageName\[128\].
pub const LIB_ENTRY_SIZE: usize = 260;
/// `sizeof(ResTable_overlayable_header)`: header + name\[256\] + actor\[256\].
pub const OVERLAYABLE_HEADER_SIZE: u16 = 1032;
/// `sizeof(ResTable_overlayable_policy_header)`.
pub const OVERLAYABLE_POLICY_HEADER_SIZE: u16 = 16;
/// `sizeof(ResTable_staged_alias_header)`.
pub const STAGED_ALIAS_HEADER_SIZE: u16 = 12;
/// `sizeof(ResTable_staged_alias_entry)`.
pub const STAGED_ALIAS_ENTRY_SIZE: usize = 8;

// ---------------------------------------------------------------------------
// Bounds-checked little-endian readers.
// ---------------------------------------------------------------------------

/// Reads a `u8` at `offset`, `None` when out of bounds.
pub fn read_u8(data: &[u8], offset: usize) -> Option<u8> {
    data.get(offset).copied()
}

/// Reads a little-endian `u16` at `offset`, `None` when out of bounds.
pub fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let slice = data.get(offset..offset.checked_add(2)?)?;
    Some(LittleEndian::read_u16(slice))
}

/// Reads a little-endian `u32` at `offset`, `None` when out of bounds.
pub fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let slice = data.get(offset..offset.checked_add(4)?)?;
    Some(LittleEndian::read_u32(slice))
}

/// Decodes a NUL-terminated fixed-length UTF-16LE field of `max_units`
/// units starting at `offset` (e.g. `ResTable_package::name`). Port of
/// `strcpy16_dtoh` + `Utf16ToUtf8`. Returns an empty string when out of
/// bounds.
pub fn read_utf16_fixed(data: &[u8], offset: usize, max_units: usize) -> String {
    let mut units = Vec::new();
    for i in 0..max_units {
        match read_u16(data, offset + i * 2) {
            Some(0) | None => break,
            Some(u) => units.push(u),
        }
    }
    String::from_utf16_lossy(&units)
}

/// Encodes `s` as UTF-16LE into a fixed field of `max_units` units at
/// absolute offset `offset` (the buffer must already contain zeroed space).
/// Truncates to `max_units - 1` and leaves at least one NUL, port of
/// `strcpy16_htod`.
pub fn write_utf16_fixed(buf: &mut [u8], offset: usize, max_units: usize, s: &str) {
    if max_units == 0 {
        return;
    }
    for (i, unit) in s.encode_utf16().take(max_units - 1).enumerate() {
        let at = offset + i * 2;
        if at + 2 > buf.len() {
            return;
        }
        buf[at..at + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

/// Patches a little-endian `u16` at `offset` (write side only; offsets are
/// produced by [`ChunkWriter`] and always in bounds).
pub fn patch_u16(buf: &mut [u8], offset: usize, value: u16) {
    if offset + 2 <= buf.len() {
        buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
}

/// Patches a little-endian `u32` at `offset`.
pub fn patch_u32(buf: &mut [u8], offset: usize, value: u32) {
    if offset + 4 <= buf.len() {
        buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// Appends a little-endian `u16`.
pub fn push_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Appends a little-endian `u32`.
pub fn push_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Pads `buf` with zeros to a 4-byte boundary (`BigBuffer::Align4`).
pub fn align4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

// ---------------------------------------------------------------------------
// Read side: chunk header + iterator (port of ResChunkPullParser).
// ---------------------------------------------------------------------------

/// A validated view of one chunk: its `ResChunk_header` fields plus the
/// full chunk bytes (header included).
#[derive(Debug, Clone, Copy)]
pub struct Chunk<'a> {
    /// `ResChunk_header::type`.
    pub type_id: u16,
    /// `ResChunk_header::headerSize`.
    pub header_size: u16,
    /// The whole chunk: `data.len() == ResChunk_header::size`.
    pub data: &'a [u8],
}

impl<'a> Chunk<'a> {
    /// The chunk data following the header (`GetChunkData` /
    /// `GetChunkDataLen` in ResChunkPullParser.h). Empty when the declared
    /// header size exceeds the chunk size.
    pub fn payload(&self) -> &'a [u8] {
        self.data.get(self.header_size as usize..).unwrap_or(&[])
    }

    /// Iterates the child chunks contained in this chunk's payload.
    pub fn children(&self) -> ChunkIterator<'a> {
        ChunkIterator::new(self.payload())
    }
}

/// Iterates over consecutive chunks in `data`, validating each header the
/// way `ResChunkPullParser::Next` does: at least 8 bytes of header,
/// `headerSize >= 8`, `size >= headerSize`, `size` within the remaining
/// data. Stops (recording [`ChunkIterator::error`]) on malformed input —
/// it never panics, since the input is untrusted APK content.
#[derive(Debug)]
pub struct ChunkIterator<'a> {
    data: &'a [u8],
    pos: usize,
    /// Set when iteration stopped because of a malformed chunk header
    /// (`Event::kBadDocument` in the C++ parser).
    pub error: Option<&'static str>,
}

impl<'a> ChunkIterator<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        ChunkIterator {
            data,
            pos: 0,
            error: None,
        }
    }
}

impl<'a> Iterator for ChunkIterator<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Chunk<'a>> {
        if self.error.is_some() || self.pos >= self.data.len() {
            return None;
        }
        let remaining = &self.data[self.pos..];
        if remaining.len() < CHUNK_HEADER_SIZE {
            self.error = Some("not enough data for chunk header");
            return None;
        }
        let type_id = read_u16(remaining, 0)?;
        let header_size = read_u16(remaining, 2)?;
        let size = read_u32(remaining, 4)? as usize;
        if (header_size as usize) < CHUNK_HEADER_SIZE {
            self.error = Some("chunk header size too small");
            return None;
        }
        if size < header_size as usize {
            self.error = Some("chunk size smaller than its header size");
            return None;
        }
        if size > remaining.len() {
            self.error = Some("chunk size larger than the available data");
            return None;
        }
        self.pos += size;
        Some(Chunk {
            type_id,
            header_size,
            data: &remaining[..size],
        })
    }
}

// ---------------------------------------------------------------------------
// Write side: ChunkWriter (port of format/binary/ChunkWriter.h).
// ---------------------------------------------------------------------------

/// Writes a chunk into a `Vec<u8>`: reserves a zeroed header of
/// `header_size` bytes up front (the C++ `StartChunk<T>` allocates a
/// zeroed `T`), lets the caller patch header fields and append data, then
/// [`ChunkWriter::finish`] aligns to 4 bytes and back-patches the total
/// chunk size.
#[derive(Debug)]
pub struct ChunkWriter {
    start: usize,
}

impl ChunkWriter {
    /// Starts a chunk of `type_id` whose header occupies `header_size`
    /// bytes (all zeroed except `type` and `headerSize`).
    pub fn start(buf: &mut Vec<u8>, type_id: u16, header_size: u16) -> ChunkWriter {
        let start = buf.len();
        buf.resize(start + header_size as usize, 0);
        patch_u16(buf, start, type_id);
        patch_u16(buf, start + 2, header_size);
        ChunkWriter { start }
    }

    /// Absolute offset of the chunk header in the buffer. Header fields
    /// live at `start_offset() + field_offset`.
    pub fn start_offset(&self) -> usize {
        self.start
    }

    /// Bytes written so far for this chunk (`ChunkWriter::size`).
    pub fn size(&self, buf: &[u8]) -> usize {
        buf.len() - self.start
    }

    /// Aligns the buffer to 4 bytes and patches the chunk's total size
    /// (`ChunkWriter::Finish`).
    pub fn finish(self, buf: &mut Vec<u8>) {
        align4(buf);
        let total = (buf.len() - self.start) as u32;
        patch_u32(buf, self.start + 4, total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_writer_round_trips_through_iterator() {
        let mut buf = Vec::new();
        let outer = ChunkWriter::start(&mut buf, RES_TABLE_TYPE, RES_TABLE_HEADER_SIZE);
        patch_u32(&mut buf, outer.start_offset() + 8, 1); // packageCount

        let inner = ChunkWriter::start(&mut buf, RES_TABLE_LIBRARY_TYPE, LIB_HEADER_SIZE);
        push_u32(&mut buf, 0xdead_beef);
        push_u16(&mut buf, 0xbeef); // forces padding
        inner.finish(&mut buf);
        outer.finish(&mut buf);

        assert_eq!(buf.len() % 4, 0);

        let mut iter = ChunkIterator::new(&buf);
        let table = iter.next().expect("outer chunk");
        assert_eq!(table.type_id, RES_TABLE_TYPE);
        assert_eq!(table.header_size, RES_TABLE_HEADER_SIZE);
        assert_eq!(table.data.len(), buf.len());
        assert!(iter.next().is_none());
        assert!(iter.error.is_none());

        let mut children = table.children();
        let lib = children.next().expect("inner chunk");
        assert_eq!(lib.type_id, RES_TABLE_LIBRARY_TYPE);
        assert_eq!(read_u32(lib.payload(), 0), Some(0xdead_beef));
        assert!(children.next().is_none());
    }

    #[test]
    fn iterator_rejects_malformed_chunks() {
        // Truncated header.
        let mut iter = ChunkIterator::new(&[0u8; 4]);
        assert!(iter.next().is_none());
        assert!(iter.error.is_some());

        // size < headerSize.
        let mut bad = Vec::new();
        push_u16(&mut bad, RES_TABLE_TYPE);
        push_u16(&mut bad, 12);
        push_u32(&mut bad, 8);
        bad.resize(12, 0);
        let mut iter = ChunkIterator::new(&bad);
        assert!(iter.next().is_none());
        assert!(iter.error.is_some());

        // size beyond available data.
        let mut bad = Vec::new();
        push_u16(&mut bad, RES_TABLE_TYPE);
        push_u16(&mut bad, 8);
        push_u32(&mut bad, 64);
        let mut iter = ChunkIterator::new(&bad);
        assert!(iter.next().is_none());
        assert!(iter.error.is_some());
    }

    #[test]
    fn utf16_fixed_round_trip() {
        let mut buf = vec![0u8; 32];
        write_utf16_fixed(&mut buf, 0, 16, "android");
        assert_eq!(read_utf16_fixed(&buf, 0, 16), "android");

        // Truncation keeps the trailing NUL.
        let mut buf = vec![0u8; 8];
        write_utf16_fixed(&mut buf, 0, 4, "abcdef");
        assert_eq!(read_utf16_fixed(&buf, 0, 4), "abc");
    }

    #[test]
    fn internal_id_predicate() {
        assert!(is_internal_id(ATTR_TYPE));
        assert!(is_internal_id(ATTR_MANY));
        assert!(!is_internal_id(0x7f01_0000));
        assert!(!is_internal_id(0x0101_0001));
        assert_eq!(ATTR_TYPE, 0x0100_0000);
        assert_eq!(ATTR_OTHER, 0x0100_0004);
        assert_eq!(ATTR_ZERO, 0x0100_0005);
    }
}
