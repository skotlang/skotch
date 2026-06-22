//! A tiny DER builder sufficient to emit the PKCS#7 SignedData used for v1
//! JAR signatures, matching apksig's `Asn1DerEncoder` output byte-for-byte.
//!
//! Only the constructs apksig actually emits are supported: INTEGER (from a
//! big-endian magnitude with sign handling), OBJECT IDENTIFIER (from a dotted
//! string), NULL, OCTET STRING, SEQUENCE, SET / SET OF (with apksig's
//! length-then-lexicographic element ordering), and IMPLICIT/EXPLICIT
//! context tags. Already-encoded blobs (certificates, the issuer Name,
//! signatures) are spliced in opaque.

/// Encodes a DER length in definite form.
fn encode_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = (len as u64).to_be_bytes();
        let first = bytes.iter().position(|&b| b != 0).unwrap();
        let significant = &bytes[first..];
        out.push(0x80 | significant.len() as u8);
        out.extend_from_slice(significant);
    }
}

/// Wraps `content` in a TLV with the given identifier octet.
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 4);
    out.push(tag);
    encode_len(content.len(), &mut out);
    out.extend_from_slice(content);
    out
}

pub const TAG_INTEGER: u8 = 0x02;
pub const TAG_OCTET_STRING: u8 = 0x04;
pub const TAG_NULL: u8 = 0x05;
pub const TAG_OID: u8 = 0x06;
pub const TAG_SEQUENCE: u8 = 0x30;
pub const TAG_SET: u8 = 0x31;

/// DER NULL.
pub fn null() -> Vec<u8> {
    vec![TAG_NULL, 0x00]
}

/// INTEGER from a non-negative `u32`.
pub fn integer_u32(v: u32) -> Vec<u8> {
    let bytes = v.to_be_bytes();
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(3);
    let mut mag = bytes[first..].to_vec();
    if mag[0] & 0x80 != 0 {
        mag.insert(0, 0);
    }
    tlv(TAG_INTEGER, &mag)
}

/// INTEGER from a big-endian two's-complement magnitude (already in the form
/// produced by `BigInteger.toByteArray()`), spliced verbatim. Used for the
/// certificate serial number.
pub fn integer_from_be_twos_complement(be: &[u8]) -> Vec<u8> {
    tlv(TAG_INTEGER, be)
}

/// OBJECT IDENTIFIER from a dotted-decimal string.
pub fn oid(dotted: &str) -> Vec<u8> {
    let parts: Vec<u64> = dotted.split('.').map(|p| p.parse().unwrap()).collect();
    let mut body = Vec::new();
    body.push((parts[0] * 40 + parts[1]) as u8);
    for &part in &parts[2..] {
        encode_base128(part, &mut body);
    }
    tlv(TAG_OID, &body)
}

fn encode_base128(mut v: u64, out: &mut Vec<u8>) {
    let mut stack = [0u8; 10];
    let mut n = 0;
    stack[n] = (v & 0x7f) as u8;
    n += 1;
    v >>= 7;
    while v > 0 {
        stack[n] = ((v & 0x7f) as u8) | 0x80;
        n += 1;
        v >>= 7;
    }
    for i in (0..n).rev() {
        out.push(stack[i]);
    }
}

pub fn octet_string(content: &[u8]) -> Vec<u8> {
    tlv(TAG_OCTET_STRING, content)
}

pub fn sequence(elements: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    for e in elements {
        body.extend_from_slice(e);
    }
    tlv(TAG_SEQUENCE, &body)
}

/// SET OF with apksig's element ordering: sort serialized elements by the
/// length-then-lexicographic comparator (`ByteArrayLexicographicComparator`).
pub fn set_of(mut elements: Vec<Vec<u8>>) -> Vec<u8> {
    if elements.len() > 1 {
        elements.sort_by(|a, b| cmp_apksig(a, b));
    }
    let mut body = Vec::new();
    for e in &elements {
        body.extend_from_slice(e);
    }
    tlv(TAG_SET, &body)
}

/// SET (no reordering — used when there is exactly one element or order is
/// already canonical).
pub fn set_unordered(elements: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    for e in elements {
        body.extend_from_slice(e);
    }
    tlv(TAG_SET, &body)
}

/// apksig's `ByteArrayLexicographicComparator`: compare common prefix by
/// unsigned byte value, then shorter array sorts first.
fn cmp_apksig(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let common = a.len().min(b.len());
    for i in 0..common {
        match a[i].cmp(&b[i]) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }
    }
    a.len().cmp(&b.len())
}

/// EXPLICIT context tag: `[n] EXPLICIT` constructed, wrapping the full TLV.
pub fn explicit_context(tag_number: u8, content: &[u8]) -> Vec<u8> {
    tlv(0xa0 | (tag_number & 0x1f), content)
}

/// IMPLICIT context tag, constructed (e.g. `[0] IMPLICIT SET OF`): replaces
/// the underlying constructed tag's identifier with `[n]`.
pub fn implicit_constructed(tag_number: u8, content: &[u8]) -> Vec<u8> {
    tlv(0xa0 | (tag_number & 0x1f), content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oid_sha256() {
        // 2.16.840.1.101.3.4.2.1
        assert_eq!(
            oid("2.16.840.1.101.3.4.2.1"),
            vec![0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01]
        );
    }

    #[test]
    fn oid_rsa() {
        // 1.2.840.113549.1.1.1
        assert_eq!(
            oid("1.2.840.113549.1.1.1"),
            vec![0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]
        );
    }

    #[test]
    fn integer_high_bit_gets_zero_pad() {
        assert_eq!(integer_u32(0x80), vec![0x02, 0x02, 0x00, 0x80]);
        assert_eq!(integer_u32(1), vec![0x02, 0x01, 0x01]);
    }

    #[test]
    fn long_length_form() {
        let content = vec![0u8; 200];
        let encoded = tlv(TAG_OCTET_STRING, &content);
        assert_eq!(&encoded[..3], &[0x04, 0x81, 200]);
    }
}
