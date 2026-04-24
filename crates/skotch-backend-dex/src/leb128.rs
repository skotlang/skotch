//! LEB128 (Little-Endian Base 128) encoders for DEX.
//!
//! DEX uses unsigned LEB128 (`uleb128`) for sizes, indexes, and
//! method/field offsets in `class_data_item`s. Signed LEB128
//! (`sleb128`) is used in encoded values and debug info; we don't
//! currently emit either of those, but the encoder is here for
//! completeness.

/// Encode `value` as uleb128 and append the bytes to `out`.
pub fn write_uleb128(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
            out.push(byte);
        } else {
            out.push(byte);
            return;
        }
    }
}

/// Encode `value` as sleb128 and append the bytes to `out`.
#[allow(dead_code)]
pub fn write_sleb128(out: &mut Vec<u8>, mut value: i32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let sign = byte & 0x40;
        let done = (value == 0 && sign == 0) || (value == -1 && sign != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(v: u32) -> Vec<u8> {
        let mut out = Vec::new();
        write_uleb128(&mut out, v);
        out
    }

    #[test]
    fn uleb128_zero() {
        assert_eq!(enc(0), vec![0x00]);
    }

    #[test]
    fn uleb128_one() {
        assert_eq!(enc(1), vec![0x01]);
    }

    #[test]
    fn uleb128_127() {
        assert_eq!(enc(127), vec![0x7f]);
    }

    #[test]
    fn uleb128_128() {
        assert_eq!(enc(128), vec![0x80, 0x01]);
    }

    #[test]
    fn uleb128_300() {
        assert_eq!(enc(300), vec![0xac, 0x02]);
    }

    #[test]
    fn uleb128_max_u32() {
        assert_eq!(enc(u32::MAX), vec![0xff, 0xff, 0xff, 0xff, 0x0f]);
    }

    #[test]
    fn sleb128_zero() {
        let mut out = Vec::new();
        write_sleb128(&mut out, 0);
        assert_eq!(out, vec![0x00]);
    }

    #[test]
    fn sleb128_minus_one() {
        let mut out = Vec::new();
        write_sleb128(&mut out, -1);
        assert_eq!(out, vec![0x7f]);
    }
}
