//! `@kotlin.Metadata` annotation writer — emits the protobuf payload
//! that kotlinc stamps onto every `.class` file, so skotch-emitted
//! classes can carry the unerased Kotlin signatures their downstream
//! consumers (other kotlinc-compiled callers, the skotch
//! `@Metadata` reader at `skotch_classinfo::kotlin_metadata`) need to
//! reconstruct `T.() -> R`, default values, parameter names, suspend-
//! ness, sealed-subclass lists, etc.
//!
//! Phase A scope: the foundation only — vendored Kotlin metadata
//! `.proto` schemas (pinned to v2.4.0, see `proto/README.md`), `prost`
//! code-gen of the message types, the `d1` String[] codec (writer side
//! of [`skotch_classinfo::kotlin_metadata::bit_encoding`]), and a
//! round-trip test proving an empty
//! [`org::jetbrains::kotlin::metadata::Class`] serialises and decodes
//! back through skotch-classinfo's existing reader. Subsequent phases
//! will layer the higher-level
//! "MIR-class → ProtoBuf.Class + StringTable" lowering on top.

pub mod bit_encoding;

/// Generated protobuf message types. The vendored `.proto` files use
/// `package org.jetbrains.kotlin.metadata{,.jvm}`, so prost emits
/// `org::jetbrains::kotlin::metadata::*` and
/// `org::jetbrains::kotlin::metadata::jvm::*` here.
pub mod proto {
    // `metadata.proto` → `org.jetbrains.kotlin.metadata`.
    pub mod org {
        pub mod jetbrains {
            pub mod kotlin {
                pub mod metadata {
                    include!(concat!(
                        env!("OUT_DIR"),
                        "/org.jetbrains.kotlin.metadata.rs"
                    ));

                    // `jvm_metadata.proto` + `jvm_module.proto` →
                    // `org.jetbrains.kotlin.metadata.jvm`.
                    pub mod jvm {
                        include!(concat!(
                            env!("OUT_DIR"),
                            "/org.jetbrains.kotlin.metadata.jvm.rs"
                        ));
                    }
                }
            }
        }
    }
}

pub use bit_encoding::encode_bytes;

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use proto::org::jetbrains::kotlin::metadata::Class;
    use skotch_classinfo::kotlin_metadata::bit_encoding::decode_bytes;

    /// Phase A acceptance test: hand-build a minimal `ProtoBuf.Class`,
    /// serialise it via `prost`, BitEncode → `d1` `String[]`, decode
    /// through the existing skotch-classinfo reader, and assert the
    /// byte stream survives unchanged. This wires the three Phase A
    /// pieces (prost codegen + BitEncoding writer + existing decoder)
    /// end-to-end.
    #[test]
    fn empty_class_round_trips_through_decoder() {
        // `flags = 0` is the only required field on a minimal Class
        // (kotlinc's `Class` message marks `fq_name` `required` too,
        // but the proto generator surfaces all proto2 fields as
        // `Option<_>` — leaving them `None` produces a well-formed
        // serialised message; field 1 simply does not appear on the
        // wire). For Phase A all we need is a non-empty payload that
        // exercises the prost runtime + the codec.
        let class = Class {
            flags: Some(0),
            fq_name: 0,
            ..Default::default()
        };

        let mut buf: Vec<u8> = Vec::with_capacity(class.encoded_len());
        class.encode(&mut buf).expect("prost encode");
        assert!(!buf.is_empty(), "expected non-empty wire bytes");

        let encoded = encode_bytes(&buf);
        let decoded = decode_bytes(&encoded);
        assert_eq!(decoded, buf, "BitEncoding round-trip mismatch");
    }

    /// Belt-and-suspenders: a payload built from a small fragment of
    /// a real metadata stream (`field 1 length-delim "listOf"`,
    /// `field 2 length-delim ""`) also round-trips.
    #[test]
    fn handcrafted_payload_round_trips() {
        let payload: &[u8] = b"\x0a\x06listOf\x12\x00";
        let encoded = encode_bytes(payload);
        assert_eq!(decode_bytes(&encoded), payload);
    }
}
