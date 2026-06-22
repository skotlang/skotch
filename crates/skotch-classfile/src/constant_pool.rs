//! JVM constant pool parsing and convenience resolution.

use anyhow::{bail, Result};

/// One constant-pool entry (the variants the dexer needs).
#[derive(Debug, Clone)]
pub enum Constant {
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class {
        name_index: u16,
    },
    String {
        string_index: u16,
    },
    FieldRef {
        class_index: u16,
        name_and_type_index: u16,
    },
    MethodRef {
        class_index: u16,
        name_and_type_index: u16,
    },
    InterfaceMethodRef {
        class_index: u16,
        name_and_type_index: u16,
    },
    NameAndType {
        name_index: u16,
        descriptor_index: u16,
    },
    MethodHandle {
        reference_kind: u8,
        reference_index: u16,
    },
    MethodType {
        descriptor_index: u16,
    },
    Dynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    InvokeDynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    Module {
        name_index: u16,
    },
    Package {
        name_index: u16,
    },
    /// The unused slot following a Long/Double.
    Unusable,
}

/// The parsed constant pool (1-indexed; index 0 is reserved).
#[derive(Debug, Clone)]
pub struct ConstantPool {
    entries: Vec<Constant>,
}

impl ConstantPool {
    pub fn parse(data: &[u8], mut pos: usize, count: u16) -> Result<(ConstantPool, usize)> {
        let mut entries = vec![Constant::Unusable]; // index 0
        let mut i = 1;
        while i < count {
            let tag = data[pos];
            pos += 1;
            let c = match tag {
                1 => {
                    let len = u16(data, pos) as usize;
                    pos += 2;
                    let s = mutf8_to_string(&data[pos..pos + len]);
                    pos += len;
                    Constant::Utf8(s)
                }
                3 => {
                    let v = i32::from_be_bytes(data[pos..pos + 4].try_into().unwrap());
                    pos += 4;
                    Constant::Integer(v)
                }
                4 => {
                    let v = f32::from_be_bytes(data[pos..pos + 4].try_into().unwrap());
                    pos += 4;
                    Constant::Float(v)
                }
                5 => {
                    let v = i64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
                    pos += 8;
                    Constant::Long(v)
                }
                6 => {
                    let v = f64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
                    pos += 8;
                    Constant::Double(v)
                }
                7 => {
                    let c = Constant::Class {
                        name_index: u16(data, pos),
                    };
                    pos += 2;
                    c
                }
                8 => {
                    let c = Constant::String {
                        string_index: u16(data, pos),
                    };
                    pos += 2;
                    c
                }
                9 => {
                    let c = Constant::FieldRef {
                        class_index: u16(data, pos),
                        name_and_type_index: u16(data, pos + 2),
                    };
                    pos += 4;
                    c
                }
                10 => {
                    let c = Constant::MethodRef {
                        class_index: u16(data, pos),
                        name_and_type_index: u16(data, pos + 2),
                    };
                    pos += 4;
                    c
                }
                11 => {
                    let c = Constant::InterfaceMethodRef {
                        class_index: u16(data, pos),
                        name_and_type_index: u16(data, pos + 2),
                    };
                    pos += 4;
                    c
                }
                12 => {
                    let c = Constant::NameAndType {
                        name_index: u16(data, pos),
                        descriptor_index: u16(data, pos + 2),
                    };
                    pos += 4;
                    c
                }
                15 => {
                    let c = Constant::MethodHandle {
                        reference_kind: data[pos],
                        reference_index: u16(data, pos + 1),
                    };
                    pos += 3;
                    c
                }
                16 => {
                    let c = Constant::MethodType {
                        descriptor_index: u16(data, pos),
                    };
                    pos += 2;
                    c
                }
                17 => {
                    let c = Constant::Dynamic {
                        bootstrap_method_attr_index: u16(data, pos),
                        name_and_type_index: u16(data, pos + 2),
                    };
                    pos += 4;
                    c
                }
                18 => {
                    let c = Constant::InvokeDynamic {
                        bootstrap_method_attr_index: u16(data, pos),
                        name_and_type_index: u16(data, pos + 2),
                    };
                    pos += 4;
                    c
                }
                19 => {
                    let c = Constant::Module {
                        name_index: u16(data, pos),
                    };
                    pos += 2;
                    c
                }
                20 => {
                    let c = Constant::Package {
                        name_index: u16(data, pos),
                    };
                    pos += 2;
                    c
                }
                other => bail!("unknown constant pool tag {other}"),
            };
            let wide = matches!(c, Constant::Long(_) | Constant::Double(_));
            entries.push(c);
            if wide {
                entries.push(Constant::Unusable);
                i += 2;
            } else {
                i += 1;
            }
        }
        Ok((ConstantPool { entries }, pos))
    }

    pub fn get(&self, index: u16) -> &Constant {
        &self.entries[index as usize]
    }

    pub fn utf8(&self, index: u16) -> Result<&str> {
        match self.get(index) {
            Constant::Utf8(s) => Ok(s),
            _ => bail!("constant {index} is not Utf8"),
        }
    }

    /// The internal class name (e.g. `java/lang/Object`) of a `Class` entry.
    pub fn class_name(&self, index: u16) -> Result<&str> {
        match self.get(index) {
            Constant::Class { name_index } => self.utf8(*name_index),
            _ => bail!("constant {index} is not a Class"),
        }
    }

    /// Resolves a `NameAndType` to `(name, descriptor)`.
    pub fn name_and_type(&self, index: u16) -> Result<(&str, &str)> {
        match self.get(index) {
            Constant::NameAndType {
                name_index,
                descriptor_index,
            } => Ok((self.utf8(*name_index)?, self.utf8(*descriptor_index)?)),
            _ => bail!("constant {index} is not NameAndType"),
        }
    }

    /// Resolves a `Field/Method/InterfaceMethodRef` to `(class, name, desc)`.
    pub fn member_ref(&self, index: u16) -> Result<(String, String, String)> {
        let (ci, nti) = match self.get(index) {
            Constant::FieldRef {
                class_index,
                name_and_type_index,
            }
            | Constant::MethodRef {
                class_index,
                name_and_type_index,
            }
            | Constant::InterfaceMethodRef {
                class_index,
                name_and_type_index,
            } => (*class_index, *name_and_type_index),
            _ => bail!("constant {index} is not a member ref"),
        };
        let class = self.class_name(ci)?.to_string();
        let (name, desc) = self.name_and_type(nti)?;
        Ok((class, name.to_string(), desc.to_string()))
    }
}

fn u16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}

/// Decodes a MUTF-8 constant-pool string.
pub fn mutf8_to_string(bytes: &[u8]) -> String {
    skotch_dex_mutf8_decode(bytes)
}

/// Inline MUTF-8 decoder (kept local so this crate doesn't depend on
/// `skotch-dex`).
fn skotch_dex_mutf8_decode(bytes: &[u8]) -> String {
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
            units.push(
                (((a & 0x0f) as u16) << 12) | (((b & 0x3f) as u16) << 6) | ((c & 0x3f) as u16),
            );
            i += 3;
        }
    }
    String::from_utf16_lossy(&units)
}

/// Convert an internal class name (`java/lang/Object`) to a DEX type descriptor
/// (`Ljava/lang/Object;`). Array names (`[...`) and primitives pass through.
pub fn internal_to_descriptor(name: &str) -> String {
    if name.starts_with('[') {
        name.to_string()
    } else {
        format!("L{name};")
    }
}
