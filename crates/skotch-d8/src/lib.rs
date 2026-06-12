//! `skotch d8` driver: a native reimplementation of the Android SDK `d8`
//! dexer. Reads `.class`/`.jar`/`.zip` inputs, dexes them, and writes a DEX.
//!
//! This is the Phase-0/1 bootstrap: it wires the format writer
//! ([`skotch_dex`]), the class reader ([`skotch_classfile`]), and the
//! bootstrap CF→DEX translator ([`skotch_dexcore::bootstrap`]) into an
//! end-to-end tool that is byte-identical to d8 for the trivial subset and
//! errors loudly (never silently miscompiles) outside it. The full SSA IR +
//! register allocator and the multidex/desugaring stages land in later phases
//! per `docs/skotch-d8-design.md`.

use anyhow::{bail, Context, Result};
use skotch_classfile::ClassFile;
use std::path::{Path, PathBuf};

/// d8 compilation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Debug,
    Release,
}

impl Mode {
    fn marker_name(self) -> &'static str {
        match self {
            Mode::Debug => "debug",
            Mode::Release => "release",
        }
    }
}

/// Options mirroring the subset of `D8Command` implemented so far.
pub struct D8Options {
    pub inputs: Vec<PathBuf>,
    pub output: PathBuf,
    pub min_api: u32,
    pub mode: Mode,
}

impl Default for D8Options {
    fn default() -> Self {
        D8Options { inputs: vec![], output: PathBuf::from("."), min_api: 1, mode: Mode::Debug }
    }
}

/// Runs the dexer end-to-end, writing `classes.dex` into the output directory.
pub fn run(opts: &D8Options) -> Result<()> {
    let classes = read_inputs(&opts.inputs)?;
    if classes.is_empty() {
        bail!("no .class inputs found");
    }
    let dex = dex_classes(&classes, opts)?;

    if opts.output.extension().map(|e| e == "zip" || e == "jar").unwrap_or(false) {
        bail!("zip output not yet supported; pass an output directory");
    }
    std::fs::create_dir_all(&opts.output)
        .with_context(|| format!("creating {}", opts.output.display()))?;
    let out_path = opts.output.join("classes.dex");
    std::fs::write(&out_path, &dex).with_context(|| format!("writing {}", out_path.display()))?;
    Ok(())
}

/// Dexes a set of classes into a single DEX (the bytes), in-process. Exposed so
/// the build pipeline can dex dependency `.class` files without a subprocess.
pub fn dex_classes(classes: &[ClassFile], opts: &D8Options) -> Result<Vec<u8>> {
    let mut model = skotch_dex::model::DexFile {
        classes: Vec::with_capacity(classes.len()),
        extra_strings: vec![skotch_dex::d8_marker(opts.mode.marker_name(), opts.min_api)],
    };
    // d8 orders class_defs so a superclass precedes its subclasses; for the
    // bootstrap (independent classes) sort by type descriptor.
    let mut sorted: Vec<&ClassFile> = classes.iter().collect();
    sorted.sort_by(|a, b| a.descriptor().cmp(&b.descriptor()));
    for cf in sorted {
        model.classes.push(
            skotch_dexcore::dex_class(cf, opts.min_api)
                .with_context(|| format!("dexing {}", cf.this_class))?,
        );
    }
    Ok(skotch_dex::write(&model))
}

fn read_inputs(inputs: &[PathBuf]) -> Result<Vec<ClassFile>> {
    let mut classes = Vec::new();
    for input in inputs {
        read_input(input, &mut classes)?;
    }
    Ok(classes)
}

fn read_input(path: &Path, out: &mut Vec<ClassFile>) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "class" => out.push(skotch_classfile::parse_class_file(path)?),
        "jar" | "zip" | "apk" => {
            let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            out.extend(skotch_classfile::parse_archive(&bytes)?);
        }
        "dex" => bail!(".dex inputs (merging) not yet supported"),
        other => bail!("unsupported input extension: .{other}"),
    }
    Ok(())
}
