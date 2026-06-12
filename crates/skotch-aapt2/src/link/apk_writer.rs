//! Final APK assembly for `link`.
//!
//! Port of `Linker::WriteApk` + `ResourceFileFlattener`: processes
//! every file reference in the table (linking + flattening XML),
//! processes the manifest, flattens the resource table, copies assets,
//! and writes the zip (or directory with `--output-to-dir`).

use super::java_gen::{KeepSet, XmlKind};
use super::{reference_linker, LinkContext, OutputFormat};
use crate::diag::Diagnostics;
use crate::res::table::ResourceTable;
use crate::res::value::{FileType, Item, ValueKind};
use crate::res::ResourceType;
use crate::xml::flatten::{flatten_xml, XmlFlattenerOptions};
use anyhow::{anyhow, bail, Context, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct ApkWriterOptions {
    pub output_format: OutputFormat,
    pub output_to_directory: bool,
    pub keep_raw_values: bool,
    pub no_xml_namespaces: bool,
    pub extensions_to_not_compress: Vec<String>,
    pub do_not_compress_anything: bool,
    pub regex_to_not_compress: Option<String>,
    pub enable_sparse_encoding: bool,
    pub force_sparse_encoding: bool,
    pub enable_compact_entries: bool,
    pub merge_only: bool,
}

/// Whether an entry should be compressed. Mirrors
/// `GetCompressionFlags`.
fn should_compress(path: &str, options: &ApkWriterOptions) -> bool {
    if options.do_not_compress_anything {
        return false;
    }
    if let Some(regex) = &options.regex_to_not_compress {
        // ECMAScript regex search ≈ substring regex match.
        if simple_regex_search(regex, path) {
            return false;
        }
    }
    for extension in &options.extensions_to_not_compress {
        if path.ends_with(extension.as_str()) {
            return false;
        }
    }
    true
}

/// Minimal regex support for `--no-compress-regex`: supports literal
/// text, `.`, `.*`, `$` anchors, and alternation `|` — the patterns
/// build systems actually pass. Full ECMAScript syntax is out of scope.
fn simple_regex_search(pattern: &str, text: &str) -> bool {
    pattern.split('|').any(|alternative| {
        let (alternative, anchored_end) = match alternative.strip_suffix('$') {
            Some(rest) => (rest, true),
            None => (alternative, false),
        };
        let literal = alternative.replace(".*", "\u{0}");
        let parts: Vec<&str> = literal.split('\u{0}').collect();
        // Check the parts appear in order.
        let mut position = 0usize;
        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            // Treat '.' as a single wildcard by splitting again.
            let found = find_with_dot(text, part, position);
            match found {
                Some(at) => position = at + part.len(),
                None => return false,
            }
            if anchored_end && index == parts.len() - 1 {
                return text.len() == position;
            }
        }
        if anchored_end {
            text.len() == position
        } else {
            true
        }
    })
}

fn find_with_dot(text: &str, pattern: &str, from: usize) -> Option<usize> {
    let text_bytes = text.as_bytes();
    let pattern_bytes = pattern.as_bytes();
    if from > text_bytes.len() {
        return None;
    }
    'outer: for start in from..=text_bytes.len().saturating_sub(pattern_bytes.len()) {
        for (offset, &p) in pattern_bytes.iter().enumerate() {
            let t = text_bytes[start + offset];
            if p != b'.' && p != t {
                continue 'outer;
            }
        }
        return Some(start);
    }
    None
}

/// An output sink: zip file or directory.
enum Sink {
    Zip(zip::ZipWriter<std::fs::File>),
    Directory(PathBuf),
}

impl Sink {
    fn add(&mut self, name: &str, data: &[u8], compress: bool, align: u16) -> Result<()> {
        match self {
            Sink::Zip(writer) => {
                let mut options = zip::write::SimpleFileOptions::default();
                options = if compress {
                    options.compression_method(zip::CompressionMethod::Deflated)
                } else {
                    options.compression_method(zip::CompressionMethod::Stored)
                };
                if !compress && align > 1 {
                    options = options.with_alignment(align);
                }
                writer.start_file(name, options)?;
                writer.write_all(data)?;
            }
            Sink::Directory(dir) => {
                let path = dir.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, data)?;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<()> {
        if let Sink::Zip(writer) = self {
            writer.finish()?;
        }
        Ok(())
    }
}

/// Writes the complete APK. Mirrors `Linker::WriteApk`.
#[allow(clippy::too_many_arguments)]
pub fn write_apk(
    output_path: &Path,
    table: &mut ResourceTable,
    manifest: &mut crate::xml::Element,
    context: &LinkContext,
    options: &ApkWriterOptions,
    assets_dirs: &[PathBuf],
    keep_set: &mut KeepSet,
    diag: &Diagnostics,
) -> Result<()> {
    let mut sink = if options.output_to_directory {
        std::fs::create_dir_all(output_path)?;
        Sink::Directory(output_path.to_path_buf())
    } else {
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Sink::Zip(zip::ZipWriter::new(std::fs::File::create(output_path).with_context(
            || format!("failed to create {}", output_path.display()),
        )?))
    };

    // ── Manifest ───────────────────────────────────────────────────
    if !options.merge_only {
        reference_linker::link_xml_references(
            manifest,
            &context.symbols,
            &context.compilation_package,
            "AndroidManifest.xml",
            diag,
        )?;
    }
    keep_set.collect_from_xml(manifest, "AndroidManifest.xml", XmlKind::Manifest);
    let manifest_bytes = match options.output_format {
        OutputFormat::Binary => flatten_xml(
            manifest,
            &XmlFlattenerOptions { keep_raw_values: options.keep_raw_values, use_utf16: true },
        ),
        OutputFormat::Proto => crate::xml::encode_pb_xml(manifest),
    };
    sink.add("AndroidManifest.xml", &manifest_bytes, true, 1)?;

    // ── Resource files ─────────────────────────────────────────────
    // Collect (dest path, contents, type, source name) sorted by
    // (config, name) for zip locality, mirroring ResourceFileFlattener.
    struct PendingEntry {
        dest: String,
        contents: std::sync::Arc<Vec<u8>>,
        file_type: FileType,
        resource_type: ResourceType,
        config_sort_key: Vec<u8>,
        name: String,
    }
    let mut pending: Vec<PendingEntry> = Vec::new();
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                for config_value in &mut entry.values {
                    let Some(value) = &mut config_value.value else { continue };
                    let ValueKind::Item(Item::FileReference(file_reference)) = &mut value.kind
                    else {
                        continue;
                    };
                    let Some(contents) = file_reference.file_contents.clone() else {
                        // Pre-existing path-only reference (e.g. merged
                        // from an APK); nothing to write.
                        continue;
                    };
                    pending.push(PendingEntry {
                        dest: file_reference.path.clone(),
                        contents,
                        file_type: file_reference.file_type,
                        resource_type: ty.named_type.ty,
                        config_sort_key: config_value.config.to_bytes(),
                        name: entry.name.clone(),
                    });
                    // After packaging, XML files become binary/proto.
                    if file_reference.file_type == FileType::ProtoXml {
                        file_reference.file_type = match options.output_format {
                            OutputFormat::Binary => FileType::BinaryXml,
                            OutputFormat::Proto => FileType::ProtoXml,
                        };
                    }
                }
            }
        }
    }
    pending.sort_by(|a, b| {
        a.config_sort_key
            .cmp(&b.config_sort_key)
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut seen = std::collections::HashSet::new();
    for entry in pending {
        if !seen.insert(entry.dest.clone()) {
            continue;
        }
        let data: Vec<u8> = match entry.file_type {
            FileType::ProtoXml => {
                let mut root = crate::xml::decode_pb_xml(&entry.contents)
                    .with_context(|| format!("failed to parse compiled XML for {}", entry.dest))?;
                if !options.merge_only {
                    reference_linker::link_xml_references(
                        &mut root,
                        &context.symbols,
                        &context.compilation_package,
                        &entry.dest,
                        diag,
                    )?;
                }
                let kind = match entry.resource_type {
                    ResourceType::Layout => XmlKind::Layout,
                    _ => XmlKind::Other,
                };
                keep_set.collect_from_xml(&root, &entry.dest, kind);
                match options.output_format {
                    OutputFormat::Binary => flatten_xml(
                        &root,
                        &XmlFlattenerOptions {
                            keep_raw_values: options.keep_raw_values,
                            use_utf16: true,
                        },
                    ),
                    OutputFormat::Proto => crate::xml::encode_pb_xml(&root),
                }
            }
            _ => entry.contents.as_ref().clone(),
        };
        let compress = should_compress(&entry.dest, options);
        sink.add(&entry.dest, &data, compress, 4)?;
    }

    // ── Assets ─────────────────────────────────────────────────────
    let mut merged_assets: std::collections::BTreeMap<String, PathBuf> = Default::default();
    for assets_dir in assets_dirs {
        collect_assets(assets_dir, assets_dir, &mut merged_assets, diag)?;
    }
    for (key, path) in merged_assets {
        let data = std::fs::read(&path)
            .with_context(|| format!("failed to read asset {}", path.display()))?;
        let compress = should_compress(&key, options);
        sink.add(&key, &data, compress, 4)?;
    }

    // ── Resource table ─────────────────────────────────────────────
    match options.output_format {
        OutputFormat::Binary => {
            let flattener_options = crate::binary::arsc_flattener::TableFlattenerOptions {
                use_sparse_entries: options.enable_sparse_encoding || options.force_sparse_encoding,
                use_compact_entries: options.enable_compact_entries,
                collapse_key_stringpool: false,
            };
            let arsc = crate::binary::arsc_flattener::flatten_table(table, &flattener_options)?;
            // resources.arsc must be stored uncompressed and 4-byte
            // aligned for the runtime to mmap it.
            sink.add("resources.arsc", &arsc, false, 4)?;
        }
        OutputFormat::Proto => {
            let table_pb = crate::pb::encode_table(table, &crate::pb::SerializeTableOptions::default());
            sink.add("resources.pb", &table_pb, true, 1)?;
        }
    }

    sink.finish()?;
    if diag.has_errors() {
        bail!("errors while writing APK");
    }
    Ok(())
}

fn collect_assets(
    root: &Path,
    dir: &Path,
    out: &mut std::collections::BTreeMap<String, PathBuf>,
    diag: &Diagnostics,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("failed to read assets dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_assets(root, &path, out, diag)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| anyhow!("asset path escapes root"))?
                .to_string_lossy()
                .replace('\\', "/");
            let key = format!("assets/{relative}");
            if out.insert(key.clone(), path.clone()).is_some() {
                diag.warn(format!("asset file overrides '{}'", path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_rules() {
        let mut options = ApkWriterOptions::default();
        assert!(should_compress("res/layout/main.xml", &options));
        options.extensions_to_not_compress = vec![".png".to_string()];
        assert!(!should_compress("res/drawable/icon.png", &options));
        assert!(should_compress("res/layout/main.xml", &options));
        options.do_not_compress_anything = true;
        assert!(!should_compress("res/layout/main.xml", &options));
    }

    #[test]
    fn regex_no_compress() {
        assert!(simple_regex_search(".*\\.png$".replace("\\.", ".").as_str(), "res/a.png"));
        assert!(simple_regex_search("ogg$|mp3$", "sounds/x.mp3"));
        assert!(!simple_regex_search("ogg$|mp3$", "sounds/x.wav"));
    }
}
