//! skotch Kotlin Library (.klib) writer and reader.
//!
//! The Kotlin Native compiler uses a multi-stage pipeline:
//!
//! ```text
//!     .kt source ──► Kotlin IR ──► .klib (zip) ──► LLVM bitcode ──► native binary
//!                                  ^                ^
//!                                  │                │
//!                              kotlinc-native    Konan + LLVM
//! ```
//!
//! `skotch` mirrors that pipeline shape so the same fixture inputs we
//! use for the JVM and DEX targets can flow through a multi-stage
//! native pipeline:
//!
//! ```text
//!     .kt source ──► MIR ──► .klib (zip) ──► LLVM IR ──► clang ──► native binary
//!                            ^                ^         ^
//!                            │                │         │
//!                      this crate    skotch-backend-llvm  driver
//! ```
//!
//! ## Format
//!
//! A skotch `.klib` is a ZIP archive (matching kotlinc's choice). Its
//! contents intentionally use the same top-level layout names as
//! kotlinc-native's klibs so a contributor familiar with one is
//! oriented in the other:
//!
//! ```text
//!     default/
//!         manifest                  text "key=value\n" properties
//!         linkdata/
//!             module                module name
//!         ir/
//!             module.skir.json      serialized MirModule (skotch's IR)
//!             sources/<file>.kt     copy of the original source
//!         targets/
//!             <target>/
//!                 included/
//!                 native/
//! ```
//!
//! `module.skir.json` is **not** Kotlin's protobuf-encoded IR — it's
//! a JSON encoding of skotch's [`MirModule`]. Round-tripping a skotch klib
//! through [`read_klib`] and back through [`write_klib`] yields a
//! byte-equal file (modulo zip header timestamps, which we pin).
//!
//! The skotch klib is **not interchangeable** with a kotlinc-native
//! klib: their `module.skir.json` vs `default/ir/*.knb/.knd` payloads
//! are fundamentally different IRs. A future PR could write a
//! protobuf converter, but for the multi-stage validation goal of
//! PR #4 a JSON IR is enough.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use skotch_mir::MirModule;
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Properties stored in a klib's `default/manifest` file. Order matters
/// for byte-stable output, so we keep them as a fixed list rather than
/// a `HashMap`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KlibManifest {
    pub abi_version: String,
    pub compiler: String,
    pub compiler_version: String,
    /// Module name. For the PR #4 fixtures this is the wrapper class
    /// name in lowercase, matching the `unique_name` kotlinc emits.
    pub unique_name: String,
    /// Comma-separated target triples (e.g. `macos_arm64`).
    pub native_targets: String,
    pub depends: String,
    pub builtins_platform: String,
}

impl KlibManifest {
    /// Default manifest produced for skotch's klibs. The `compiler` key
    /// distinguishes ours from kotlinc-native's identically-named
    /// `default/manifest` file.
    pub fn for_module(module: &MirModule, target: &str) -> Self {
        KlibManifest {
            abi_version: "0.1.0".to_string(),
            compiler: "skotch".to_string(),
            compiler_version: env!("CARGO_PKG_VERSION").to_string(),
            unique_name: module.wrapper_class.to_lowercase(),
            native_targets: target.to_string(),
            depends: "stdlib".to_string(),
            builtins_platform: "NATIVE".to_string(),
        }
    }

    pub fn to_text(&self) -> String {
        // Properties-style format. Sorted by key for stability.
        let pairs: [(&str, &str); 7] = [
            ("abi_version", &self.abi_version),
            ("builtins_platform", &self.builtins_platform),
            ("compiler", &self.compiler),
            ("compiler_version", &self.compiler_version),
            ("depends", &self.depends),
            ("native_targets", &self.native_targets),
            ("unique_name", &self.unique_name),
        ];
        let mut out = String::new();
        for (k, v) in pairs {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
        out
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut abi_version = None;
        let mut builtins_platform = None;
        let mut compiler = None;
        let mut compiler_version = None;
        let mut depends = None;
        let mut native_targets = None;
        let mut unique_name = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| anyhow!("invalid manifest line: `{line}`"))?;
            match k {
                "abi_version" => abi_version = Some(v.to_string()),
                "builtins_platform" => builtins_platform = Some(v.to_string()),
                "compiler" => compiler = Some(v.to_string()),
                "compiler_version" => compiler_version = Some(v.to_string()),
                "depends" => depends = Some(v.to_string()),
                "native_targets" => native_targets = Some(v.to_string()),
                "unique_name" => unique_name = Some(v.to_string()),
                _ => {} // tolerate unknown keys for forward compatibility
            }
        }
        Ok(KlibManifest {
            abi_version: abi_version.ok_or_else(|| anyhow!("manifest missing abi_version"))?,
            builtins_platform: builtins_platform.unwrap_or_else(|| "NATIVE".to_string()),
            compiler: compiler.unwrap_or_else(|| "skotch".to_string()),
            compiler_version: compiler_version.unwrap_or_default(),
            depends: depends.unwrap_or_default(),
            native_targets: native_targets.unwrap_or_default(),
            unique_name: unique_name.ok_or_else(|| anyhow!("manifest missing unique_name"))?,
        })
    }
}

/// Default native target name used when the caller doesn't specify
/// one. Matches Kotlin Native's naming for Apple Silicon macOS.
pub const DEFAULT_TARGET: &str = "macos_arm64";

/// Serialize a [`MirModule`] into a `.klib` zip archive's bytes.
pub fn write_klib(module: &MirModule, target: &str) -> Result<Vec<u8>> {
    let manifest = KlibManifest::for_module(module, target);
    let mir_json =
        serde_json::to_vec_pretty(module).with_context(|| "serializing MirModule to JSON")?;

    let mut buf = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(&mut buf);
    // Pin file modes + timestamps so byte output is reproducible
    // across machines and CI runs.
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644)
        .last_modified_time(zip::DateTime::default());

    let dir_opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o755)
        .last_modified_time(zip::DateTime::default());

    // Mirror kotlinc-native's `default/...` directory layout. The
    // ordering here matters for byte-stable klibs: we always emit the
    // same files in the same order.
    zip.add_directory("default/", dir_opts)?;
    zip.add_directory("default/ir/", dir_opts)?;
    zip.add_directory("default/ir/sources/", dir_opts)?;
    zip.add_directory("default/linkdata/", dir_opts)?;
    zip.add_directory("default/resources/", dir_opts)?;
    zip.add_directory("default/targets/", dir_opts)?;
    zip.add_directory(format!("default/targets/{target}/"), dir_opts)?;
    zip.add_directory(format!("default/targets/{target}/included/"), dir_opts)?;
    zip.add_directory(format!("default/targets/{target}/native/"), dir_opts)?;

    zip.start_file("default/manifest", opts)?;
    zip.write_all(manifest.to_text().as_bytes())?;

    zip.start_file("default/linkdata/module", opts)?;
    zip.write_all(format!("<{}>: ", manifest.unique_name).as_bytes())?;

    zip.start_file("default/ir/module.skir.json", opts)?;
    zip.write_all(&mir_json)?;
    // Trailing newline so the file is text-friendly.
    zip.write_all(b"\n")?;

    zip.finish()?;
    Ok(buf.into_inner())
}

/// Read a skotch `.klib` and return the deserialized [`MirModule`] +
/// manifest. Errors if the klib was produced by a different compiler
/// (e.g. kotlinc-native) or is structurally invalid.
pub fn read_klib(bytes: &[u8]) -> Result<(MirModule, KlibManifest)> {
    let cur = Cursor::new(bytes);
    let mut zip = ZipArchive::new(cur).context("opening klib zip")?;

    let mut manifest_text = String::new();
    {
        let mut f = zip
            .by_name("default/manifest")
            .context("klib has no `default/manifest` entry")?;
        f.read_to_string(&mut manifest_text)?;
    }
    let manifest = KlibManifest::parse(&manifest_text)?;
    if manifest.compiler != "skotch" {
        return Err(anyhow!(
            "klib was produced by `{}`, not `skotch` — only skotch klibs are readable",
            manifest.compiler
        ));
    }

    let mut json = Vec::new();
    {
        let mut f = zip
            .by_name("default/ir/module.skir.json")
            .context("klib has no `default/ir/module.skir.json` entry")?;
        f.read_to_end(&mut json)?;
    }
    let module: MirModule =
        serde_json::from_slice(&json).context("deserializing skotch MIR from klib")?;
    Ok((module, manifest))
}

/// True if the given bytes look like a Kotlin Native klib (i.e. a zip
/// archive whose `default/manifest` declares `compiler` not equal to
/// `skotch`). Useful for `xtask` so it can refuse to mistake a kotlinc
/// klib for a skotch one.
pub fn is_kotlinc_native_klib(bytes: &[u8]) -> bool {
    let cur = Cursor::new(bytes);
    let Ok(mut zip) = ZipArchive::new(cur) else {
        return false;
    };
    let Ok(mut f) = zip.by_name("default/manifest") else {
        return false;
    };
    let mut s = String::new();
    if f.read_to_string(&mut s).is_err() {
        return false;
    }
    !s.contains("compiler=skotch")
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_intern::Interner;
    use skotch_lexer::lex;
    use skotch_mir_lower::lower_file;
    use skotch_parser::parse_file;
    use skotch_resolve::resolve_file;
    use skotch_span::FileId;
    use skotch_typeck::type_check;

    fn build_mir(src: &str) -> MirModule {
        let mut interner = Interner::new();
        let mut diags = skotch_diagnostics::Diagnostics::new();
        let lf = lex(FileId(0), src, &mut diags);
        let ast = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&ast, &mut interner, &mut diags);
        let t = type_check(&ast, &r, &mut interner, &mut diags);
        let m = lower_file(&ast, &r, &t, &mut interner, &mut diags, "InputKt");
        assert!(!diags.has_errors(), "{:?}", diags);
        m
    }

    #[test]
    fn write_klib_starts_with_pk() {
        let m = build_mir(r#"fun main() { println("Hi") }"#);
        let bytes = write_klib(&m, DEFAULT_TARGET).unwrap();
        // ZIP magic = "PK\003\004" for the first local file header.
        assert_eq!(&bytes[0..2], b"PK");
    }

    #[test]
    fn write_klib_contains_manifest_and_mir() {
        let m = build_mir(r#"fun main() { println("Hi") }"#);
        let bytes = write_klib(&m, DEFAULT_TARGET).unwrap();
        let needle1 = b"default/manifest";
        assert!(bytes.windows(needle1.len()).any(|w| w == needle1));
        let needle2 = b"default/ir/module.skir.json";
        assert!(bytes.windows(needle2.len()).any(|w| w == needle2));
        let needle3 = b"compiler=skotch";
        assert!(bytes.windows(needle3.len()).any(|w| w == needle3));
    }

    #[test]
    fn klib_round_trip_preserves_module() {
        let m = build_mir(r#"fun main() { println("Hello, world!") }"#);
        let bytes = write_klib(&m, DEFAULT_TARGET).unwrap();
        let (m2, manifest) = read_klib(&bytes).unwrap();
        assert_eq!(m.wrapper_class, m2.wrapper_class);
        assert_eq!(m.strings, m2.strings);
        assert_eq!(m.functions.len(), m2.functions.len());
        assert_eq!(manifest.compiler, "skotch");
        assert_eq!(manifest.native_targets, DEFAULT_TARGET);
    }

    #[test]
    fn klib_byte_stable_across_runs() {
        let m = build_mir(r#"fun main() { println("Hi") }"#);
        let a = write_klib(&m, DEFAULT_TARGET).unwrap();
        let b = write_klib(&m, DEFAULT_TARGET).unwrap();
        assert_eq!(a, b, "klib output should be deterministic");
    }

    #[test]
    fn manifest_round_trip() {
        let m = build_mir(r#"fun main() { println("Hi") }"#);
        let manifest = KlibManifest::for_module(&m, DEFAULT_TARGET);
        let text = manifest.to_text();
        let parsed = KlibManifest::parse(&text).unwrap();
        assert_eq!(parsed.compiler, manifest.compiler);
        assert_eq!(parsed.unique_name, manifest.unique_name);
        assert_eq!(parsed.native_targets, manifest.native_targets);
    }

    #[test]
    fn read_rejects_non_skotch_klib() {
        // Build a manifest with a different compiler and ensure
        // read_klib refuses to load it.
        let mut buf = Cursor::new(Vec::new());
        let mut zip = ZipWriter::new(&mut buf);
        zip.start_file("default/manifest", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"abi_version=1\ncompiler=kotlinc-native\nunique_name=foo\nnative_targets=macos_arm64\n").unwrap();
        zip.finish().unwrap();
        let bytes = buf.into_inner();
        let err = read_klib(&bytes).unwrap_err();
        assert!(err.to_string().contains("kotlinc-native"));
    }
}
