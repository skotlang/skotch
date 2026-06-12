//! LEB128 variable-length integer encoding used throughout the DEX format.

/// Appends an unsigned LEB128 value.
pub fn write_uleb128(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Appends a signed LEB128 value.
pub fn write_sleb128(out: &mut Vec<u8>, mut value: i32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let sign_bit = byte & 0x40;
        let done = (value == 0 && sign_bit == 0) || (value == -1 && sign_bit != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

/// Appends `uleb128p1`: the value plus one, so `-1` encodes as `0`.
pub fn write_uleb128p1(out: &mut Vec<u8>, value: i32) {
    write_uleb128(out, (value.wrapping_add(1)) as u32);
}

/// Reads an unsigned LEB128 value, returning `(value, bytes_consumed)`.
pub fn read_uleb128(data: &[u8], mut pos: usize) -> (u32, usize) {
    let start = pos;
    let mut result: u32 = 0;
    let mut shift = 0;
    loop {
        let byte = data[pos];
        pos += 1;
        result |= ((byte & 0x7f) as u32) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    (result, pos - start)
}

/// Reads a signed LEB128 value, returning `(value, bytes_consumed)`.
pub fn read_sleb128(data: &[u8], mut pos: usize) -> (i32, usize) {
    let start = pos;
    let mut result: i32 = 0;
    let mut shift = 0;
    let mut byte;
    loop {
        byte = data[pos];
        pos += 1;
        result |= ((byte & 0x7f) as i32) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
    }
    if shift < 32 && (byte & 0x40) != 0 {
        result |= -(1i32 << shift);
    }
    (result, pos - start)
}

/// Reads a `uleb128p1` value.
pub fn read_uleb128p1(data: &[u8], pos: usize) -> (i32, usize) {
    let (v, n) = read_uleb128(data, pos);
    ((v as i32).wrapping_sub(1), n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_u(v: u32) {
        let mut out = Vec::new();
        write_uleb128(&mut out, v);
        assert_eq!(read_uleb128(&out, 0), (v, out.len()));
    }
    fn roundtrip_s(v: i32) {
        let mut out = Vec::new();
        write_sleb128(&mut out, v);
        assert_eq!(read_sleb128(&out, 0), (v, out.len()));
    }

    #[test]
    fn uleb() {
        for v in [0u32, 1, 0x7f, 0x80, 0x3fff, 0x4000, 0xd0, 0x10001, u32::MAX] {
            roundtrip_u(v);
        }
        // Known DEX encodings.
        let mut out = Vec::new();
        write_uleb128(&mut out, 0xd0);
        assert_eq!(out, vec![0xd0, 0x01]);
        out.clear();
        write_uleb128(&mut out, 0x10001);
        assert_eq!(out, vec![0x81, 0x80, 0x04]);
    }

    #[test]
    fn sleb() {
        for v in [0i32, 1, -1, 63, 64, -64, -65, i32::MIN, i32::MAX] {
            roundtrip_s(v);
        }
    }

    #[test]
    fn uleb_p1() {
        let mut out = Vec::new();
        write_uleb128p1(&mut out, -1);
        assert_eq!(out, vec![0x00]);
        assert_eq!(read_uleb128p1(&out, 0), (-1, 1));
        out.clear();
        write_uleb128p1(&mut out, 5);
        assert_eq!(read_uleb128p1(&out, 0), (5, out.len()));
    }
}
