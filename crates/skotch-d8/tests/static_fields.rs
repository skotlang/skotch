//! Constant static-field initializers are hoisted into the DEX `static_values`
//! encoded array with an emptied `<clinit>`, matching d8 — common to nearly every
//! real class (`static`/`static final` constants).

use skotch_d8::{dex_classes, D8Options, Mode};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../skotch-dex/tests/fixtures")
}

fn assert_byte_identical(name: &str) {
    let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
    let opts = D8Options {
        min_api: 1,
        mode: Mode::Release,
        ..Default::default()
    };
    let produced = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name} should dex: {e:#}"));
    let golden = std::fs::read(fixtures().join(format!("{name}.d8.dex"))).unwrap();
    if produced != golden {
        std::fs::write(format!("/tmp/skotch-{name}-produced.dex"), &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "{name}: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// `StaticAcc` (`static int C=10`) and `Sv` (`static int C=10; static final int
/// D=20`): the constant inits land in `static_values` (`02 04 0a 04 14`), the
/// `<clinit>` becomes a bare `return-void`. Byte-identical to d8.
#[test]
fn static_const_init_byte_identical() {
    assert_byte_identical("StaticAcc");
    assert_byte_identical("Sv");
}
