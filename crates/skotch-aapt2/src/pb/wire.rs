//! Minimal protobuf (proto3) wire-format reader and writer.
//!
//! Hand-rolled to keep skotch dependency-free: only the message shapes
//! defined in aapt2's `Resources.proto` / `ResourcesInternal.proto` are
//! needed, and they use nothing beyond varint, fixed32, and
//! length-delimited fields.
//!
//! Encoding matches C++ protobuf conventions so emitted containers are
//! compatible with the real aapt2: fields are written in ascending
//! field-number order, zero-valued scalars are omitted, and `oneof`
//! members are written when set even if zero-valued.

/// Protobuf wire types.
pub const WIRE_VARINT: u32 = 0;
pub const WIRE_FIXED64: u32 = 1;
pub const WIRE_LEN: u32 = 2;
pub const WIRE_FIXED32: u32 = 5;

// ───────────────────────────── writer ─────────────────────────────

/// Append-only protobuf writer.
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn write_raw_varint(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(byte);
                break;
            }
            self.buf.push(byte | 0x80);
        }
    }

    fn tag(&mut self, field: u32, wire_type: u32) {
        self.write_raw_varint(((field << 3) | wire_type) as u64);
    }

    /// Writes a varint field, omitting it when zero (proto3 default).
    pub fn varint(&mut self, field: u32, v: u64) {
        if v != 0 {
            self.varint_always(field, v);
        }
    }

    /// Writes a varint field even when zero (for `oneof` members).
    pub fn varint_always(&mut self, field: u32, v: u64) {
        self.tag(field, WIRE_VARINT);
        self.write_raw_varint(v);
    }

    /// Writes an `int32` field using protobuf's sign-extended encoding.
    pub fn int32(&mut self, field: u32, v: i32) {
        if v != 0 {
            self.int32_always(field, v);
        }
    }

    pub fn int32_always(&mut self, field: u32, v: i32) {
        self.tag(field, WIRE_VARINT);
        self.write_raw_varint(v as i64 as u64);
    }

    pub fn bool(&mut self, field: u32, v: bool) {
        if v {
            self.varint_always(field, 1);
        }
    }

    pub fn bool_always(&mut self, field: u32, v: bool) {
        self.varint_always(field, v as u64);
    }

    pub fn fixed32(&mut self, field: u32, v: u32) {
        self.tag(field, WIRE_FIXED32);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn float(&mut self, field: u32, v: f32) {
        if v != 0.0 || v.is_sign_negative() {
            self.float_always(field, v);
        }
    }

    pub fn float_always(&mut self, field: u32, v: f32) {
        self.fixed32(field, v.to_bits());
    }

    pub fn string(&mut self, field: u32, v: &str) {
        if !v.is_empty() {
            self.string_always(field, v);
        }
    }

    pub fn string_always(&mut self, field: u32, v: &str) {
        self.bytes_always(field, v.as_bytes());
    }

    pub fn bytes(&mut self, field: u32, v: &[u8]) {
        if !v.is_empty() {
            self.bytes_always(field, v);
        }
    }

    pub fn bytes_always(&mut self, field: u32, v: &[u8]) {
        self.tag(field, WIRE_LEN);
        self.write_raw_varint(v.len() as u64);
        self.buf.extend_from_slice(v);
    }

    /// Writes an embedded message field. The message is encoded by `f`
    /// into a scratch writer to learn its length.
    pub fn message(&mut self, field: u32, f: impl FnOnce(&mut Writer)) {
        let mut inner = Writer::new();
        f(&mut inner);
        self.bytes_always(field, &inner.buf);
    }
}

// ───────────────────────────── reader ─────────────────────────────

/// A field encountered while walking a message.
pub struct Field<'a> {
    pub number: u32,
    pub wire_type: u32,
    /// For `WIRE_LEN`: the payload. For other types: empty.
    pub data: &'a [u8],
    /// For `WIRE_VARINT`/`WIRE_FIXED32`/`WIRE_FIXED64`: the value.
    pub value: u64,
}

impl<'a> Field<'a> {
    pub fn as_u32(&self) -> u32 {
        self.value as u32
    }

    pub fn as_i32(&self) -> i32 {
        self.value as i64 as i32
    }

    pub fn as_bool(&self) -> bool {
        self.value != 0
    }

    pub fn as_f32(&self) -> f32 {
        f32::from_bits(self.value as u32)
    }

    pub fn as_str(&self) -> &'a str {
        std::str::from_utf8(self.data).unwrap_or("")
    }

    pub fn as_string(&self) -> String {
        String::from_utf8_lossy(self.data).into_owned()
    }
}

/// Reads protobuf messages from a byte slice.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn read_raw_varint(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            if shift >= 64 {
                return None;
            }
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
        }
    }

    /// Reads the next field, or `None` at end of input / on malformed
    /// data.
    pub fn next_field(&mut self) -> Option<Field<'a>> {
        if self.is_empty() {
            return None;
        }
        let key = self.read_raw_varint()?;
        let number = (key >> 3) as u32;
        let wire_type = (key & 0x7) as u32;
        match wire_type {
            WIRE_VARINT => {
                let value = self.read_raw_varint()?;
                Some(Field {
                    number,
                    wire_type,
                    data: &[],
                    value,
                })
            }
            WIRE_FIXED64 => {
                let bytes = self.data.get(self.pos..self.pos + 8)?;
                self.pos += 8;
                Some(Field {
                    number,
                    wire_type,
                    data: &[],
                    value: u64::from_le_bytes(bytes.try_into().ok()?),
                })
            }
            WIRE_LEN => {
                let len = self.read_raw_varint()? as usize;
                let data = self.data.get(self.pos..self.pos + len)?;
                self.pos += len;
                Some(Field {
                    number,
                    wire_type,
                    data,
                    value: 0,
                })
            }
            WIRE_FIXED32 => {
                let bytes = self.data.get(self.pos..self.pos + 4)?;
                self.pos += 4;
                Some(Field {
                    number,
                    wire_type,
                    data: &[],
                    value: u32::from_le_bytes(bytes.try_into().ok()?) as u64,
                })
            }
            _ => None,
        }
    }
}

/// Walks every field of `data`, calling `f` for each. Unknown fields are
/// skipped for free since `f` simply ignores them.
pub fn for_each_field<'a>(data: &'a [u8], mut f: impl FnMut(&Field<'a>)) {
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        f(&field);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip() {
        let mut w = Writer::new();
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            w.write_raw_varint(v);
        }
        let mut r = Reader::new(&w.buf);
        for expected in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            assert_eq!(r.read_raw_varint(), Some(expected));
        }
    }

    #[test]
    fn fields_round_trip() {
        let mut w = Writer::new();
        w.varint(1, 42);
        w.string(2, "hello");
        w.float_always(3, 1.5);
        w.int32(4, -7);
        w.message(5, |inner| inner.varint(1, 9));

        let mut seen = Vec::new();
        for_each_field(&w.buf, |f| seen.push((f.number, f.wire_type)));
        assert_eq!(
            seen,
            vec![
                (1, WIRE_VARINT),
                (2, WIRE_LEN),
                (3, WIRE_FIXED32),
                (4, WIRE_VARINT),
                (5, WIRE_LEN)
            ]
        );

        let mut r = Reader::new(&w.buf);
        assert_eq!(r.next_field().unwrap().value, 42);
        assert_eq!(r.next_field().unwrap().as_str(), "hello");
        assert_eq!(r.next_field().unwrap().as_f32(), 1.5);
        assert_eq!(r.next_field().unwrap().as_i32(), -7);
        let msg = r.next_field().unwrap();
        let mut inner = Reader::new(msg.data);
        assert_eq!(inner.next_field().unwrap().value, 9);
    }

    #[test]
    fn zero_scalars_omitted() {
        let mut w = Writer::new();
        w.varint(1, 0);
        w.string(2, "");
        w.bool(3, false);
        assert!(w.buf.is_empty());
        w.varint_always(1, 0);
        assert_eq!(w.buf, vec![0x08, 0x00]);
    }
}
