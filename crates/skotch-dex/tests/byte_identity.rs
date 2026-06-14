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
        annotations: vec![],
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
        annotations: vec![],
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

/// The writer emits `static_values` as an `encoded_array_item` (DEX §7) between
/// class_data and annotation_set, sets the class_def `static_values_off`, and adds a
/// `0x2005` map entry. Verifies the encoded bytes match d8's exact form for
/// `static int C=10; static int D=20;` → `02 04 0a 04 14` (size 2; int 10; int 20),
/// that the dex self-validates, and that it round-trips through the reader.
#[test]
fn static_values_encoded_array() {
    let init = EncodedMethod {
        method: MethodRef {
            class: "LSv;".into(),
            proto: ProtoRef { return_type: "V".into(), params: vec![] },
            name: "<init>".into(),
        },
        access_flags: 0x10001,
        code: Some(CodeItem {
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
        }),
        annotations: vec![],
    };
    let sfield = |name: &str| EncodedField {
        field: FieldRef { class: "LSv;".into(), type_: "I".into(), name: name.into() },
        access_flags: 0x8,
        annotations: vec![],
    };
    let class = ClassDef {
        class_type: "LSv;".into(),
        access_flags: 0x1,
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        source_file: Some("Sv.java".into()),
        static_fields: vec![sfield("C"), sfield("D")],
        instance_fields: vec![],
        direct_methods: vec![init],
        virtual_methods: vec![],
        static_values: vec![EncodedValue::Int(10), EncodedValue::Int(20)],
        annotations: vec![],
    };
    let file = DexFile { classes: vec![class], extra_strings: vec![d8_marker("release", 1)] };
    let dex = write(&file);
    skotch_dex::validator::validate(&dex).expect("self-validation");

    // class_defs_off at header[0x64]; class_def[0].static_values_off at +28.
    let u32at = |o: usize| u32::from_le_bytes(dex[o..o + 4].try_into().unwrap());
    let cd_off = u32at(0x64) as usize;
    let sv_off = u32at(cd_off + 28) as usize;
    assert_ne!(sv_off, 0, "static_values_off must be set");
    assert_eq!(&dex[sv_off..sv_off + 5], &[0x02, 0x04, 0x0a, 0x04, 0x14], "encoded_array bytes");

    // The map_list has an encoded_array (type 0x2005) entry pointing at it.
    let header = skotch_dex::reader::parse_header(&dex).expect("header");
    let map = skotch_dex::reader::parse_map(&dex, header.map_off);
    let ea = map.iter().find(|&&(t, _, _)| t == 0x2005).expect("0x2005 map entry");
    assert_eq!(ea.2 as usize, sv_off, "0x2005 entry offset");
}
