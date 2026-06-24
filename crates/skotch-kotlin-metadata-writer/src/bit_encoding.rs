//! `@kotlin.Metadata.d1` codec (writer side).
//!
//! Port of `org.jetbrains.kotlin.metadata.jvm.serialization.BitEncoding`
//! / `utfEncoding.kt`. Mirror of the decoder at
//! [`skotch_classinfo::kotlin_metadata::bit_encoding`] — the decoder is
//! the source-of-truth for the on-disk format, and the round-trip test
//! below feeds this encoder's output back through that decoder.
//!
//! Two encodings exist; we emit the modern **UTF-8 mode** by default.
//! In UTF-8 mode each output string is preceded by a `'\u{0000}'` mode
//! marker (only in the first string), and every other character carries
//! one byte of the raw protobuf payload in its low 8 bits.
//!
//! kotlinc splits the payload across multiple strings to dodge the JVM
//! `CONSTANT_Utf8_info` length limit (65 535 modified-UTF-8 bytes per
//! entry). We mirror that splitting; the boundary is conservative,
//! since any high byte expands from one source byte to two modified-
//! UTF-8 bytes in the constant pool.

/// Mode marker that precedes the payload in UTF-8 mode.
///
/// This MUST match
/// [`skotch_classinfo::kotlin_metadata::bit_encoding`]'s decoder, which
/// dispatches on the first character of `d1[0]`.
pub(crate) const UTF8_MODE_MARKER: char = '\u{0}';

/// Conservative upper bound on the number of bytes packed into one
/// output `String`. The JVM constant-pool limit is 65 535 *modified*
/// UTF-8 bytes; high (0x80..=0xFF) source bytes expand to 2 modified
/// UTF-8 bytes (and the NUL marker, if any, expands to 2 as well).
/// Splitting at 32 KiB of source bytes guarantees every chunk fits
/// regardless of byte distribution.
const MAX_BYTES_PER_STRING: usize = 32 * 1024;

/// Encode a raw protobuf byte stream as the `@Metadata.d1` UTF-8-mode
/// `String[]`. Round-trips through
/// [`skotch_classinfo::kotlin_metadata::bit_encoding::decode_bytes`] —
/// see `tests::round_trip_via_decoder`.
pub fn encode_bytes(bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        // Empty payload: a single string carrying only the mode marker.
        // The decoder's `strings_to_bytes` on this yields the empty
        // byte vector (the leading marker char is dropped first).
        return vec![UTF8_MODE_MARKER.to_string()];
    }

    let mut out: Vec<String> = Vec::new();
    let mut first = true;
    for chunk in bytes.chunks(MAX_BYTES_PER_STRING) {
        // Allocate worst-case (marker + one char per byte). Capacity is
        // in bytes of the final Rust `String`'s UTF-8, which for our
        // `char::from_u32(b as u32)` outputs is at most 2 per source
        // byte (every char is in 0x00..=0xFF).
        let mut s = String::with_capacity(1 + chunk.len() * 2);
        if first {
            s.push(UTF8_MODE_MARKER);
            first = false;
        }
        for &b in chunk {
            // `char::from_u32` cannot fail for values in 0..=0xFF
            // (no surrogates in that range).
            s.push(char::from_u32(b as u32).expect("byte → char in 0..=0xFF"));
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_classinfo::kotlin_metadata::bit_encoding::decode_bytes;

    /// The decoder treats `'\u{0000}'` as a UTF-8-mode marker only when
    /// it is the first character of the FIRST string. Verify our
    /// encoder places it correctly.
    #[test]
    fn first_char_is_utf8_marker() {
        let encoded = encode_bytes(b"hello");
        assert!(!encoded.is_empty());
        assert_eq!(encoded[0].chars().next(), Some(UTF8_MODE_MARKER));
    }

    #[test]
    fn empty_payload_is_single_marker_string() {
        let encoded = encode_bytes(&[]);
        assert_eq!(encoded, vec![UTF8_MODE_MARKER.to_string()]);
        // Round-trip via the decoder.
        assert_eq!(decode_bytes(&encoded), Vec::<u8>::new());
    }

    fn sample_payloads() -> Vec<Vec<u8>> {
        vec![
            vec![],
            vec![0x00],
            vec![0x7f],
            vec![0xff],
            vec![0x00, 0x00, 0x00],
            (1u8..=10).collect(),
            (0u16..=255).map(|b| b as u8).collect(),
            // A scrap that looks like a tiny protobuf message
            // (field 1 length-delim "listOf", field 2 length-delim "").
            b"\x0a\x06listOf\x12\x00".to_vec(),
        ]
    }

    #[test]
    fn round_trip_via_decoder() {
        for payload in sample_payloads() {
            let encoded = encode_bytes(&payload);
            let decoded = decode_bytes(&encoded);
            assert_eq!(decoded, payload, "round-trip failed for {payload:?}");
        }
    }

    /// Drive a payload large enough to exceed `MAX_BYTES_PER_STRING`,
    /// forcing the encoder to emit multiple `String`s.
    #[test]
    fn round_trip_multi_string_payload() {
        let payload: Vec<u8> = (0..(MAX_BYTES_PER_STRING * 2 + 17))
            .map(|i| (i % 256) as u8)
            .collect();
        let encoded = encode_bytes(&payload);
        assert!(
            encoded.len() >= 3,
            "expected ≥3 chunks for >2× MAX, got {}",
            encoded.len()
        );
        // Only the first chunk must start with the UTF-8-mode marker;
        // subsequent chunks carry payload bytes verbatim (any `'\u{0}'`
        // there is a payload byte, NOT a marker — the decoder only
        // inspects the first character of the first string).
        assert_eq!(
            encoded[0].chars().next(),
            Some(UTF8_MODE_MARKER),
            "first chunk must carry marker"
        );
        assert_eq!(decode_bytes(&encoded), payload);
    }
}
