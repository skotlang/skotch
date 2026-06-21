//! Port of Android's resource string pool: the aapt2 build-side pool
//! (`frameworks/base/libs/androidfw/StringPool.cpp` + `StringPool.h`) and a
//! validated parser for the flattened `ResStringPool` chunk format
//! (`ResStringPool::setTo` / `stringAt` / `string8At` / `styleAt` in
//! `frameworks/base/libs/androidfw/ResourceTypes.cpp`, structs in
//! `include/androidfw/ResourceTypes.h`).
//!
//! Self-contained: only `std` + `byteorder`. No `unsafe`, and the parse paths
//! never panic on malformed input.
//!
//! # Binary layout (see `ResStringPool_header` in ResourceTypes.h)
//!
//! ```text
//! ResChunk_header   { type: u16 = 0x0001, headerSize: u16 = 28, size: u32 }
//! stringCount: u32  -- number of string offsets (styles + plain strings)
//! styleCount:  u32  -- number of style offsets
//! flags:       u32  -- SORTED_FLAG = 1<<0, UTF8_FLAG = 1<<8
//! stringsStart:u32  -- offset from chunk start to string data
//! stylesStart: u32  -- offset from chunk start to style data (0 if none)
//! [stringCount x u32]  string offsets, relative to stringsStart (bytes)
//! [styleCount  x u32]  style offsets, relative to stylesStart (bytes)
//! ...string data...    (4-byte aligned at the end)
//! ...style data...     spans terminated by END, block terminated by an
//!                      entire ResStringPool_span worth of 0xFFFFFFFF
//! ```
//!
//! Styled strings *always* come first in the pool: the runtime ResStringPool
//! indexes the style array by the same indices as the string array, so style
//! data would otherwise need to be sparse.

use byteorder::{ByteOrder, LittleEndian};
use std::cmp::Ordering;
use std::collections::HashMap;

/// `RES_STRING_POOL_TYPE` from ResourceTypes.h.
pub const RES_STRING_POOL_TYPE: u16 = 0x0001;
/// Size of `ResStringPool_header` (8-byte `ResChunk_header` + 5 u32 fields).
pub const STRING_POOL_HEADER_SIZE: usize = 28;
/// `ResStringPool_header::SORTED_FLAG`.
pub const SORTED_FLAG: u32 = 1 << 0;
/// `ResStringPool_header::UTF8_FLAG`.
pub const UTF8_FLAG: u32 = 1 << 8;
/// `ResStringPool_span::END` — terminates a span array, and (x3) the style block.
pub const SPAN_END: u32 = 0xFFFF_FFFF;

/// Maximum length encodable with two `u8` length units (`EncodeLengthMax<char>`).
const ENCODE_LENGTH_MAX_U8: usize = 0x7FFF;
/// Maximum length encodable with two `u16` length units (`EncodeLengthMax<char16_t>`).
const ENCODE_LENGTH_MAX_U16: usize = 0x7FFF_FFFF;

/// What aapt2 writes in place of a string that cannot be length-encoded
/// (`kStringTooLarge` in StringPool.cpp).
pub const STRING_TOO_LARGE: &str = "STRING_TOO_LARGE";

// ---------------------------------------------------------------------------
// Write side: StringPool builder + flattener
// ---------------------------------------------------------------------------

/// Sorting context for a pool entry, port of `StringPool::Context`.
///
/// The C++ Context holds a `ConfigDescription`; to keep this module
/// self-contained we store an opaque `config_sort_key` instead. Callers pass
/// the config's binary (or any other stable) representation, or an empty
/// vector for the default config. [`StringPool::sort`] orders entries by
/// `priority` ascending, then `config_sort_key`, then string value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub priority: u32,
    pub config_sort_key: Vec<u8>,
}

impl Context {
    /// `Context::kHighPriority`.
    pub const HIGH_PRIORITY: u32 = 1;
    /// `Context::kNormalPriority`.
    pub const NORMAL_PRIORITY: u32 = 0x7fff_ffff;
    /// `Context::kLowPriority`.
    pub const LOW_PRIORITY: u32 = 0xffff_ffff;

    pub fn new(priority: u32, config_sort_key: Vec<u8>) -> Self {
        Context {
            priority,
            config_sort_key,
        }
    }

    pub fn with_priority(priority: u32) -> Self {
        Context {
            priority,
            config_sort_key: Vec::new(),
        }
    }
}

impl Default for Context {
    fn default() -> Self {
        Context {
            priority: Context::NORMAL_PRIORITY,
            config_sort_key: Vec::new(),
        }
    }
}

/// Stable handle to a plain string in a [`StringPool`], port of
/// `StringPool::Ref`. Handles stay valid across [`StringPool::sort`];
/// [`StringPool::resolve`] maps a handle to its final flattened index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ref(usize);

impl Ref {
    /// Re-addresses a `Ref` issued by a pool that was [`StringPool::merge`]d
    /// into another pool, given the returned id offset.
    pub fn offset_by(self, id_offset: usize) -> Ref {
        Ref(self.0 + id_offset)
    }
}

/// Stable handle to a styled string in a [`StringPool`], port of
/// `StringPool::StyleRef`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StyleRef(usize);

/// A span attached to a styled string, port of `StringPool::Span`.
/// `name` is itself a reference to a plain string in the same pool.
#[derive(Clone, Debug)]
pub struct SpanEntry {
    pub name: Ref,
    pub first_char: u32,
    pub last_char: u32,
}

#[derive(Debug)]
struct Entry {
    id: usize,
    value: String,
    context: Context,
}

#[derive(Debug)]
struct StyleEntry {
    id: usize,
    value: String,
    context: Context,
    spans: Vec<SpanEntry>,
}

/// Build-side string pool, port of aapt2's `StringPool`.
///
/// Plain strings and styled strings are stored separately; styled strings are
/// always flattened first (see module docs). String handles ([`Ref`] /
/// [`StyleRef`]) remain valid across [`StringPool::sort`].
#[derive(Debug, Default)]
pub struct StringPool {
    strings: Vec<Entry>,
    styles: Vec<StyleEntry>,
    /// `Ref(id)` -> current position in `strings`.
    string_pos: Vec<usize>,
    /// `StyleRef(id)` -> current position in `styles`.
    style_pos: Vec<usize>,
    /// value -> ids of entries holding that value (multimap, like
    /// `indexed_strings_`).
    indexed: HashMap<String, Vec<usize>>,
}

impl StringPool {
    pub fn new() -> Self {
        StringPool::default()
    }

    /// Total number of pool entries (styles + plain strings), port of
    /// `StringPool::size()`. This is the value written as `stringCount`.
    pub fn len(&self) -> usize {
        self.styles.len() + self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty() && self.strings.is_empty()
    }

    /// Number of styled entries (written as `styleCount`).
    pub fn style_count(&self) -> usize {
        self.styles.len()
    }

    /// Adds a plain string with the default context.
    pub fn make_ref_plain(&mut self, s: &str) -> Ref {
        self.make_ref(s, Context::default())
    }

    /// Adds a plain string, deduplicating against existing entries. Port of
    /// `StringPool::MakeRefImpl(str, context, unique=true)`:
    ///
    /// * if an entry with the same value and the same `priority` exists, it is
    ///   reused as-is (exact C++ behavior);
    /// * additionally (per the port spec), if an entry with the same value and
    ///   the same `config_sort_key` exists, the entries are merged and the
    ///   merged entry takes the *minimum* of the two priorities.
    pub fn make_ref(&mut self, s: &str, context: Context) -> Ref {
        let ids: Vec<usize> = self.indexed.get(s).cloned().unwrap_or_default();
        for &id in &ids {
            let pos = self.string_pos[id];
            if self.strings[pos].context.priority == context.priority {
                return Ref(id);
            }
        }
        for &id in &ids {
            let pos = self.string_pos[id];
            if self.strings[pos].context.config_sort_key == context.config_sort_key {
                let entry = &mut self.strings[pos];
                entry.context.priority = entry.context.priority.min(context.priority);
                return Ref(id);
            }
        }

        let id = self.string_pos.len();
        self.string_pos.push(self.strings.len());
        self.strings.push(Entry {
            id,
            value: s.to_string(),
            context,
        });
        self.indexed.entry(s.to_string()).or_default().push(id);
        Ref(id)
    }

    /// Adds a styled string, port of `StringPool::MakeRef(StyleString, Context)`.
    /// Styled strings are never deduplicated (matching C++). Each span name is
    /// interned as a plain string in this pool with the default context.
    pub fn make_style_ref(
        &mut self,
        s: &str,
        spans: Vec<(String, u32, u32)>,
        context: Context,
    ) -> StyleRef {
        let spans = spans
            .into_iter()
            .map(|(name, first_char, last_char)| SpanEntry {
                name: self.make_ref(&name, Context::default()),
                first_char,
                last_char,
            })
            .collect();

        let id = self.style_pos.len();
        self.style_pos.push(self.styles.len());
        self.styles.push(StyleEntry {
            id,
            value: s.to_string(),
            context,
            spans,
        });
        StyleRef(id)
    }

    /// Sorts the pool, port of `StringPool::Sort` with the comparator aapt2's
    /// table flattener uses: priority ascending, then config sort key, then
    /// string value (lexicographically).
    ///
    /// Styled and plain entries are sorted independently — the "styles come
    /// first" invariant is structural (separate vectors), exactly as in the
    /// C++ class. Handles are re-indexed afterwards (`ReAssignIndices`).
    pub fn sort(&mut self) {
        fn context_cmp(a: &Context, b: &Context) -> Ordering {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.config_sort_key.cmp(&b.config_sort_key))
        }
        self.styles.sort_by(|a, b| {
            context_cmp(&a.context, &b.context).then_with(|| a.value.cmp(&b.value))
        });
        self.strings.sort_by(|a, b| {
            context_cmp(&a.context, &b.context).then_with(|| a.value.cmp(&b.value))
        });
        self.reassign_indices();
    }

    fn reassign_indices(&mut self) {
        for (pos, entry) in self.strings.iter().enumerate() {
            self.string_pos[entry.id] = pos;
        }
        for (pos, entry) in self.styles.iter().enumerate() {
            self.style_pos[entry.id] = pos;
        }
    }

    /// Appends every entry of `other` WITHOUT deduplication — port of
    /// `StringPool::Merge`, which the XML flattener relies on to keep
    /// same-named attributes from different packages as distinct entries
    /// (their contexts carry different resource IDs).
    ///
    /// Returns the id offset: a `Ref(id)` issued by `other` addresses the
    /// merged entry via `Ref::offset_by(id_offset)` on `self`.
    pub fn merge(&mut self, other: StringPool) -> usize {
        let id_offset = self.string_pos.len();
        let style_id_offset = self.style_pos.len();

        // Plain strings, in `other`'s current position order.
        self.string_pos
            .resize(id_offset + other.string_pos.len(), usize::MAX);
        for entry in other.strings {
            let new_id = entry.id + id_offset;
            self.string_pos[new_id] = self.strings.len();
            self.indexed
                .entry(entry.value.clone())
                .or_default()
                .push(new_id);
            self.strings.push(Entry {
                id: new_id,
                ..entry
            });
        }

        // Styled strings: span names reference plain entries of `other`.
        self.style_pos
            .resize(style_id_offset + other.style_pos.len(), usize::MAX);
        for mut entry in other.styles {
            for span in &mut entry.spans {
                span.name = span.name.offset_by(id_offset);
            }
            let new_id = entry.id + style_id_offset;
            self.style_pos[new_id] = self.styles.len();
            self.styles.push(StyleEntry {
                id: new_id,
                ..entry
            });
        }

        id_offset
    }

    /// Context priorities of the plain strings in flattened (sorted)
    /// order. The XML flattener reads these to build the resource map.
    pub fn priorities(&self) -> impl Iterator<Item = u32> + '_ {
        self.strings.iter().map(|e| e.context.priority)
    }

    /// Final flattened index of a plain string, port of `Ref::index()`:
    /// styles always come first, so the index is offset by the style count.
    pub fn resolve(&self, r: Ref) -> usize {
        self.styles.len() + self.string_pos[r.0]
    }

    /// Final flattened index of a styled string, port of `StyleRef::index()`.
    pub fn resolve_style(&self, r: StyleRef) -> usize {
        self.style_pos[r.0]
    }

    /// Flattens to a full `ResStringPool` chunk in UTF-8 mode, port of
    /// `StringPool::FlattenUtf8`.
    ///
    /// Strings whose Modified-UTF-8 byte length or UTF-16 unit length exceeds
    /// 0x7FFF are replaced with [`STRING_TOO_LARGE`], exactly as aapt2's
    /// `EncodeString` does (it logs an error and substitutes).
    pub fn flatten_utf8(&self) -> Vec<u8> {
        self.flatten(true)
    }

    /// Flattens to a full `ResStringPool` chunk in UTF-16 mode, port of
    /// `StringPool::FlattenUtf16`.
    pub fn flatten_utf16(&self) -> Vec<u8> {
        self.flatten(false)
    }

    /// Port of `StringPool::Flatten(BigBuffer*, pool, utf8, diag)`.
    fn flatten(&self, utf8: bool) -> Vec<u8> {
        let string_count = self.len();
        let style_count = self.styles.len();

        let mut out: Vec<u8> = Vec::new();
        // ResStringPool_header.
        out.extend_from_slice(&RES_STRING_POOL_TYPE.to_le_bytes());
        out.extend_from_slice(&(STRING_POOL_HEADER_SIZE as u16).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]); // header.size — patched below
        out.extend_from_slice(&(string_count as u32).to_le_bytes());
        out.extend_from_slice(&(style_count as u32).to_le_bytes());
        out.extend_from_slice(&(if utf8 { UTF8_FLAG } else { 0 }).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]); // stringsStart — patched below
        out.extend_from_slice(&[0u8; 4]); // stylesStart — stays 0 without styles

        // Index arrays: string offsets (styles + strings), then style offsets.
        let indices_off = out.len();
        out.resize(out.len() + 4 * string_count, 0);
        let style_indices_off = out.len();
        out.resize(out.len() + 4 * style_count, 0);

        let before_strings = out.len();
        patch_u32(&mut out, 20, before_strings as u32); // stringsStart

        // Styles always come first.
        let mut slot = indices_off;
        let values = self
            .styles
            .iter()
            .map(|e| e.value.as_str())
            .chain(self.strings.iter().map(|e| e.value.as_str()));
        for value in values {
            let offset = (out.len() - before_strings) as u32;
            patch_u32(&mut out, slot, offset);
            slot += 4;
            encode_string(value, utf8, &mut out);
        }

        align4(&mut out);

        if style_count > 0 {
            let before_styles = out.len();
            patch_u32(&mut out, 24, before_styles as u32); // stylesStart

            let mut style_slot = style_indices_off;
            for entry in &self.styles {
                let offset = (out.len() - before_styles) as u32;
                patch_u32(&mut out, style_slot, offset);
                style_slot += 4;

                for span in &entry.spans {
                    out.extend_from_slice(&(self.resolve(span.name) as u32).to_le_bytes());
                    out.extend_from_slice(&span.first_char.to_le_bytes());
                    out.extend_from_slice(&span.last_char.to_le_bytes());
                }
                // Each span array is terminated by END.
                out.extend_from_slice(&SPAN_END.to_le_bytes());
            }

            // The platform's error checking looks for an entire
            // ResStringPool_span (3 words) of 0xFFFFFFFF at the end of the
            // style block, so write the remaining 2 words of 0xFFFFFFFF.
            out.extend_from_slice(&[0xFF; 8]);
            align4(&mut out);
        }

        let total = out.len() as u32;
        patch_u32(&mut out, 4, total); // header.size
        out
    }
}

/// Number of UTF-16 code units needed to encode `s` (surrogate pairs count as
/// two units). Mirrors `utf8_to_utf16_length` over the Modified-UTF-8 form.
fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// Port of `util::Utf8ToModifiedUtf8`: Java's Modified UTF-8 only supports the
/// 1/2/3-byte UTF-8 forms; supplementary code points (4-byte UTF-8) are
/// re-encoded as a CESU-8 surrogate pair (two 3-byte sequences).
fn utf8_to_modified_utf8(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut buf = [0u8; 4];
    for c in s.chars() {
        if c.len_utf8() == 4 {
            let mut units = [0u16; 2];
            c.encode_utf16(&mut units);
            for &u in &units {
                out.push(0xE0 | (((u >> 12) & 0x0F) as u8));
                out.push(0x80 | (((u >> 6) & 0x3F) as u8));
                out.push(0x80 | ((u & 0x3F) as u8));
            }
        } else {
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}

/// Port of `EncodeLength<char>`: lengths > 0x7F take two bytes — high 7 bits
/// (with the continuation bit 0x80 set) then the low 8 bits.
fn encode_length_u8(out: &mut Vec<u8>, len: usize) {
    if len > 0x7F {
        out.push(0x80 | (((len >> 8) & 0x7F) as u8));
    }
    out.push((len & 0xFF) as u8);
}

/// Port of `EncodeLength<char16_t>`: lengths > 0x7FFF take two u16 units —
/// high 15 bits (with the continuation bit 0x8000 set) then the low 16 bits.
fn encode_length_u16(out: &mut Vec<u8>, len: usize) {
    if len > 0x7FFF {
        let hi = 0x8000u16 | (((len >> 16) & 0x7FFF) as u16);
        out.extend_from_slice(&hi.to_le_bytes());
    }
    out.extend_from_slice(&((len & 0xFFFF) as u16).to_le_bytes());
}

/// Port of `EncodeString` in StringPool.cpp. Returns `false` when the string
/// was too large to length-encode and [`STRING_TOO_LARGE`] was substituted.
fn encode_string(s: &str, utf8: bool, out: &mut Vec<u8>) -> bool {
    if utf8 {
        let encoded = utf8_to_modified_utf8(s);
        let u16_length = utf16_len(s);
        if encoded.len() > ENCODE_LENGTH_MAX_U8 || u16_length > ENCODE_LENGTH_MAX_U8 {
            encode_string(STRING_TOO_LARGE, utf8, out);
            return false;
        }
        // First the UTF-16 length, then the byte length of the real UTF-8
        // (Modified-UTF-8) data, then the data and a NUL byte.
        encode_length_u8(out, u16_length);
        encode_length_u8(out, encoded.len());
        out.extend_from_slice(&encoded);
        out.push(0);
    } else {
        let units: Vec<u16> = s.encode_utf16().collect();
        if units.len() > ENCODE_LENGTH_MAX_U16 {
            encode_string(STRING_TOO_LARGE, utf8, out);
            return false;
        }
        encode_length_u16(out, units.len());
        for u in &units {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes()); // NUL terminator
    }
    true
}

fn patch_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn align4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

// ---------------------------------------------------------------------------
// Read side: validated ResStringPool chunk parser
// ---------------------------------------------------------------------------

/// Read-side view of a flattened `ResStringPool` chunk, a (simplified, owned)
/// port of `ResStringPool::setTo` plus `stringAt`/`string8At`/`styleAt`.
/// All accessors are bounds-checked; malformed data yields `None`/empty
/// rather than a panic, since this parses untrusted APK content.
#[derive(Debug)]
pub struct BinaryStringPool {
    data: Vec<u8>,
    string_count: usize,
    style_count: usize,
    flags: u32,
    strings_start: usize,
    styles_start: usize,
    /// Offset of the string-offsets array (== headerSize).
    entries_off: usize,
    /// Offset of the style-offsets array.
    entry_styles_off: usize,
    /// Size of the string data region, in char units (bytes for UTF-8,
    /// u16 units for UTF-16). Mirrors `mStringPoolSize`.
    string_pool_size: usize,
    /// Size of the style data region in u32 units. Mirrors `mStylePoolSize`.
    style_pool_size: usize,
}

impl BinaryStringPool {
    /// Parses a full string-pool chunk (starting at its `ResChunk_header`).
    /// Validation mirrors `ResStringPool::setTo`.
    pub fn parse(chunk: &[u8]) -> Option<BinaryStringPool> {
        if chunk.len() < STRING_POOL_HEADER_SIZE {
            return None;
        }
        if read_u16(chunk, 0)? != RES_STRING_POOL_TYPE {
            return None;
        }
        let header_size = read_u16(chunk, 2)? as usize;
        let size = read_u32(chunk, 4)? as usize;
        // validate_chunk: headerSize >= minSize, headerSize <= size, both
        // 4-byte aligned, size within the available data.
        if header_size < STRING_POOL_HEADER_SIZE || header_size > size || size > chunk.len() {
            return None;
        }
        if (header_size | size) & 0x3 != 0 {
            return None;
        }

        let string_count = read_u32(chunk, 8)? as usize;
        let style_count = read_u32(chunk, 12)? as usize;
        let flags = read_u32(chunk, 16)?;
        let strings_start = read_u32(chunk, 20)? as usize;
        let styles_start = read_u32(chunk, 24)? as usize;

        let is_utf8 = (flags & UTF8_FLAG) != 0;
        let char_size: usize = if is_utf8 { 1 } else { 2 };

        let entries_off = header_size;
        let entries_bytes = string_count.checked_mul(4)?;
        if entries_off.checked_add(entries_bytes)? > size {
            return None;
        }
        let entry_styles_off = entries_off + entries_bytes;

        let mut string_pool_size = 0usize;
        if string_count > 0 {
            // There should be at least space for the smallest string
            // (2 bytes length, NUL terminator).
            if strings_start >= size.saturating_sub(2) {
                return None;
            }
            string_pool_size = if style_count == 0 {
                (size - strings_start) / char_size
            } else {
                // Styles must start before the end of data and after strings.
                if styles_start >= size.saturating_sub(2) {
                    return None;
                }
                if styles_start <= strings_start {
                    return None;
                }
                (styles_start - strings_start) / char_size
            };
            if string_pool_size == 0 {
                return None;
            }
            // The last unit of string data must be a NUL terminator.
            let last_off = strings_start + (string_pool_size - 1) * char_size;
            if is_utf8 {
                if *chunk.get(last_off)? != 0 {
                    return None;
                }
            } else if read_u16(chunk, last_off)? != 0 {
                return None;
            }
        }

        let mut style_pool_size = 0usize;
        if style_count > 0 {
            let style_entries_bytes = style_count.checked_mul(4)?;
            if entry_styles_off.checked_add(style_entries_bytes)? > size {
                return None;
            }
            if styles_start >= size {
                return None;
            }
            style_pool_size = (size - styles_start) / 4;
            // The style block must end with an entire ResStringPool_span
            // (3 u32 words) of 0xFFFFFFFF.
            if style_pool_size < 3 {
                return None;
            }
            let end_off = styles_start + (style_pool_size - 3) * 4;
            for k in 0..3 {
                if read_u32(chunk, end_off + k * 4)? != SPAN_END {
                    return None;
                }
            }
        }

        Some(BinaryStringPool {
            data: chunk[..size].to_vec(),
            string_count,
            style_count,
            flags,
            strings_start,
            styles_start,
            entries_off,
            entry_styles_off,
            string_pool_size,
            style_pool_size,
        })
    }

    /// Number of strings in the pool (`stringCount`, includes styled strings).
    pub fn len(&self) -> usize {
        self.string_count
    }

    pub fn is_empty(&self) -> bool {
        self.string_count == 0
    }

    /// Number of style span arrays (`styleCount`).
    pub fn style_count(&self) -> usize {
        self.style_count
    }

    pub fn is_utf8(&self) -> bool {
        (self.flags & UTF8_FLAG) != 0
    }

    pub fn is_sorted(&self) -> bool {
        (self.flags & SORTED_FLAG) != 0
    }

    /// Decodes the string at `idx`, port of `stringAt` (UTF-16 pools) /
    /// `string8At` + `stringDecodeAt` (UTF-8 pools). Returns `None` for
    /// out-of-range indices or malformed entries.
    pub fn get(&self, idx: usize) -> Option<String> {
        if idx >= self.string_count {
            return None;
        }
        let off = read_u32(&self.data, self.entries_off + idx * 4)? as usize;
        if self.is_utf8() {
            self.get_utf8(off)
        } else {
            self.get_utf16(off)
        }
    }

    fn get_utf8(&self, off: usize) -> Option<String> {
        // `off < mStringPoolSize - 1` (offsets are bytes in UTF-8 mode).
        if off.checked_add(1)? >= self.string_pool_size {
            return None;
        }
        let base = self.strings_start;
        let pool_end = base + self.string_pool_size; // absolute, in bytes
        let mut pos = base + off;

        // UTF-16 length (unused for decoding), then the UTF-8 byte length.
        let (_u16_len, consumed) = decode_u8_length(&self.data, pos, pool_end)?;
        pos += consumed;
        let (u8_len, consumed) = decode_u8_length(&self.data, pos, pool_end)?;
        pos += consumed;

        // Port of `stringDecodeAt`: aapt(1) used to write a truncated length
        // for strings longer than 0x7FFF bytes, so scan forward through the
        // possible un-truncated lengths until the NUL terminator is found.
        let mut end = u8_len;
        let mut i = 0usize;
        let actual_len = loop {
            let abs = pos.checked_add(end)?;
            if abs >= pool_end {
                // Not NUL-terminated within the pool: malformed.
                return None;
            }
            if *self.data.get(abs)? == 0 {
                break end;
            }
            i += 1;
            end = (i << 15) | u8_len;
        };

        let bytes = self.data.get(pos..pos + actual_len)?;
        Some(decode_modified_utf8(bytes))
    }

    fn get_utf16(&self, off: usize) -> Option<String> {
        // Offsets are bytes; convert to u16 units like `off / sizeof(uint16_t)`.
        let unit_off = off / 2;
        if unit_off.checked_add(1)? >= self.string_pool_size {
            return None;
        }
        let base = self.strings_start;
        let pool_end = base + self.string_pool_size * 2; // absolute, in bytes
        let mut pos = base + unit_off * 2;

        let (len, consumed) = decode_u16_length(&self.data, pos, pool_end)?;
        pos += consumed;

        // The NUL terminator at `str + len` must lie within the pool and be 0.
        let end_unit = (pos - base) / 2 + len;
        if end_unit >= self.string_pool_size {
            return None;
        }
        let nul_off = base + end_unit * 2;
        if read_u16(&self.data, nul_off)? != 0 {
            return None;
        }

        let mut units = Vec::with_capacity(len);
        for k in 0..len {
            units.push(read_u16(&self.data, pos + k * 2)?);
        }
        Some(String::from_utf16_lossy(&units))
    }

    /// Returns the spans of the styled string at `idx` as
    /// `(name_index, first_char, last_char)` triples, port of `styleAt` plus
    /// the END-terminated iteration its callers perform. Empty for unstyled
    /// indices or malformed data.
    pub fn spans(&self, idx: usize) -> Vec<(u32, u32, u32)> {
        let mut result = Vec::new();
        if idx >= self.style_count {
            return result;
        }
        let Some(off) = read_u32(&self.data, self.entry_styles_off + idx * 4) else {
            return result;
        };
        let unit_off = off as usize / 4;
        if unit_off >= self.style_pool_size {
            return result;
        }
        let mut pos = self.styles_start + unit_off * 4;
        let end = self.styles_start + self.style_pool_size * 4;
        while pos + 12 <= end {
            let Some(name) = read_u32(&self.data, pos) else {
                break;
            };
            if name == SPAN_END {
                break;
            }
            let Some(first) = read_u32(&self.data, pos + 4) else {
                break;
            };
            let Some(last) = read_u32(&self.data, pos + 8) else {
                break;
            };
            result.push((name, first, last));
            pos += 12;
        }
        result
    }
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let slice = data.get(offset..offset.checked_add(2)?)?;
    Some(LittleEndian::read_u16(slice))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let slice = data.get(offset..offset.checked_add(4)?)?;
    Some(LittleEndian::read_u32(slice))
}

/// Port of `decodeLength` for UTF-8 pools: a `u8` length, with a 2-byte form
/// when the high bit of the first byte is set. Returns `(length, bytes_used)`.
fn decode_u8_length(data: &[u8], pos: usize, end: usize) -> Option<(usize, usize)> {
    if pos >= end {
        return None;
    }
    let b0 = *data.get(pos)? as usize;
    if b0 & 0x80 != 0 {
        if pos + 1 >= end {
            return None;
        }
        let b1 = *data.get(pos + 1)? as usize;
        Some((((b0 & 0x7F) << 8) | b1, 2))
    } else {
        Some((b0, 1))
    }
}

/// Port of `decodeLength` for UTF-16 pools: a `u16` length, with a 2-unit form
/// when the high bit of the first unit is set. Returns `(length, bytes_used)`.
fn decode_u16_length(data: &[u8], pos: usize, end: usize) -> Option<(usize, usize)> {
    if pos.checked_add(2)? > end {
        return None;
    }
    let w0 = read_u16(data, pos)? as usize;
    if w0 & 0x8000 != 0 {
        if pos.checked_add(4)? > end {
            return None;
        }
        let w1 = read_u16(data, pos + 2)? as usize;
        Some((((w0 & 0x7FFF) << 16) | w1, 4))
    } else {
        Some((w0, 2))
    }
}

/// Lenient decoder for UTF-8 *or* Modified UTF-8 (CESU-8) bytes.
///
/// Framework `resources.arsc` files may contain Modified UTF-8: supplementary
/// characters encoded as two 3-byte surrogate sequences (which standard UTF-8
/// validation rejects). Fast path: plain `from_utf8`. Fallback: raw code-point
/// decoding that pairs CESU-8 surrogates and replaces anything malformed with
/// U+FFFD — never fails.
fn decode_modified_utf8(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }

    const REPLACEMENT: char = '\u{FFFD}';
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let (cp, advance) = decode_raw_code_point(bytes, i);
        match cp {
            None => {
                out.push(REPLACEMENT);
                i += advance;
            }
            Some(hi) if (0xD800..0xDC00).contains(&hi) => {
                // High surrogate: try to pair it with a following CESU-8
                // encoded low surrogate.
                let (cp2, advance2) = decode_raw_code_point(bytes, i + advance);
                match cp2 {
                    Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                        let combined = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                        out.push(char::from_u32(combined).unwrap_or(REPLACEMENT));
                        i += advance + advance2;
                    }
                    _ => {
                        // Lone high surrogate.
                        out.push(REPLACEMENT);
                        i += advance;
                    }
                }
            }
            Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                // Lone low surrogate.
                out.push(REPLACEMENT);
                i += advance;
            }
            Some(cp) => {
                out.push(char::from_u32(cp).unwrap_or(REPLACEMENT));
                i += advance;
            }
        }
    }
    out
}

/// Decodes one UTF-8-shaped sequence at `i` without rejecting surrogate code
/// points (needed for CESU-8). Returns `(code_point, bytes_consumed)`;
/// `(None, 1)` for malformed leads or truncated sequences.
fn decode_raw_code_point(bytes: &[u8], i: usize) -> (Option<u32>, usize) {
    let Some(&b0) = bytes.get(i) else {
        return (None, 1);
    };
    if b0 < 0x80 {
        return (Some(b0 as u32), 1);
    }
    let (continuation_count, initial) = if b0 & 0xE0 == 0xC0 {
        (1usize, (b0 & 0x1F) as u32)
    } else if b0 & 0xF0 == 0xE0 {
        (2, (b0 & 0x0F) as u32)
    } else if b0 & 0xF8 == 0xF0 {
        (3, (b0 & 0x07) as u32)
    } else {
        return (None, 1);
    };
    let mut cp = initial;
    for k in 1..=continuation_count {
        match bytes.get(i + k) {
            Some(&b) if b & 0xC0 == 0x80 => cp = (cp << 6) | (b & 0x3F) as u32,
            _ => return (None, 1),
        }
    }
    (Some(cp), continuation_count + 1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a pool with plain + styled + non-ASCII strings, then sorts it.
    /// Returns the pool, `(ref, value)` pairs for the plain strings, and the
    /// styled string's handle.
    fn build_test_pool() -> (StringPool, Vec<(Ref, String)>, StyleRef) {
        let mut pool = StringPool::new();

        // Styled string first (its spans intern "b" and "i" as plain strings).
        let style = pool.make_style_ref(
            "Bold and italic text",
            vec![("b".to_string(), 0, 3), ("i".to_string(), 9, 14)],
            Context::default(),
        );

        let long = "x".repeat(200); // > 127 bytes -> 2-byte u8 length encoding
        let inputs: Vec<(String, Context)> = vec![
            ("hello world".to_string(), Context::default()),
            (
                "zebra".to_string(),
                Context::with_priority(Context::HIGH_PRIORITY),
            ),
            ("apple".to_string(), Context::default()),
            ("emoji \u{1F389} party".to_string(), Context::default()), // surrogate pair
            (
                "\u{4F60}\u{597D}\u{4E16}\u{754C}".to_string(),
                Context::default(),
            ), // CJK
            (long, Context::default()),
        ];
        let refs: Vec<(Ref, String)> = inputs
            .into_iter()
            .map(|(s, ctx)| {
                let r = pool.make_ref(&s, ctx);
                (r, s)
            })
            .collect();

        pool.sort();
        (pool, refs, style)
    }

    fn check_roundtrip(utf8: bool) {
        let (pool, refs, style) = build_test_pool();
        let bytes = if utf8 {
            pool.flatten_utf8()
        } else {
            pool.flatten_utf16()
        };

        // Whole chunk is 4-byte aligned and self-describing.
        assert_eq!(bytes.len() % 4, 0, "chunk must be 4-byte aligned");
        assert_eq!(read_u32(&bytes, 4).unwrap() as usize, bytes.len());

        let parsed = BinaryStringPool::parse(&bytes).expect("parse flattened pool");
        assert_eq!(parsed.is_utf8(), utf8);
        assert_eq!(parsed.len(), pool.len());
        assert_eq!(parsed.style_count(), 1);

        // Every plain string round-trips at its resolved index.
        for (r, expected) in &refs {
            let idx = pool.resolve(*r);
            assert_eq!(
                parsed.get(idx).as_deref(),
                Some(expected.as_str()),
                "string {expected:?} at index {idx} (utf8={utf8})"
            );
        }

        // The styled string occupies index 0 (styles always come first).
        let style_idx = pool.resolve_style(style);
        assert_eq!(style_idx, 0);
        assert_eq!(
            parsed.get(style_idx).as_deref(),
            Some("Bold and italic text")
        );

        // Spans round-trip, and the span names resolve through the pool.
        let spans = parsed.spans(style_idx);
        assert_eq!(spans.len(), 2);
        assert_eq!(parsed.get(spans[0].0 as usize).as_deref(), Some("b"));
        assert_eq!((spans[0].1, spans[0].2), (0, 3));
        assert_eq!(parsed.get(spans[1].0 as usize).as_deref(), Some("i"));
        assert_eq!((spans[1].1, spans[1].2), (9, 14));

        // Unstyled strings have no spans; out-of-range index decodes to None.
        assert!(parsed.spans(1).is_empty());
        assert!(parsed.get(parsed.len()).is_none());
    }

    #[test]
    fn roundtrip_utf8() {
        check_roundtrip(true);
    }

    #[test]
    fn roundtrip_utf16() {
        check_roundtrip(false);
    }

    #[test]
    fn utf8_flag_bit() {
        let (pool, _, _) = build_test_pool();

        let utf8_bytes = pool.flatten_utf8();
        let flags = read_u32(&utf8_bytes, 16).unwrap();
        assert_ne!(flags & UTF8_FLAG, 0, "UTF8_FLAG must be set in utf8 mode");

        let utf16_bytes = pool.flatten_utf16();
        let flags = read_u32(&utf16_bytes, 16).unwrap();
        assert_eq!(
            flags & UTF8_FLAG,
            0,
            "UTF8_FLAG must be clear in utf16 mode"
        );
    }

    #[test]
    fn priority_ordering_after_sort() {
        let (pool, refs, style) = build_test_pool();
        let find = |needle: &str| {
            refs.iter()
                .find(|(_, s)| s == needle)
                .map(|(r, _)| *r)
                .expect("ref present")
        };

        let zebra = pool.resolve(find("zebra")); // high priority (1)
        let apple = pool.resolve(find("apple")); // normal priority
        let hello = pool.resolve(find("hello world")); // normal priority

        // Style entries always occupy the first indices.
        assert_eq!(pool.resolve_style(style), 0);
        assert!(zebra >= pool.style_count());

        // Priority ascending wins over lexicographic order...
        assert!(zebra < apple, "high priority sorts before normal priority");
        // ...and equal priorities are ordered lexicographically.
        assert!(
            apple < hello,
            "\"apple\" < \"hello world\" at equal priority"
        );
    }

    #[test]
    fn dedup_and_min_priority_merge() {
        let mut pool = StringPool::new();
        let a = pool.make_ref("dup", Context::default());
        let b = pool.make_ref("dup", Context::default());
        assert_eq!(a, b, "same string + same context dedupes");

        // Same string + same config key merges, taking the minimum priority.
        let c = pool.make_ref("dup", Context::with_priority(Context::HIGH_PRIORITY));
        assert_eq!(a, c);

        let d = pool.make_ref("aaaa", Context::default());
        pool.sort();
        // After the merge, "dup" carries HIGH_PRIORITY and sorts before
        // "aaaa" despite being lexicographically larger.
        assert!(pool.resolve(a) < pool.resolve(d));
        assert_eq!(pool.len(), 2);

        // Distinct config keys (and distinct priorities) make a new entry.
        let e = pool.make_ref("dup", Context::new(Context::LOW_PRIORITY, vec![1, 2, 3]));
        assert_ne!(a, e);
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn two_byte_length_and_string_too_large() {
        // 32768 ASCII chars: UTF-16 length 0x8000 > 0x7FFF.
        let big = "a".repeat(0x8000);
        let mut pool = StringPool::new();
        let r = pool.make_ref(&big, Context::default());
        pool.sort();

        // UTF-16 mode handles it via the 2-unit length encoding.
        let utf16_bytes = pool.flatten_utf16();
        assert_eq!(utf16_bytes.len() % 4, 0);
        let parsed = BinaryStringPool::parse(&utf16_bytes).expect("parse utf16");
        assert_eq!(parsed.get(pool.resolve(r)).as_deref(), Some(big.as_str()));

        // UTF-8 mode cannot encode the length: aapt2 substitutes
        // STRING_TOO_LARGE (and logs an error).
        let utf8_bytes = pool.flatten_utf8();
        let parsed = BinaryStringPool::parse(&utf8_bytes).expect("parse utf8");
        assert_eq!(
            parsed.get(pool.resolve(r)).as_deref(),
            Some(STRING_TOO_LARGE)
        );
    }

    #[test]
    fn modified_utf8_surrogate_pair_roundtrip() {
        // A supplementary character must be CESU-8 encoded in utf8 mode
        // (two 3-byte surrogates), and the parser must decode it back.
        let mut pool = StringPool::new();
        let r = pool.make_ref("\u{1F600}", Context::default()); // 😀 U+1F600
        pool.sort();

        let bytes = pool.flatten_utf8();
        let parsed = BinaryStringPool::parse(&bytes).expect("parse");
        assert_eq!(parsed.get(pool.resolve(r)).as_deref(), Some("\u{1F600}"));

        // The on-disk bytes are CESU-8: 6 data bytes, not 4. Entry layout is
        // [u16len=2][u8len=6][6 bytes][NUL] at stringsStart.
        let strings_start = read_u32(&bytes, 20).unwrap() as usize;
        assert_eq!(bytes[strings_start], 2); // UTF-16 length
        assert_eq!(bytes[strings_start + 1], 6); // Modified-UTF-8 byte length
        assert_eq!(
            &bytes[strings_start + 2..strings_start + 5],
            &[0xED, 0xA0, 0xBD]
        );
        assert_eq!(
            &bytes[strings_start + 5..strings_start + 8],
            &[0xED, 0xB8, 0x80]
        );
    }

    #[test]
    fn empty_pool_roundtrip() {
        let pool = StringPool::new();
        for bytes in [pool.flatten_utf8(), pool.flatten_utf16()] {
            assert_eq!(bytes.len(), STRING_POOL_HEADER_SIZE);
            assert_eq!(bytes.len() % 4, 0);
            let parsed = BinaryStringPool::parse(&bytes).expect("parse empty pool");
            assert_eq!(parsed.len(), 0);
            assert_eq!(parsed.style_count(), 0);
            assert!(parsed.get(0).is_none());
            assert!(parsed.spans(0).is_empty());
        }
    }

    #[test]
    fn parse_rejects_malformed() {
        // Too small / empty.
        assert!(BinaryStringPool::parse(&[]).is_none());
        assert!(BinaryStringPool::parse(&[0u8; 8]).is_none());

        let (pool, _, _) = build_test_pool();
        let good = pool.flatten_utf8();
        assert!(BinaryStringPool::parse(&good).is_some());

        // Wrong chunk type.
        let mut bad = good.clone();
        bad[0] = 0x02;
        assert!(BinaryStringPool::parse(&bad).is_none());

        // Declared size larger than the data.
        let mut bad = good.clone();
        let huge = (good.len() as u32 + 4).to_le_bytes();
        bad[4..8].copy_from_slice(&huge);
        assert!(BinaryStringPool::parse(&bad).is_none());

        // Truncated chunk.
        assert!(BinaryStringPool::parse(&good[..good.len() - 4]).is_none());

        // Unaligned header size.
        let mut bad = good.clone();
        bad[2] = 30;
        assert!(BinaryStringPool::parse(&bad).is_none());

        // stringsStart beyond the chunk.
        let mut bad = good.clone();
        let far = (good.len() as u32).to_le_bytes();
        bad[20..24].copy_from_slice(&far);
        assert!(BinaryStringPool::parse(&bad).is_none());

        // Styles starting before strings.
        let mut bad = good;
        bad[24..28].copy_from_slice(&8u32.to_le_bytes());
        assert!(BinaryStringPool::parse(&bad).is_none());
    }

    #[test]
    fn lenient_cesu8_decoder() {
        // Standard UTF-8 passes through.
        assert_eq!(decode_modified_utf8("plain".as_bytes()), "plain");
        assert_eq!(decode_modified_utf8("\u{1F389}".as_bytes()), "\u{1F389}");

        // CESU-8 surrogate pair for U+1F600.
        let cesu8 = [0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80];
        assert_eq!(decode_modified_utf8(&cesu8), "\u{1F600}");

        // Lone high surrogate -> replacement character, no panic.
        let lone = [0xED, 0xA0, 0xBD, b'x'];
        assert_eq!(decode_modified_utf8(&lone), "\u{FFFD}x");

        // Garbage bytes -> replacement characters, no panic.
        let garbage = [0xFF, 0xC0, 0x20];
        assert_eq!(decode_modified_utf8(&garbage), "\u{FFFD}\u{FFFD} ");
    }
}
