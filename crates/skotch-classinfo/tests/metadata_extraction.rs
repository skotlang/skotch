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
fn schema_walk_recovers_functions_and_extension_types() {
    let bytes = crate_file("tests/data/MetaProbeKt.class");
    let ci = skotch_classinfo::parse_class(&bytes).expect("parse class");
    let md = ci.metadata.expect("@kotlin.Metadata present");

    let cm = skotch_classinfo::kotlin_metadata::parse_metadata(&md)
        .expect("schema walk over the decoded protobuf");

    let names: Vec<&str> = cm.functions.iter().map(|f| f.name.as_str()).collect();
    for expected in ["topLevel", "applyish", "letish"] {
        assert!(
            names.contains(&expected),
            "function {expected} recovered; got {names:?}"
        );
    }

    // `applyish(block: T.() -> Unit)` — its block parameter is a
    // *receiver* function type (this-binding); `letish(block: (T) -> R)`
    // is a plain function type (it-binding). This is the structural
    // signal that lets scope-fn this/it be derived without a name list.
    let applyish = cm.functions.iter().find(|f| f.name == "applyish").unwrap();
    assert!(
        applyish
            .value_params
            .iter()
            .any(|p| p.ty.is_extension_function_type),
        "applyish's block is an extension function type: {:?}",
        applyish.value_params
    );
    // It's an extension function (fun T.applyish), so it has a receiver.
    assert!(
        applyish.receiver_type.is_some(),
        "applyish has an extension receiver"
    );
    let letish = cm.functions.iter().find(|f| f.name == "letish").unwrap();
    assert!(
        letish
            .value_params
            .iter()
            .all(|p| !p.ty.is_extension_function_type),
        "letish's block is a plain function type: {:?}",
        letish.value_params
    );
    // Parameter names are recovered from metadata too.
    assert!(
        applyish.value_params.iter().any(|p| p.name == "block"),
        "param name 'block' recovered: {:?}",
        applyish.value_params
    );

    // Concrete parameter/return types resolve to their FQ names:
    // `topLevel(x: Int): Int`.
    let top = cm.functions.iter().find(|f| f.name == "topLevel").unwrap();
    assert_eq!(
        top.return_type
            .as_ref()
            .and_then(|t| t.class_name.as_deref()),
        Some("kotlin/Int"),
        "topLevel returns Int: {:?}",
        top.return_type
    );
    assert_eq!(
        top.value_params
            .first()
            .map(|p| (p.name.as_str(), p.ty.class_name.as_deref())),
        Some(("x", Some("kotlin/Int"))),
        "topLevel's param x: Int: {:?}",
        top.value_params
    );
}

#[test]
fn recovers_generic_nullable_and_nested_types() {
    // MetaProbe2Kt:
    //   fun makeList(): List<String>
    //   fun maybe(s: String?): String?
    //   fun nested(): Map<String, List<Int>>
    let bytes = crate_file("tests/data/MetaProbe2Kt.class");
    let ci = skotch_classinfo::parse_class(&bytes).expect("parse class");
    let md = ci.metadata.expect("@kotlin.Metadata present");
    let cm = skotch_classinfo::kotlin_metadata::parse_metadata(&md).expect("schema walk");

    // makeList(): List<String>
    let make_list = cm.functions.iter().find(|f| f.name == "makeList").unwrap();
    let ret = make_list.return_type.as_ref().expect("return type");
    assert_eq!(ret.class_name.as_deref(), Some("kotlin/collections/List"));
    assert_eq!(
        ret.arguments.first().and_then(|a| a.class_name.as_deref()),
        Some("kotlin/String"),
        "List element is String: {ret:?}"
    );

    // maybe(s: String?): String? — nullability on both param and return.
    let maybe = cm.functions.iter().find(|f| f.name == "maybe").unwrap();
    assert!(
        maybe.return_type.as_ref().is_some_and(|t| t.nullable),
        "maybe returns String?: {:?}",
        maybe.return_type
    );
    assert!(
        maybe.value_params.first().is_some_and(|p| p.ty.nullable),
        "maybe's param is String?: {:?}",
        maybe.value_params
    );

    // nested(): Map<String, List<Int>> — nested generic arguments.
    let nested = cm.functions.iter().find(|f| f.name == "nested").unwrap();
    let map = nested.return_type.as_ref().expect("return type");
    assert_eq!(map.class_name.as_deref(), Some("kotlin/collections/Map"));
    assert_eq!(
        map.arguments.first().and_then(|a| a.class_name.as_deref()),
        Some("kotlin/String"),
        "Map key is String: {map:?}"
    );
    let inner = map.arguments.get(1).expect("Map value arg");
    assert_eq!(inner.class_name.as_deref(), Some("kotlin/collections/List"));
    assert_eq!(
        inner
            .arguments
            .first()
            .and_then(|a| a.class_name.as_deref()),
        Some("kotlin/Int"),
        "Map value is List<Int>: {inner:?}"
    );
}

#[test]
fn recovers_functions_from_a_class_kind_1() {
    // A regular class (not a file facade) → @Metadata.k == 1, with
    // functions under `Class.function` (field 9) rather than
    // `Package.function` (field 3). Exercises that path.
    //   class Box(val value: Int) {
    //       fun describe(): String
    //       fun plus(other: Int): Int
    //   }
    let bytes = crate_file("tests/data/Box.class");
    let ci = skotch_classinfo::parse_class(&bytes).expect("parse class");
    let md = ci.metadata.expect("@kotlin.Metadata present");
    assert_eq!(md.kind, 1, "Box is a class (k=1)");

    let cm = skotch_classinfo::kotlin_metadata::parse_metadata(&md).expect("schema walk");
    let describe = cm
        .functions
        .iter()
        .find(|f| f.name == "describe")
        .expect("describe recovered from a k=1 class");
    assert_eq!(
        describe
            .return_type
            .as_ref()
            .and_then(|t| t.class_name.as_deref()),
        Some("kotlin/String"),
        "describe(): String: {:?}",
        describe.return_type
    );

    let plus = cm.functions.iter().find(|f| f.name == "plus").unwrap();
    assert_eq!(
        plus.return_type
            .as_ref()
            .and_then(|t| t.class_name.as_deref()),
        Some("kotlin/Int"),
        "plus(other: Int): Int: {:?}",
        plus.return_type
    );
    assert_eq!(
        plus.value_params
            .first()
            .map(|p| (p.name.as_str(), p.ty.class_name.as_deref())),
        Some(("other", Some("kotlin/Int"))),
        "plus's param other: Int: {:?}",
        plus.value_params
    );
}

#[test]
fn class_function_metadata_bridge_resolves_by_name() {
    let bytes = crate_file("tests/data/MetaProbeKt.class");
    let ci = skotch_classinfo::parse_class(&bytes).expect("parse class");

    // The inference-facing bridge finds a function by source name and
    // surfaces its recovered shape.
    let applyish = skotch_classinfo::class_function_metadata(&ci, "applyish")
        .expect("applyish recovered via bridge");
    assert!(
        applyish
            .value_params
            .iter()
            .any(|p| p.ty.is_extension_function_type),
        "bridge surfaces applyish's receiver-lambda param"
    );

    // A plain Java class (no @Metadata) yields None, not a panic.
    assert!(skotch_classinfo::class_function_metadata(&ci, "doesNotExist").is_none());
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
