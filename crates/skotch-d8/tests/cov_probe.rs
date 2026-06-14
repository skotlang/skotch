use skotch_d8::{dex_classes, D8Options, Mode};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn walk(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("class") {
            out.push(p);
        }
    }
}

/// Measures whole-class dex-OK% and the first-bail histogram over a corpus of `.class`
/// files (e.g. an unzipped Maven jar). Opt-in: set COV_ROOT to a directory of classes.
/// Skips (passes) when the corpus is absent, so it never breaks the default suite.
/// Usage: `COV_ROOT=/tmp/corpus/kstdlib cargo test -p skotch-d8 --release --test cov_probe -- --nocapture`
#[test]
fn cov() {
    let Ok(root) = std::env::var("COV_ROOT") else {
        eprintln!("SKIP cov: set COV_ROOT=<dir of .class files> to measure dex-OK coverage");
        return;
    };
    if !PathBuf::from(&root).is_dir() {
        eprintln!("SKIP cov: COV_ROOT={root} is not a directory");
        return;
    }
    let mut files = Vec::new();
    walk(&PathBuf::from(&root), &mut files);
    files.sort();
    let mut ok = 0usize;
    let mut bail = 0usize;
    let mut parse_err = 0usize;
    let mut hist: BTreeMap<String, usize> = BTreeMap::new();
    for f in &files {
        let cf = match skotch_classfile::parse_class_file(f) {
            Ok(c) => c,
            Err(_) => {
                parse_err += 1;
                continue;
            }
        };
        match dex_classes(
            &[cf],
            &D8Options { min_api: 1, mode: Mode::Release, ..Default::default() },
        ) {
            Ok(_) => ok += 1,
            Err(e) => {
                bail += 1;
                // Normalize the bail message to a (path: opcode) signature.
                let m = format!("{e:#}");
                let sig = normalize(&m);
                *hist.entry(sig).or_default() += 1;
            }
        }
    }
    let total = ok + bail;
    eprintln!("=== corpus {root}: {} files ({parse_err} parse-err) ===", files.len());
    eprintln!(
        "dex-OK: {ok}/{total} = {:.1}%   (bail {bail})",
        100.0 * ok as f64 / total.max(1) as f64
    );
    let mut v: Vec<_> = hist.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("--- bail histogram (first bail per class) ---");
    for (sig, n) in v.iter().take(40) {
        eprintln!("{n:5}  {sig}");
    }
}

/// Collapse a bail message to "path | opcode" so e.g. method names / descriptors don't fragment.
fn normalize(m: &str) -> String {
    // Pull the path prefix before the first ':' inside the dexer message.
    let path = if m.contains("(cfg)") {
        "cfg"
    } else if m.contains("ssa stack-sim") {
        "ssa-sim"
    } else if m.contains("ssa:") {
        "ssa"
    } else if m.contains("dexer:") {
        "straight"
    } else {
        "other"
    };
    // Pull "opcode 0xNN" if present.
    if let Some(i) = m.find("opcode 0x") {
        let op = &m[i + 7..(i + 11).min(m.len())];
        return format!("{path:9} opcode {op}");
    }
    // Otherwise keep the leading clause (up to first " in " or "(").
    let head = m.splitn(2, " in ").next().unwrap_or(m);
    let head = head.splitn(2, " (").next().unwrap_or(head);
    format!("{path:9} {head}")
}
