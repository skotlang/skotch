//! JVM constant pool builder.
//!
//! Per JVMS chapter 4 the constant pool is **1-indexed**: index 0 is
//! reserved as a sentinel. `Long` and `Double` entries take *two*
//! consecutive slots (the second one is unusable). For PR #1 we never
//! emit either, but the slot-counting logic is in place so future PRs
//! can.
//!
//! Entries are deduplicated by structural key — emitting the same
//! `Utf8`/`Class`/`Methodref` twice yields the same index. This isn't
//! required by the JVM but matches what `kotlinc` produces, which
//! makes byte-level diffing of golden files much less noisy.

use byteorder::{BigEndian, WriteBytesExt};
use rustc_hash::FxHashMap;
use std::io::Write;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Utf8(String),
    Integer(i32),
    Long(i64),
    Double(u64), // f64 bits for hashing
    Class(u16),
    String(u16),
    NameAndType(u16, u16),
    Fieldref(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
}

#[derive(Clone, Debug)]
enum Entry {
    Utf8(String),
    Integer(i32),
    Long(i64),
    Double(f64),
    Class(u16),
    String(u16),
    NameAndType(u16, u16),
    Fieldref(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
    /// Placeholder for the second slot of Double/Long entries.
    WideSlot,
}

#[derive(Default)]
pub struct ConstantPool {
    entries: Vec<Entry>,
    dedupe: FxHashMap<Key, u16>,
}

impl ConstantPool {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(&mut self, key: Key, entry: Entry) -> u16 {
        if let Some(&idx) = self.dedupe.get(&key) {
            return idx;
        }
        let idx = (self.entries.len() + 1) as u16;
        let is_wide = matches!(entry, Entry::Double(_) | Entry::Long(_));
        self.entries.push(entry);
        self.dedupe.insert(key, idx);
        // Double (and Long) entries occupy two constant pool slots.
        if is_wide {
            self.entries.push(Entry::WideSlot);
        }
        idx
    }

    pub fn utf8(&mut self, s: &str) -> u16 {
        self.add(Key::Utf8(s.to_string()), Entry::Utf8(s.to_string()))
    }

    pub fn integer(&mut self, v: i32) -> u16 {
        self.add(Key::Integer(v), Entry::Integer(v))
    }

    pub fn long(&mut self, v: i64) -> u16 {
        self.add(Key::Long(v), Entry::Long(v))
    }

    pub fn double(&mut self, v: f64) -> u16 {
        self.add(Key::Double(v.to_bits()), Entry::Double(v))
    }

    pub fn class(&mut self, internal_name: &str) -> u16 {
        let name_idx = self.utf8(internal_name);
        self.add(Key::Class(name_idx), Entry::Class(name_idx))
    }

    pub fn string(&mut self, value: &str) -> u16 {
        let utf8 = self.utf8(value);
        self.add(Key::String(utf8), Entry::String(utf8))
    }

    pub fn name_and_type(&mut self, name: &str, descriptor: &str) -> u16 {
        let n = self.utf8(name);
        let d = self.utf8(descriptor);
        self.add(Key::NameAndType(n, d), Entry::NameAndType(n, d))
    }

    pub fn fieldref(&mut self, class_internal: &str, name: &str, descriptor: &str) -> u16 {
        let c = self.class(class_internal);
        let nt = self.name_and_type(name, descriptor);
        self.add(Key::Fieldref(c, nt), Entry::Fieldref(c, nt))
    }

    pub fn methodref(&mut self, class_internal: &str, name: &str, descriptor: &str) -> u16 {
        let c = self.class(class_internal);
        let nt = self.name_and_type(name, descriptor);
        self.add(Key::Methodref(c, nt), Entry::Methodref(c, nt))
    }

    pub fn interface_methodref(
        &mut self,
        class_internal: &str,
        name: &str,
        descriptor: &str,
    ) -> u16 {
        let c = self.class(class_internal);
        let nt = self.name_and_type(name, descriptor);
        self.add(
            Key::InterfaceMethodref(c, nt),
            Entry::InterfaceMethodref(c, nt),
        )
    }

    /// Per JVMS 4.1, `constant_pool_count` is the number of entries plus one.
    pub fn count(&self) -> u16 {
        (self.entries.len() + 1) as u16
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        for entry in &self.entries {
            match entry {
                Entry::Utf8(s) => {
                    out.push(1); // CONSTANT_Utf8
                    let bytes = s.as_bytes();
                    out.write_u16::<BigEndian>(bytes.len() as u16).unwrap();
                    out.write_all(bytes).unwrap();
                }
                Entry::Integer(v) => {
                    out.push(3); // CONSTANT_Integer
                    out.write_i32::<BigEndian>(*v).unwrap();
                }
                Entry::Long(v) => {
                    out.push(5); // CONSTANT_Long
                    out.write_i64::<BigEndian>(*v).unwrap();
                }
                Entry::Double(v) => {
                    out.push(6); // CONSTANT_Double
                    out.write_u64::<BigEndian>(v.to_bits()).unwrap();
                }
                Entry::WideSlot => {
                    // Second slot of a Double/Long entry — already accounted for.
                    continue;
                }
                Entry::Class(idx) => {
                    out.push(7);
                    out.write_u16::<BigEndian>(*idx).unwrap();
                }
                Entry::String(idx) => {
                    out.push(8);
                    out.write_u16::<BigEndian>(*idx).unwrap();
                }
                Entry::Fieldref(c, nt) => {
                    out.push(9);
                    out.write_u16::<BigEndian>(*c).unwrap();
                    out.write_u16::<BigEndian>(*nt).unwrap();
                }
                Entry::Methodref(c, nt) => {
                    out.push(10); // CONSTANT_Methodref
                    out.write_u16::<BigEndian>(*c).unwrap();
                    out.write_u16::<BigEndian>(*nt).unwrap();
                }
                Entry::InterfaceMethodref(c, nt) => {
                    out.push(11); // CONSTANT_InterfaceMethodref
                    out.write_u16::<BigEndian>(*c).unwrap();
                    out.write_u16::<BigEndian>(*nt).unwrap();
                }
                Entry::NameAndType(n, d) => {
                    out.push(12);
                    out.write_u16::<BigEndian>(*n).unwrap();
                    out.write_u16::<BigEndian>(*d).unwrap();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_utf8() {
        let mut cp = ConstantPool::new();
        let a = cp.utf8("hello");
        let b = cp.utf8("hello");
        let c = cp.utf8("world");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn fieldref_uses_class_and_nameandtype() {
        let mut cp = ConstantPool::new();
        let fr = cp.fieldref("java/lang/System", "out", "Ljava/io/PrintStream;");
        // Manually re-derive: utf8 for class name + utf8 for name + utf8 for desc
        // + class + nameandtype + fieldref. So fieldref index should be 6.
        assert_eq!(fr, 6);
    }

    #[test]
    fn index_starts_at_one() {
        let mut cp = ConstantPool::new();
        let idx = cp.utf8("first");
        assert_eq!(idx, 1);
        // count() is one more than the number of entries.
        assert_eq!(cp.count(), 2);
    }

    // ─── future test stubs ───────────────────────────────────────────────
    // TODO: long_takes_two_slots — Long entries skip the next index
    // TODO: modified_utf8         — null bytes encoded as 0xC0 0x80
}
