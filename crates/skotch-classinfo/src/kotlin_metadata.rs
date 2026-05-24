//! Kotlin `@Metadata` decoding — recover Kotlin-level type information
//! that the JVM erases.
//!
//! kotlinc stamps every Kotlin class with a `@kotlin.Metadata`
//! annotation carrying a protobuf-encoded description of the
//! declaration: its functions, their parameter types, type variables,
//! receiver-ness (`T.() -> R` vs `(T) -> R`), `suspend`-ness, default
//! values, parameter names, etc. The JVM `Signature` attribute erases
//! all of that (`T.() -> R` and `(T) -> R` both become `Function1`), so
//! reading `@Metadata` is the only way to recover it from a `.class`
//! file — which is exactly how kotlinc resolves stdlib calls without
//! hardcoding their names. Wiring this in lets skotch shrink the
//! name-based fallbacks in [`skotch_types::intrinsics`] (task #297).
//!
//! Layers, each verified against a real kotlinc-compiled class
//! (`tests/data/MetaProbeKt.class`, exercised by
//! `tests/metadata_extraction.rs`):
//!
//!   * [`bit_encoding`] — the `@Metadata.d1` `String[]` ⇄ `byte[]`
//!     codec, a direct port of
//!     `org.jetbrains.kotlin.metadata.jvm.deserialization.BitEncoding`
//!     (+ `utfEncoding.kt`).
//!   * [`protobuf`] — a minimal protobuf-2 wire reader for the decoded
//!     `ProtoBuf.Class` / `ProtoBuf.Package` messages
//!     (schema: `kotlin/core/metadata/src/metadata.proto`).
//!   * [`decode_modified_utf8`] + [`RawMetadata`] — the constant-pool
//!     annotation read (`skotch_classinfo::parse_class` populates
//!     `ClassInfo.metadata`).
//!   * [`parse_metadata`] + [`NameResolver`] — the schema walk: recovers
//!     each [`FunctionInfo`]'s name, parameter names, and whether a
//!     parameter is a *receiver* function type (`T.() -> R`), which is
//!     the structural signal kotlinc uses for scope-fn `this`/`it`.
//!
//! Still TODO: feeding the recovered signatures into the inference
//! unifier and retiring the corresponding name lists in
//! `skotch_types::intrinsics` (the `@Metadata` reader itself is done).

/// `@Metadata.d1` codec: the array of `String`s stored in the
/// annotation back into the raw protobuf byte stream.
///
/// Port of `BitEncoding`/`utfEncoding.kt`. Two on-disk encodings exist
/// and are distinguished by the first character of the first string:
///
///   * **UTF-8 mode** (`U+0000` marker) — the modern default. Each
///     subsequent character holds one byte (its low 8 bits).
///   * **8-to-7 mode** (`U+FFFF` marker, or no marker for the oldest
///     form) — bytes were repacked into 7-bit groups to dodge the
///     `0xf0..0xff` range disallowed in JVM UTF-8 constant-pool entries.
///
/// Input strings are assumed already decoded from Modified UTF-8 (so a
/// character's value is in `0..=0xFF`), which is what a constant-pool
/// reader yields.
pub mod bit_encoding {
    /// First char of `d1[0]` when the payload is UTF-8 encoded.
    const UTF8_MODE_MARKER: char = '\u{0}';
    /// First char of `d1[0]` when the payload is 8-to-7 encoded.
    const MODE_8TO7_MARKER: char = '\u{ffff}';

    /// Decode the `d1` string array into the raw protobuf bytes.
    pub fn decode_bytes(strings: &[String]) -> Vec<u8> {
        if let Some(marker) = strings.first().and_then(|s| s.chars().next()) {
            if marker == UTF8_MODE_MARKER {
                // UTF-8 mode: drop the marker, concatenate bytes.
                return strings_to_bytes(&drop_first_char(strings));
            }
            if marker == MODE_8TO7_MARKER {
                // 8-to-7 mode with explicit marker: drop it, then decode.
                return decode_8to7(&drop_first_char(strings));
            }
        }
        // No marker → legacy 8-to-7 form.
        decode_8to7(strings)
    }

    /// Concatenate every character's low byte across all strings.
    /// (`stringsToBytes` / `combineStringArrayIntoBytes` — they perform
    /// the identical truncating cast.)
    fn strings_to_bytes(strings: &[String]) -> Vec<u8> {
        let mut out = Vec::new();
        for s in strings {
            for c in s.chars() {
                out.push((c as u32) as u8);
            }
        }
        out
    }

    /// Return a copy of `strings` with the first character of the first
    /// element removed (the mode marker).
    fn drop_first_char(strings: &[String]) -> Vec<String> {
        let mut out = strings.to_vec();
        if let Some(first) = out.first_mut() {
            let mut chars = first.chars();
            chars.next();
            *first = chars.as_str().to_string();
        }
        out
    }

    /// Combine → subtract-1-modulo-128 → unpack 7-bit groups into bytes.
    fn decode_8to7(strings: &[String]) -> Vec<u8> {
        let mut bytes = strings_to_bytes(strings);
        // Adding 0x7f modulo 128 is the inverse of the `+1` applied
        // during encoding.
        add_modulo_byte(&mut bytes, 0x7f);
        decode_7to8(&bytes)
    }

    /// `data[i] = (data[i] + increment) mod 128`.
    fn add_modulo_byte(data: &mut [u8], increment: u8) {
        for b in data.iter_mut() {
            *b = ((*b as u32 + increment as u32) & 0x7f) as u8;
        }
    }

    /// Reassemble the least-significant 7 bits of each input byte into a
    /// contiguous bit string and re-split it into 8-bit output bytes,
    /// dropping the final padding bits. Port of `decode7to8`.
    fn decode_7to8(data: &[u8]) -> Vec<u8> {
        let result_length = 7 * data.len() / 8;
        let mut result = vec![0u8; result_length];
        let mut byte_index = 0usize;
        let mut bit = 0u32;
        for item in result.iter_mut() {
            let first_part = (data[byte_index] as u32) >> bit;
            byte_index += 1;
            let second_part = ((data[byte_index] as u32) & ((1u32 << (bit + 1)) - 1)) << (7 - bit);
            *item = (first_part + second_part) as u8;
            if bit == 6 {
                byte_index += 1;
                bit = 0;
            } else {
                bit += 1;
            }
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Test-only UTF-8 encoder (single chunk — the 64 KB splitting
        /// the real `bytesToStrings` does is irrelevant for round-trips).
        fn encode_utf8(bytes: &[u8]) -> Vec<String> {
            let mut s = String::new();
            s.push(UTF8_MODE_MARKER);
            for &b in bytes {
                s.push(char::from_u32(b as u32).unwrap());
            }
            vec![s]
        }

        /// Test-only port of `encode8to7` (mirror of [`decode_7to8`]).
        fn encode_8to7_raw(data: &[u8]) -> Vec<u8> {
            let result_length = (data.len() * 8).div_ceil(7);
            if result_length == 0 {
                return Vec::new();
            }
            let mut result = vec![0u8; result_length];
            let mut byte_index = 0usize;
            let mut bit = 0u32;
            // All output bytes except the last are full 7-bit chunks.
            let (head, last_slot) = result.split_at_mut(result_length - 1);
            for item in head.iter_mut() {
                if bit == 0 {
                    *item = data[byte_index] & 0x7f;
                    bit = 7;
                    continue;
                }
                let first_part = (data[byte_index] as u32) >> bit;
                let new_bit = (bit + 7) & 7;
                byte_index += 1;
                let second_part =
                    ((data[byte_index] as u32) & ((1u32 << new_bit) - 1)) << (8 - bit);
                *item = (first_part + second_part) as u8;
                bit = new_bit;
            }
            // The final byte is just the remaining high bits, zero-padded.
            last_slot[0] = ((data[byte_index] as u32) >> bit) as u8;
            result
        }

        /// Test-only 8-to-7 encoder (single chunk, with marker).
        fn encode_8to7(bytes: &[u8]) -> Vec<String> {
            let mut packed = encode_8to7_raw(bytes);
            add_modulo_byte(&mut packed, 1);
            let mut s = String::new();
            s.push(MODE_8TO7_MARKER);
            for &b in &packed {
                s.push(char::from_u32(b as u32).unwrap());
            }
            vec![s]
        }

        fn sample_payloads() -> Vec<Vec<u8>> {
            vec![
                vec![],
                vec![0x00],
                vec![0x7f],
                vec![0xff],
                vec![0x00, 0x00, 0x00],
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
                (0u16..=255).map(|b| b as u8).collect(),
                b"\x0a\x06listOf\x12\x00".to_vec(),
            ]
        }

        #[test]
        fn utf8_round_trips() {
            for payload in sample_payloads() {
                let encoded = encode_utf8(&payload);
                assert_eq!(decode_bytes(&encoded), payload, "utf8 payload {payload:?}");
            }
        }

        #[test]
        fn eight_to_seven_round_trips() {
            for payload in sample_payloads() {
                let encoded = encode_8to7(&payload);
                assert_eq!(decode_bytes(&encoded), payload, "8to7 payload {payload:?}");
            }
        }

        #[test]
        fn empty_input_is_empty() {
            assert!(decode_bytes(&[]).is_empty());
        }
    }
}

/// Minimal protobuf-2 wire-format reader.
///
/// Enough to walk the messages in `metadata.proto` (`ProtoBuf.Class`,
/// `Package`, `Function`, `ValueParameter`, `Type`): read tags, varints
/// and length-delimited sub-messages, and skip fields we don't care
/// about. Deliberately tiny — no codegen, no schema types; callers
/// dispatch on field numbers themselves.
pub mod protobuf {
    /// Wire type 0 — varint (`int32`/`int64`/`bool`/`enum`).
    pub const WIRE_VARINT: u8 = 0;
    /// Wire type 1 — 64-bit fixed (`fixed64`/`double`).
    pub const WIRE_I64: u8 = 1;
    /// Wire type 2 — length-delimited (`string`/`bytes`/sub-message).
    pub const WIRE_LEN: u8 = 2;
    /// Wire type 5 — 32-bit fixed (`fixed32`/`float`).
    pub const WIRE_I32: u8 = 5;

    /// A cursor over a protobuf message body.
    pub struct Reader<'a> {
        buf: &'a [u8],
        pos: usize,
    }

    impl<'a> Reader<'a> {
        pub fn new(buf: &'a [u8]) -> Self {
            Reader { buf, pos: 0 }
        }

        /// True once the whole message body has been consumed.
        pub fn is_at_end(&self) -> bool {
            self.pos >= self.buf.len()
        }

        /// Read a base-128 varint. `None` on truncation or overflow.
        pub fn read_varint(&mut self) -> Option<u64> {
            let mut result: u64 = 0;
            let mut shift: u32 = 0;
            loop {
                let byte = *self.buf.get(self.pos)?;
                self.pos += 1;
                result |= u64::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    return Some(result);
                }
                shift += 7;
                if shift >= 64 {
                    return None;
                }
            }
        }

        /// Read a field tag, returning `(field_number, wire_type)`.
        /// `None` at clean end-of-message or on a zero field number.
        pub fn read_tag(&mut self) -> Option<(u32, u8)> {
            if self.is_at_end() {
                return None;
            }
            let key = self.read_varint()?;
            let field = (key >> 3) as u32;
            let wire = (key & 0x7) as u8;
            if field == 0 {
                return None;
            }
            Some((field, wire))
        }

        /// Read a length-delimited field's bytes (string / sub-message).
        pub fn read_len_bytes(&mut self) -> Option<&'a [u8]> {
            let len = self.read_varint()? as usize;
            let start = self.pos;
            let end = start.checked_add(len)?;
            if end > self.buf.len() {
                return None;
            }
            self.pos = end;
            Some(&self.buf[start..end])
        }

        /// Read a length-delimited field as a fresh sub-message reader.
        pub fn read_message(&mut self) -> Option<Reader<'a>> {
            Some(Reader::new(self.read_len_bytes()?))
        }

        /// The not-yet-consumed remainder of the buffer. Used to pick up
        /// the message that follows a `parseDelimitedFrom`-style prefix
        /// (e.g. `ProtoBuf.Package` after the leading `StringTableTypes`).
        pub fn remaining(&self) -> &'a [u8] {
            &self.buf[self.pos.min(self.buf.len())..]
        }

        fn read_fixed(&mut self, n: usize) -> Option<u64> {
            let end = self.pos.checked_add(n)?;
            if end > self.buf.len() {
                return None;
            }
            let mut v: u64 = 0;
            for (i, &b) in self.buf[self.pos..end].iter().enumerate() {
                v |= u64::from(b) << (8 * i);
            }
            self.pos = end;
            Some(v)
        }

        pub fn read_fixed32(&mut self) -> Option<u32> {
            self.read_fixed(4).map(|v| v as u32)
        }

        pub fn read_fixed64(&mut self) -> Option<u64> {
            self.read_fixed(8)
        }

        /// Advance past a field of the given wire type.
        pub fn skip_field(&mut self, wire: u8) -> Option<()> {
            match wire {
                WIRE_VARINT => self.read_varint().map(|_| ()),
                WIRE_I64 => self.read_fixed64().map(|_| ()),
                WIRE_LEN => self.read_len_bytes().map(|_| ()),
                WIRE_I32 => self.read_fixed32().map(|_| ()),
                // Wire types 3/4 (start/end group) are deprecated and
                // never appear in Kotlin metadata.
                _ => None,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Encode a varint (test helper).
        fn varint(mut v: u64) -> Vec<u8> {
            let mut out = Vec::new();
            loop {
                let mut byte = (v & 0x7f) as u8;
                v >>= 7;
                if v != 0 {
                    byte |= 0x80;
                }
                out.push(byte);
                if v == 0 {
                    return out;
                }
            }
        }

        #[test]
        fn varints_decode() {
            for value in [0u64, 1, 127, 128, 150, 300, 16384, u32::MAX as u64] {
                let bytes = varint(value);
                let mut r = Reader::new(&bytes);
                assert_eq!(r.read_varint(), Some(value));
                assert!(r.is_at_end());
            }
        }

        #[test]
        fn walks_a_message() {
            // field 1 (varint) = 150; field 3 (len) = "abc";
            // field 9 (varint) = 6.
            let mut msg = Vec::new();
            msg.extend(varint((1 << 3) | u64::from(WIRE_VARINT)));
            msg.extend(varint(150));
            msg.extend(varint((3 << 3) | u64::from(WIRE_LEN)));
            msg.extend(varint(3));
            msg.extend_from_slice(b"abc");
            msg.extend(varint((9 << 3) | u64::from(WIRE_VARINT)));
            msg.extend(varint(6));

            let mut r = Reader::new(&msg);
            let mut seen = Vec::new();
            while let Some((field, wire)) = r.read_tag() {
                match (field, wire) {
                    (1, WIRE_VARINT) => seen.push(("f1", r.read_varint().unwrap())),
                    (3, WIRE_LEN) => {
                        assert_eq!(r.read_len_bytes().unwrap(), b"abc");
                        seen.push(("f3", 0));
                    }
                    (9, WIRE_VARINT) => seen.push(("f9", r.read_varint().unwrap())),
                    (_, w) => r.skip_field(w).unwrap(),
                }
            }
            assert_eq!(seen, vec![("f1", 150), ("f3", 0), ("f9", 6)]);
        }

        #[test]
        fn skip_unknown_fields() {
            // field 2 (i64) then field 4 (i32) then field 5 (varint)=42.
            let mut msg = Vec::new();
            msg.extend(varint((2 << 3) | u64::from(WIRE_I64)));
            msg.extend_from_slice(&[0; 8]);
            msg.extend(varint((4 << 3) | u64::from(WIRE_I32)));
            msg.extend_from_slice(&[0; 4]);
            msg.extend(varint((5 << 3) | u64::from(WIRE_VARINT)));
            msg.extend(varint(42));

            let mut r = Reader::new(&msg);
            let mut last = None;
            while let Some((field, wire)) = r.read_tag() {
                if field == 5 {
                    last = r.read_varint();
                } else {
                    r.skip_field(wire).unwrap();
                }
            }
            assert_eq!(last, Some(42));
        }

        #[test]
        fn truncated_varint_is_none() {
            let bytes = [0x80u8]; // continuation bit set, no next byte
            let mut r = Reader::new(&bytes);
            assert_eq!(r.read_varint(), None);
        }
    }
}

/// Decode a JVMS "Modified UTF-8" byte sequence (the encoding of
/// `CONSTANT_Utf8_info`, JVMS §4.4.7) into a `String`.
///
/// Modified UTF-8 differs from standard UTF-8 in two ways that matter
/// here, both of which appear in `@Metadata.d1` payloads:
///   * the null character `U+0000` is encoded as the two bytes
///     `0xC0 0x80` (never a bare `0x00`), and
///   * characters `U+0080..U+00FF` (common in the packed metadata
///     bytes) use the two-byte form.
///
/// Because of this, `std::str::from_utf8` rejects these payloads, so
/// the constant-pool reader must use this decoder. Malformed sequences
/// are replaced with `U+FFFD`. (Six-byte supplementary encodings are
/// not used by `@Metadata` and decode to replacement chars.)
pub fn decode_modified_utf8(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (code, width) = if b & 0x80 == 0 {
            // 0xxxxxxx — single byte (0x01..0x7F).
            (u32::from(b), 1)
        } else if b & 0xE0 == 0xC0 {
            // 110xxxxx 10xxxxxx — two bytes.
            match bytes.get(i + 1) {
                Some(&b2) => (((u32::from(b) & 0x1F) << 6) | (u32::from(b2) & 0x3F), 2),
                None => (0xFFFD, 1),
            }
        } else if b & 0xF0 == 0xE0 {
            // 1110xxxx 10xxxxxx 10xxxxxx — three bytes.
            match (bytes.get(i + 1), bytes.get(i + 2)) {
                (Some(&b2), Some(&b3)) => (
                    ((u32::from(b) & 0x0F) << 12)
                        | ((u32::from(b2) & 0x3F) << 6)
                        | (u32::from(b3) & 0x3F),
                    3,
                ),
                _ => (0xFFFD, 1),
            }
        } else {
            (0xFFFD, 1)
        };
        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
        i += width;
    }
    out
}

/// Raw, undecoded contents of a `@kotlin.Metadata` annotation, as read
/// straight from the class-file constant pool.
///
/// Mirrors the annotation's elements (see `kotlin.Metadata`):
///   * `kind` — `k`: 1 = class, 2 = file facade (top-level members),
///     3 = synthetic class, 4 = multi-file facade, 5 = multi-file part.
///   * `data1` — `d1`: the `BitEncoding`-packed protobuf payload
///     (decode with [`bit_encoding::decode_bytes`]).
///   * `data2` — `d2`: the string table the protobuf indices reference.
///
/// This is the input to the (forthcoming) `metadata.proto` schema walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMetadata {
    pub kind: i32,
    pub data1: Vec<String>,
    pub data2: Vec<String>,
}

#[cfg(test)]
mod mutf8_tests {
    use super::decode_modified_utf8;

    #[test]
    fn ascii_is_identity() {
        assert_eq!(decode_modified_utf8(b"listOf"), "listOf");
        assert_eq!(decode_modified_utf8(b""), "");
    }

    #[test]
    fn null_is_two_bytes() {
        // U+0000 encodes as 0xC0 0x80 (the UTF-8-mode marker).
        let s = decode_modified_utf8(&[0xC0, 0x80, b'A']);
        let chars: Vec<u32> = s.chars().map(|c| c as u32).collect();
        assert_eq!(chars, vec![0x00, 0x41]);
    }

    #[test]
    fn high_bytes_two_byte_form() {
        // 0xFF → 0xC3 0xBF ; 0x80 → 0xC2 0x80.
        let s = decode_modified_utf8(&[0xC3, 0xBF, 0xC2, 0x80]);
        let chars: Vec<u32> = s.chars().map(|c| c as u32).collect();
        assert_eq!(chars, vec![0xFF, 0x80]);
    }

    #[test]
    fn three_byte_marker() {
        // U+FFFF (the 8-to-7 mode marker) → 0xEF 0xBF 0xBF.
        let s = decode_modified_utf8(&[0xEF, 0xBF, 0xBF]);
        assert_eq!(s.chars().next().map(|c| c as u32), Some(0xFFFF));
    }
}

// ── Schema walk: decoded protobuf → Kotlin declarations ─────────
//
// The decoded `d1` byte stream is a length-delimited
// `JvmProtoBuf.StringTableTypes` (the JVM name table) followed by a
// `ProtoBuf.Package` (file facade, k=2) or `ProtoBuf.Class` (k=1), per
// `JvmProtoBufUtil.readPackageDataFrom`. Names are indices resolved by
// [`NameResolver`] (port of `JvmNameResolverBase`). Field numbers below
// come from `kotlin/core/metadata/src/metadata.proto`.

use protobuf::{Reader, WIRE_LEN, WIRE_VARINT};

/// kotlinc's predefined string table
/// (`JvmNameResolverBase.PREDEFINED_STRINGS`). Records may reference
/// these by index instead of carrying the string in `d2`.
const PREDEFINED_STRINGS: &[&str] = &[
    "kotlin/Any",
    "kotlin/Nothing",
    "kotlin/Unit",
    "kotlin/Throwable",
    "kotlin/Number",
    "kotlin/Byte",
    "kotlin/Double",
    "kotlin/Float",
    "kotlin/Int",
    "kotlin/Long",
    "kotlin/Short",
    "kotlin/Boolean",
    "kotlin/Char",
    "kotlin/CharSequence",
    "kotlin/String",
    "kotlin/Comparable",
    "kotlin/Enum",
    "kotlin/Array",
    "kotlin/ByteArray",
    "kotlin/DoubleArray",
    "kotlin/FloatArray",
    "kotlin/IntArray",
    "kotlin/LongArray",
    "kotlin/ShortArray",
    "kotlin/BooleanArray",
    "kotlin/CharArray",
    "kotlin/Cloneable",
    "kotlin/Annotation",
    "kotlin/collections/Iterable",
    "kotlin/collections/MutableIterable",
    "kotlin/collections/Collection",
    "kotlin/collections/MutableCollection",
    "kotlin/collections/List",
    "kotlin/collections/MutableList",
    "kotlin/collections/Set",
    "kotlin/collections/MutableSet",
    "kotlin/collections/Map",
    "kotlin/collections/MutableMap",
    "kotlin/collections/Map.Entry",
    "kotlin/collections/MutableMap.MutableEntry",
    "kotlin/collections/Iterator",
    "kotlin/collections/MutableIterator",
    "kotlin/collections/ListIterator",
    "kotlin/collections/MutableListIterator",
];

/// One `StringTableTypes.Record` (already range-expanded into one entry
/// per covered index).
#[derive(Default, Clone)]
struct Record {
    range: i32,
    predefined_index: Option<i32>,
    string: Option<String>,
    operation: i32,
    substring: Vec<i32>,
    replace_char: Vec<i32>,
}

/// Resolves a metadata name/FQ-class index to its string. Port of
/// `JvmNameResolverBase.getString` (the JVM resolver also routes
/// `getQualifiedClassName` through `getString`).
pub struct NameResolver {
    strings: Vec<String>,
    records: Vec<Record>,
}

impl NameResolver {
    /// Resolve the string at `index`.
    pub fn get_string(&self, index: usize) -> String {
        let record = match self.records.get(index) {
            Some(r) => r,
            None => return self.strings.get(index).cloned().unwrap_or_default(),
        };
        let mut s = if let Some(str) = &record.string {
            str.clone()
        } else if let Some(pi) = record.predefined_index.filter(|&pi| pi >= 0) {
            PREDEFINED_STRINGS
                .get(pi as usize)
                .map(|p| (*p).to_string())
                .unwrap_or_else(|| self.strings.get(index).cloned().unwrap_or_default())
        } else {
            self.strings.get(index).cloned().unwrap_or_default()
        };
        if record.substring.len() >= 2 {
            let (begin, end) = (record.substring[0], record.substring[1]);
            let chars: Vec<char> = s.chars().collect();
            if begin >= 0 && begin <= end && (end as usize) <= chars.len() {
                s = chars[begin as usize..end as usize].iter().collect();
            }
        }
        if record.replace_char.len() >= 2 {
            if let (Some(from), Some(to)) = (
                char::from_u32(record.replace_char[0] as u32),
                char::from_u32(record.replace_char[1] as u32),
            ) {
                s = s.replace(from, &to.to_string());
            }
        }
        match record.operation {
            // INTERNAL_TO_CLASS_ID
            1 => s = s.replace('$', "."),
            // DESC_TO_CLASS_ID
            2 => {
                let chars: Vec<char> = s.chars().collect();
                if chars.len() >= 2 {
                    s = chars[1..chars.len() - 1].iter().collect();
                }
                s = s.replace('$', ".");
            }
            _ => {}
        }
        s
    }
}

/// A Kotlin type recovered from `@Metadata` (`ProtoBuf.Type`), in the
/// reduced form skotch's inferrer needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeInfo {
    /// Resolved fully-qualified class name (e.g. `kotlin/Int`,
    /// `kotlin/collections/List`, `kotlin/Function1`). `None` for a bare
    /// type variable.
    pub class_name: Option<String>,
    /// Whether the type is nullable (`T?`).
    pub nullable: bool,
    /// True when the type is a *receiver* function type (`T.() -> R`),
    /// as opposed to a plain function type (`(T) -> R`). This is the
    /// structural signal kotlinc uses to bind `this` vs `it` in
    /// scope-function lambdas (`KotlinType.isExtensionFunctionType`).
    pub is_extension_function_type: bool,
    /// Generic type arguments, e.g. `[String]` for `List<String>` or
    /// `[String, List<Int>]` for `Map<String, List<Int>>`. Star
    /// projections (`*`) contribute a default (empty) entry.
    pub arguments: Vec<TypeInfo>,
}

/// A value parameter recovered from `@Metadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamInfo {
    /// Source-level parameter name (e.g. `block`).
    pub name: String,
    /// The parameter's declared type.
    pub ty: TypeInfo,
}

/// A function recovered from `@Metadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionInfo {
    pub name: String,
    pub value_params: Vec<ParamInfo>,
    /// Declared return type, when present inline (`Function.return_type`).
    pub return_type: Option<TypeInfo>,
    /// Extension receiver type for `fun T.foo()` (`Function.receiver_type`).
    pub receiver_type: Option<TypeInfo>,
}

/// The functions of a Kotlin class / file facade, recovered from its
/// `@Metadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassMetadata {
    pub functions: Vec<FunctionInfo>,
}

/// Parse a [`RawMetadata`] into its declared functions and their
/// parameter shapes. Returns `None` on a malformed/empty payload.
pub fn parse_metadata(raw: &RawMetadata) -> Option<ClassMetadata> {
    let decoded = bit_encoding::decode_bytes(&raw.data1);
    if decoded.is_empty() {
        return None;
    }
    let mut top = Reader::new(&decoded);
    // Leading delimited StringTableTypes, then the Package/Class body.
    let string_table_bytes = top.read_len_bytes()?;
    let body = top.remaining();
    let resolver = parse_string_table(string_table_bytes, raw.data2.clone());

    // Package.function = 3 ; Class.function = 9.
    let function_field = if raw.kind == 1 { 9 } else { 3 };
    let mut functions = Vec::new();
    let mut r = Reader::new(body);
    while let Some((field, wire)) = r.read_tag() {
        if field == function_field && wire == WIRE_LEN {
            if let Some(fn_bytes) = r.read_len_bytes() {
                functions.push(parse_function(fn_bytes, &resolver));
            }
        } else {
            r.skip_field(wire)?;
        }
    }
    Some(ClassMetadata { functions })
}

fn parse_string_table(bytes: &[u8], strings: Vec<String>) -> NameResolver {
    let mut r = Reader::new(bytes);
    let mut raw_records: Vec<Record> = Vec::new();
    while let Some((field, wire)) = r.read_tag() {
        // StringTableTypes.record = 1.
        if field == 1 && wire == WIRE_LEN {
            if let Some(rec) = r.read_len_bytes() {
                raw_records.push(parse_record(rec));
            }
        } else if r.skip_field(wire).is_none() {
            break;
        }
    }
    // Expand each record across the `range` indices it covers.
    let mut records = Vec::new();
    for rec in raw_records {
        for _ in 0..rec.range.max(0) {
            records.push(rec.clone());
        }
    }
    NameResolver { strings, records }
}

fn parse_record(bytes: &[u8]) -> Record {
    let mut rec = Record {
        range: 1,
        ..Default::default()
    };
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.read_tag() {
        match (field, wire) {
            (1, WIRE_VARINT) => rec.range = r.read_varint().unwrap_or(1) as i32,
            (2, WIRE_VARINT) => rec.predefined_index = Some(r.read_varint().unwrap_or(0) as i32),
            (3, WIRE_VARINT) => rec.operation = r.read_varint().unwrap_or(0) as i32,
            // substring_index / replace_char are `packed` repeated int32,
            // but tolerate the non-packed (one varint per tag) form too.
            (4, WIRE_LEN) => rec.substring = read_packed_int32(r.read_len_bytes().unwrap_or(&[])),
            (4, WIRE_VARINT) => rec.substring.push(r.read_varint().unwrap_or(0) as i32),
            (5, WIRE_LEN) => {
                rec.replace_char = read_packed_int32(r.read_len_bytes().unwrap_or(&[]))
            }
            (5, WIRE_VARINT) => rec.replace_char.push(r.read_varint().unwrap_or(0) as i32),
            (6, WIRE_LEN) => {
                rec.string = r
                    .read_len_bytes()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
            }
            (_, w) => {
                if r.skip_field(w).is_none() {
                    break;
                }
            }
        }
    }
    rec
}

fn read_packed_int32(bytes: &[u8]) -> Vec<i32> {
    let mut r = Reader::new(bytes);
    let mut out = Vec::new();
    while !r.is_at_end() {
        match r.read_varint() {
            Some(v) => out.push(v as i32),
            None => break,
        }
    }
    out
}

/// A `ProtoBuf.TypeTable`: types a declaration references by `type_id`
/// instead of inlining. kotlinc moves shared/large types here.
struct TypeTable {
    /// Raw `Type` message bytes, indexed by id.
    types: Vec<Vec<u8>>,
    /// Types at index `>= first_nullable` (when `>= 0`) are implicitly
    /// nullable — a serializer optimisation (`TypeTable` in
    /// `org.jetbrains.kotlin.metadata.deserialization`).
    first_nullable: i32,
}

impl TypeTable {
    fn parse(bytes: &[u8]) -> TypeTable {
        let mut types = Vec::new();
        let mut first_nullable = -1;
        let mut r = Reader::new(bytes);
        while let Some((field, wire)) = r.read_tag() {
            match (field, wire) {
                // TypeTable.type = 1.
                (1, WIRE_LEN) => {
                    if let Some(t) = r.read_len_bytes() {
                        types.push(t.to_vec());
                    }
                }
                // TypeTable.first_nullable = 2.
                (2, WIRE_VARINT) => first_nullable = r.read_varint().unwrap_or(0) as i32,
                (_, w) => {
                    if r.skip_field(w).is_none() {
                        break;
                    }
                }
            }
        }
        TypeTable {
            types,
            first_nullable,
        }
    }

    fn implicitly_nullable(&self, id: usize) -> bool {
        self.first_nullable >= 0 && id >= self.first_nullable as usize
    }
}

/// A type reference: either inlined bytes, or an index into the
/// enclosing declaration's [`TypeTable`].
enum TypeRef<'a> {
    Inline(&'a [u8]),
    Id(usize),
}

/// Cap on `Type` nesting / `TypeTable` chasing, guarding against
/// pathological or cyclic references in untrusted `.class` files.
const MAX_TYPE_DEPTH: u32 = 24;

fn resolve_type(r: TypeRef, nr: &NameResolver, table: Option<&TypeTable>, depth: u32) -> TypeInfo {
    if depth >= MAX_TYPE_DEPTH {
        return TypeInfo::default();
    }
    match r {
        TypeRef::Inline(b) => parse_type(b, nr, table, depth),
        TypeRef::Id(id) => match table.and_then(|t| t.types.get(id).map(|b| (t, b))) {
            Some((t, bytes)) => {
                let mut info = parse_type(bytes, nr, table, depth);
                if t.implicitly_nullable(id) {
                    info.nullable = true;
                }
                info
            }
            None => TypeInfo::default(),
        },
    }
}

fn parse_function(bytes: &[u8], nr: &NameResolver) -> FunctionInfo {
    let mut name = String::new();
    let mut param_raws: Vec<&[u8]> = Vec::new();
    let mut return_ref: Option<TypeRef> = None;
    let mut receiver_ref: Option<TypeRef> = None;
    let mut table: Option<TypeTable> = None;
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.read_tag() {
        match (field, wire) {
            // Function.name = 2.
            (2, WIRE_VARINT) => name = nr.get_string(r.read_varint().unwrap_or(0) as usize),
            // Function.return_type = 3 (inline) / return_type_id = 7.
            (3, WIRE_LEN) => return_ref = r.read_len_bytes().map(TypeRef::Inline),
            (7, WIRE_VARINT) => {
                return_ref = Some(TypeRef::Id(r.read_varint().unwrap_or(0) as usize))
            }
            // Function.receiver_type = 5 (inline) / receiver_type_id = 8.
            (5, WIRE_LEN) => receiver_ref = r.read_len_bytes().map(TypeRef::Inline),
            (8, WIRE_VARINT) => {
                receiver_ref = Some(TypeRef::Id(r.read_varint().unwrap_or(0) as usize))
            }
            // Function.value_parameter = 6.
            (6, WIRE_LEN) => {
                if let Some(vp) = r.read_len_bytes() {
                    param_raws.push(vp);
                }
            }
            // Function.type_table = 30.
            (30, WIRE_LEN) => table = r.read_len_bytes().map(TypeTable::parse),
            (_, w) => {
                if r.skip_field(w).is_none() {
                    break;
                }
            }
        }
    }
    let t = table.as_ref();
    FunctionInfo {
        name,
        value_params: param_raws
            .into_iter()
            .map(|vp| parse_value_parameter(vp, nr, t))
            .collect(),
        return_type: return_ref.map(|rf| resolve_type(rf, nr, t, 0)),
        receiver_type: receiver_ref.map(|rf| resolve_type(rf, nr, t, 0)),
    }
}

fn parse_value_parameter(bytes: &[u8], nr: &NameResolver, table: Option<&TypeTable>) -> ParamInfo {
    let mut name = String::new();
    let mut type_ref: Option<TypeRef> = None;
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.read_tag() {
        match (field, wire) {
            // ValueParameter.name = 2.
            (2, WIRE_VARINT) => name = nr.get_string(r.read_varint().unwrap_or(0) as usize),
            // ValueParameter.type = 3 (inline) / type_id = 5.
            (3, WIRE_LEN) => type_ref = r.read_len_bytes().map(TypeRef::Inline),
            (5, WIRE_VARINT) => type_ref = Some(TypeRef::Id(r.read_varint().unwrap_or(0) as usize)),
            (_, w) => {
                if r.skip_field(w).is_none() {
                    break;
                }
            }
        }
    }
    let ty = type_ref
        .map(|rf| resolve_type(rf, nr, table, 0))
        .unwrap_or_default();
    ParamInfo { name, ty }
}

/// Parse a `ProtoBuf.Type` into the reduced [`TypeInfo`], following
/// generic arguments and `TypeTable` references.
fn parse_type(bytes: &[u8], nr: &NameResolver, table: Option<&TypeTable>, depth: u32) -> TypeInfo {
    let mut info = TypeInfo::default();
    if depth >= MAX_TYPE_DEPTH {
        return info;
    }
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.read_tag() {
        match (field, wire) {
            // Type.argument = 2 (repeated Argument).
            (2, WIRE_LEN) => {
                if let Some(arg) = r.read_len_bytes() {
                    info.arguments
                        .push(parse_type_argument(arg, nr, table, depth + 1));
                }
            }
            // Type.nullable = 3.
            (3, WIRE_VARINT) => info.nullable = r.read_varint().unwrap_or(0) != 0,
            // Type.class_name = 6 (fq-class index).
            (6, WIRE_VARINT) => {
                info.class_name = Some(nr.get_string(r.read_varint().unwrap_or(0) as usize))
            }
            // Type.annotation = 100 → kotlin/ExtensionFunctionType marks
            // a receiver function type (`T.() -> R`).
            (100, WIRE_LEN) => {
                if let Some(ann) = r.read_len_bytes() {
                    if annotation_id(ann, nr).as_deref() == Some("kotlin/ExtensionFunctionType") {
                        info.is_extension_function_type = true;
                    }
                }
            }
            (_, w) => {
                if r.skip_field(w).is_none() {
                    break;
                }
            }
        }
    }
    info
}

/// Parse a `Type.Argument` (one generic arg). A star projection has no
/// type and yields a default [`TypeInfo`].
fn parse_type_argument(
    bytes: &[u8],
    nr: &NameResolver,
    table: Option<&TypeTable>,
    depth: u32,
) -> TypeInfo {
    let mut type_ref: Option<TypeRef> = None;
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.read_tag() {
        match (field, wire) {
            // Argument.type = 2 (inline) / type_id = 3.
            (2, WIRE_LEN) => type_ref = r.read_len_bytes().map(TypeRef::Inline),
            (3, WIRE_VARINT) => type_ref = Some(TypeRef::Id(r.read_varint().unwrap_or(0) as usize)),
            (_, w) => {
                if r.skip_field(w).is_none() {
                    break;
                }
            }
        }
    }
    type_ref
        .map(|rf| resolve_type(rf, nr, table, depth))
        .unwrap_or_default()
}

/// Resolve `Annotation.id = 1` (an fq-class index) to its name.
fn annotation_id(bytes: &[u8], nr: &NameResolver) -> Option<String> {
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.read_tag() {
        if field == 1 && wire == WIRE_VARINT {
            return Some(nr.get_string(r.read_varint()? as usize));
        }
        r.skip_field(wire)?;
    }
    None
}
