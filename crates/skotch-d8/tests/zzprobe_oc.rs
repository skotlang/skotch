use skotch_d8::{dex_classes, D8Options, Mode};
use std::path::PathBuf;

fn probe(path: &str) {
    let p = PathBuf::from(path);
    let Ok(cf) = skotch_classfile::parse_class_file(&p) else {
        eprintln!("PROBE-OC {path}: parse failed");
        return;
    };
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    match dex_classes(&[cf], &opts) {
        Ok(_) => eprintln!("PROBE-OC {path}: DEXES"),
        Err(e) => eprintln!("PROBE-OC {path}: BAILS: {e:#}"),
    }
}

#[test]
fn probe_oc() {
    probe("/tmp/corpus/kstdlib/kotlin/text/UStringsKt.class");
    probe("/tmp/corpus/kstdlib/kotlin/time/Duration.class");
    probe("/tmp/corpus/kstdlib/kotlin/time/DurationKt.class");
    probe("/tmp/corpus/kstdlib/kotlin/text/StringsKt__StringNumberConversionsJVMKt.class");
}
