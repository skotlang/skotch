//! Loaded APK abstraction.
//!
//! Port of `LoadedApk.{h,cpp}`: opens an APK (or `android.jar`-style
//! zip with embedded resources), detects whether its resources are in
//! binary (`resources.arsc` + binary XML) or proto (`resources.pb` +
//! proto XML) format, and exposes the resource table, manifest, and
//! file entries.

use crate::diag::Diagnostics;
use crate::res::table::ResourceTable;
use crate::xml::Element;
use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::path::Path;

pub const MANIFEST_PATH: &str = "AndroidManifest.xml";
pub const TABLE_BINARY_PATH: &str = "resources.arsc";
pub const TABLE_PROTO_PATH: &str = "resources.pb";

/// Resource serialization format of an APK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApkFormat {
    Binary,
    Proto,
    /// Neither table file is present (e.g. a plain jar or an APK with
    /// no resources).
    None,
}

/// An APK opened for reading: all entries are held in memory.
pub struct LoadedApk {
    pub source: String,
    pub format: ApkFormat,
    /// Entry name → bytes, in zip order.
    entries: Vec<(String, Vec<u8>)>,
    /// The decoded resource table (empty when the APK has none).
    pub table: ResourceTable,
    /// The decoded manifest, if present.
    pub manifest: Option<Element>,
}

impl LoadedApk {
    /// Opens an APK file from disk.
    pub fn load(path: &Path, diag: &Diagnostics) -> Result<LoadedApk> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("failed to open APK '{}'", path.display()))?;
        let mut archive = zip::ZipArchive::new(file)
            .with_context(|| format!("failed to open APK '{}'", path.display()))?;
        let mut entries = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.is_dir() {
                continue;
            }
            let mut data = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut data)?;
            entries.push((entry.name().to_string(), data));
        }
        Self::from_entries(path.to_string_lossy().into_owned(), entries, diag)
    }

    /// Builds a `LoadedApk` from in-memory entries.
    pub fn from_entries(
        source: String,
        entries: Vec<(String, Vec<u8>)>,
        diag: &Diagnostics,
    ) -> Result<LoadedApk> {
        let find = |name: &str| entries.iter().find(|(n, _)| n == name).map(|(_, d)| d);

        let format = if find(TABLE_PROTO_PATH).is_some() {
            ApkFormat::Proto
        } else if find(TABLE_BINARY_PATH).is_some() {
            ApkFormat::Binary
        } else {
            ApkFormat::None
        };

        let table = match format {
            ApkFormat::Proto => {
                let data = find(TABLE_PROTO_PATH).unwrap();
                crate::pb::decode_table(data)
                    .with_context(|| format!("failed to parse {TABLE_PROTO_PATH} in {source}"))?
            }
            ApkFormat::Binary => {
                let data = find(TABLE_BINARY_PATH).unwrap();
                crate::binary::arsc_parser::parse_table(data)
                    .with_context(|| format!("failed to parse {TABLE_BINARY_PATH} in {source}"))?
            }
            ApkFormat::None => ResourceTable::new_unvalidated(),
        };

        let manifest = match find(MANIFEST_PATH) {
            Some(data) => {
                let parsed = match format {
                    ApkFormat::Proto => crate::xml::decode_pb_xml(data)
                        .map_err(|e| anyhow!("failed to parse proto manifest in {source}: {e}")),
                    _ => crate::xml::axml::parse_binary_xml(data).map_err(|e| {
                        anyhow!("failed to parse binary manifest in {source}: {e}")
                    }),
                };
                match parsed {
                    Ok(root) => Some(root),
                    Err(e) => {
                        // Some inputs (android.jar without compiled
                        // manifest) carry a plain-text manifest; tolerate.
                        if let Ok(text) = std::str::from_utf8(find(MANIFEST_PATH).unwrap()) {
                            match crate::xml::parse_source_xml(MANIFEST_PATH, text) {
                                Ok(res) => res.root,
                                Err(_) => {
                                    diag.warn(format!("{e}"));
                                    None
                                }
                            }
                        } else {
                            diag.warn(format!("{e}"));
                            None
                        }
                    }
                }
            }
            None => None,
        };

        Ok(LoadedApk { source, format, entries, table, manifest })
    }

    pub fn entry(&self, name: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_slice())
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.entries.iter().map(|(n, d)| (n.as_str(), d.as_slice()))
    }

    pub fn manifest(&self) -> Result<&Element> {
        self.manifest
            .as_ref()
            .ok_or_else(|| anyhow!("{}: no AndroidManifest.xml", self.source))
    }

    /// Writes a copy of this APK with `table` substituted as the
    /// (binary) resource table. Used by `optimize`.
    pub fn write_with_table(
        &self,
        table: &ResourceTable,
        output: &Path,
        sparse: bool,
        compact: bool,
    ) -> Result<()> {
        use std::io::Write as _;
        let file = std::fs::File::create(output)
            .with_context(|| format!("failed to create {}", output.display()))?;
        let mut writer = zip::ZipWriter::new(file);
        for (name, data) in self.entries() {
            if name == TABLE_BINARY_PATH || name == TABLE_PROTO_PATH {
                continue;
            }
            let stored = name == MANIFEST_PATH || name.ends_with(".png");
            let mut options = zip::write::SimpleFileOptions::default();
            options = if stored {
                options
                    .compression_method(zip::CompressionMethod::Stored)
                    .with_alignment(4)
            } else {
                options.compression_method(zip::CompressionMethod::Deflated)
            };
            writer.start_file(name, options)?;
            writer.write_all(data)?;
        }
        let arsc = crate::binary::arsc_flattener::flatten_table(
            table,
            &crate::binary::arsc_flattener::TableFlattenerOptions {
                use_sparse_entries: sparse,
                use_compact_entries: compact,
                collapse_key_stringpool: false,
            },
        )?;
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .with_alignment(4);
        writer.start_file(TABLE_BINARY_PATH, options)?;
        writer.write_all(&arsc)?;
        writer.finish()?;
        Ok(())
    }

    /// The app package name from the manifest.
    pub fn package_name(&self) -> Result<String> {
        let manifest = self.manifest()?;
        if manifest.name != "manifest" {
            bail!("{}: root tag of AndroidManifest.xml is not <manifest>", self.source);
        }
        manifest
            .attr_value("", "package")
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow!("{}: AndroidManifest.xml has no 'package' attribute", self.source)
            })
    }
}
