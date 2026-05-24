//! Integration test for the `@kotlin.Metadata` extraction pipeline,
//! exercised end-to-end against a real kotlinc-compiled class.
//!
//! The vector `tests/data/MetaProbeKt.class` is the file facade kotlinc
//! emits for this top-level Kotlin source (committed so the test is
//! deterministic and needs no kotlinc at run time):
//!
//! ```kotlin
//! package probe
//! fun topLevel(x: Int): Int = x + 1
//! fun <T> T.applyish(block: T.() -> Unit): T { block(); return this }
//! fun <T, R> T.letish(block: (T) -> R): R = block(this)
//! ```
//!
//! (The committed fixture `*.class` goldens elsewhere have `@Metadata`
//! stripped, so they can't serve as a vector here.)

use skotch_classinfo::kotlin_metadata::{bit_encoding, protobuf};
use std::path::PathBuf;

fn crate_file(rel: &str) -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn workspace_file(rel: &str) -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.push(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn extracts_and_decodes_metadata_from_file_facade() {
    let bytes = crate_file("tests/data/MetaProbeKt.class");
    let ci = skotch_classinfo::parse_class(&bytes).expect("parse class");

    let md = ci
        .metadata
        .expect("a kotlinc-compiled class must carry @kotlin.Metadata");
    // A top-level file facade → @Metadata.k == 2.
    assert_eq!(md.kind, 2, "MetaProbeKt is a file facade");
    assert!(!md.data1.is_empty(), "d1 payload present");
    assert!(!md.data2.is_empty(), "d2 string table present");

    // The BitEncoding-packed d1 decodes to a non-empty protobuf stream
    // whose first field tag is well-formed — proving the constant-pool
    // Modified-UTF8 read + BitEncoding decode line up correctly.
    let proto = bit_encoding::decode_bytes(&md.data1);
    assert!(!proto.is_empty(), "decoded protobuf is non-empty");
    let mut r = protobuf::Reader::new(&proto);
    let (field, _wire) = r.read_tag().expect("first protobuf tag is readable");
    assert!(field > 0, "field numbers are positive");

    // d2 holds the names referenced by the protobuf; our function names
    // must appear there.
    assert!(
        md.data2.iter().any(|s| s == "topLevel"),
        "d2 should contain the top-level function name, got {:?}",
        md.data2
    );
}

#[test]
fn class_without_kotlin_metadata_is_none() {
    // skotch's own emitted goldens are not kotlinc output and carry no
    // @kotlin.Metadata, so extraction yields None (and does not panic).
    let bytes = workspace_file("tests/fixtures/expected/jvm/120-logical-and-or/skotch.class");
    let ci = skotch_classinfo::parse_class(&bytes).expect("parse class");
    assert!(
        ci.metadata.is_none(),
        "skotch-emitted class has no @kotlin.Metadata"
    );
}
