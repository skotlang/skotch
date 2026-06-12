//! Modified UTF-8 (MUTF-8) used for DEX string data, and UTF-16 ordering used
//! for sorting the string pool exactly as the DEX format requires.

/// Encodes a Rust `&str` as MUTF-8 (the encoding DEX strings use): the NUL
/// character is encoded as two bytes and supplementary characters as a
/// surrogate pair, each surrogate as 3 bytes (CESU-8 style).
pub fn encode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 1);
    for c in s.chars() {
        let cp = c as u32;
        if cp == 0 {
            out.push(0xc0);
            out.push(0x80);
        } else if cp < 0x80 {
            out.push(cp as u8);
        } else if cp < 0x800 {
            out.push(0xc0 | (cp >> 6) as u8);
            out.push(0x80 | (cp & 0x3f) as u8);
        } else if cp < 0x10000 {
            out.push(0xe0 | (cp >> 12) as u8);
            out.push(0x80 | ((cp >> 6) & 0x3f) as u8);
            out.push(0x80 | (cp & 0x3f) as u8);
        } else {
            // Supplementary: encode as a UTF-16 surrogate pair, each surrogate
            // as a 3-byte sequence.
            let v = cp - 0x10000;
            let hi = 0xd800 + (v >> 10);
            let lo = 0xdc00 + (v & 0x3ff);
            for surrogate in [hi, lo] {
                out.push(0xe0 | (surrogate >> 12) as u8);
                out.push(0x80 | ((surrogate >> 6) & 0x3f) as u8);
                out.push(0x80 | (surrogate & 0x3f) as u8);
            }
        }
    }
    out
}

/// Decodes MUTF-8 bytes (terminated by an implicit length, no trailing NUL) to
/// a Rust `String`. Surrogate pairs are recombined.
pub fn decode(bytes: &[u8]) -> String {
    // First decode to UTF-16 code units, then to chars (handling surrogates).
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i];
        if a & 0x80 == 0 {
            units.push(a as u16);
            i += 1;
        } else if a & 0xe0 == 0xc0 {
            let b = bytes[i + 1];
            units.push((((a & 0x1f) as u16) << 6) | ((b & 0x3f) as u16));
            i += 2;
        } else {
            let b = bytes[i + 1];
            let c = bytes[i + 2];
            units.push((((a & 0x0f) as u16) << 12) | (((b & 0x3f) as u16) << 6) | ((c & 0x3f) as u16));
            i += 3;
        }
    }
    String::from_utf16_lossy(&units)
}

/// The number of UTF-16 code units in `s` (the `utf16_size` stored before DEX
/// string data).
pub fn utf16_units(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

/// Compares two strings by their UTF-16 code-unit sequence, which is the order
/// the DEX string pool must be sorted in. For the BMP this equals char order;
/// supplementary characters compare via their surrogate pairs (so they sort
/// after the BMP, matching the DEX/Java `String.compareTo` contract used by d8).
pub fn cmp_utf16(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.encode_utf16();
    let mut bi = b.encode_utf16();
    loop {
        match (ai.next(), bi.next()) {
            (Some(x), Some(y)) => match x.cmp(&y) {
                std::cmp::Ordering::Equal => continue,
                ord => return ord,
            },
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (None, None) => return std::cmp::Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_roundtrip() {
        for s in ["<init>", "LEmpty;", "Ljava/lang/Object;", "V", "()V"] {
            let e = encode(s);
            assert_eq!(decode(&e), s);
            assert_eq!(utf16_units(s), s.len() as u32);
        }
    }

    #[test]
    fn nul_is_two_bytes() {
        assert_eq!(encode("\0"), vec![0xc0, 0x80]);
        assert_eq!(decode(&[0xc0, 0x80]), "\0");
    }

    #[test]
    fn supplementary_surrogate_pair() {
        let s = "\u{1F600}"; // emoji, supplementary plane
        let e = encode(s);
        assert_eq!(e.len(), 6); // two 3-byte surrogates
        assert_eq!(decode(&e), s);
        assert_eq!(utf16_units(s), 2);
    }

    #[test]
    fn sort_order() {
        let mut v = vec!["V", "<init>", "Ljava/lang/Object;", "LEmpty;", "Empty.java"];
        v.sort_by(|a, b| cmp_utf16(a, b));
        assert_eq!(
            v,
            vec!["<init>", "Empty.java", "LEmpty;", "Ljava/lang/Object;", "V"]
        );
    }
}
