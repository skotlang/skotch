//! Byte-identity of the DEX writer against real d8 8.10.9 output.

use skotch_dex::model::*;
use skotch_dex::{d8_marker, write};
use std::path::Path;

fn golden(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

fn object_init() -> MethodRef {
    MethodRef {
        class: "Ljava/lang/Object;".into(),
        proto: ProtoRef { return_type: "V".into(), params: vec![] },
        name: "<init>".into(),
    }
}

/// The `Empty` class: a single synthesized `<init>` that calls `super()`.
#[test]
fn empty_class_byte_identical() {
    // insns: invoke-direct {v0}, Object.<init>:()V ; return-void
    //   unit0 = 0x1070 (35c, A=1, op=invoke-direct)
    //   unit1 = method idx (patched via fixup)
    //   unit2 = 0x0000 (registers: C=v0)
    //   unit3 = 0x000e (return-void)
    let code = CodeItem {
        registers_size: 1,
        ins_size: 1,
        outs_size: 1,
        insns: vec![0x1070, 0x0000, 0x0000, 0x000e],
        fixups: vec![Fixup { unit: 1, item: ItemRef::Method(object_init()), wide: false }],
        tries: vec![],
        debug_info: Some(DebugInfo {
            line_start: 1,
            parameter_names: vec![],
            events: vec![DebugEvent::Special(0x0e)],
        }),
    };
    let init = EncodedMethod {
        method: MethodRef {
            class: "LEmpty;".into(),
            proto: ProtoRef { return_type: "V".into(), params: vec![] },
            name: "<init>".into(),
        },
        access_flags: 0x10001, // public constructor
        code: Some(code),
    };
    let class = ClassDef {
        class_type: "LEmpty;".into(),
        access_flags: 0x1,
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        source_file: Some("Empty.java".into()),
        static_fields: vec![],
        instance_fields: vec![],
        direct_methods: vec![init],
        virtual_methods: vec![],
        static_values: vec![],
    };
    let file = DexFile {
        classes: vec![class],
        extra_strings: vec![d8_marker("release", 1)],
    };

    let produced = write(&file);
    let golden = golden("Empty.d8.dex");

    skotch_dex::validator::validate(&produced).expect("self-validation");
    if produced != golden {
        let diff = (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i]);
        panic!(
            "Empty.dex mismatch: produced {} bytes, golden {} bytes, first diff at {:?}",
            produced.len(),
            golden.len(),
            diff
        );
    }
}
