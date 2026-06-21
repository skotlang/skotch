//! `aapt2 convert` — converts an APK between binary and proto formats.
//!
//! Port of `cmd/Convert.cpp`.

use crate::apk::{ApkFormat, LoadedApk, MANIFEST_PATH, TABLE_BINARY_PATH, TABLE_PROTO_PATH};
use crate::cli::ParsedArgs;
use crate::diag::Diagnostics;
use crate::link::OutputFormat;
use crate::xml::flatten::{flatten_xml, XmlFlattenerOptions};
use anyhow::{anyhow, bail, Result};
use std::io::Write as _;
use std::path::Path;

pub fn run(args: &[String], diag: &Diagnostics) -> Result<i32> {
    let parsed = ParsedArgs::parse(
        args,
        &["-o", "--output-format", "--resources-config-path"],
        &[
            "--keep-raw-values",
            "--enable-sparse-encoding",
            "--force-sparse-encoding",
            "--enable-compact-entries",
            "--collapse-resource-names",
            "--deduplicate-entry-values",
            "-v",
        ],
    )?;
    let output = parsed
        .value("-o")
        .ok_or_else(|| anyhow!("-o flag is required"))?;
    let output_format = match parsed.value("--output-format").unwrap_or("binary") {
        "binary" => OutputFormat::Binary,
        "proto" => OutputFormat::Proto,
        other => bail!("invalid value for flag --output-format: {other}"),
    };
    if parsed.positional.len() != 1 {
        bail!("must supply exactly one proto APK");
    }
    let input = Path::new(&parsed.positional[0]);
    let apk = LoadedApk::load(input, diag)?;
    convert_apk(
        &apk,
        Path::new(output),
        output_format,
        parsed.has("--keep-raw-values"),
        parsed.has("--enable-sparse-encoding") || parsed.has("--force-sparse-encoding"),
        parsed.has("--enable-compact-entries"),
        diag,
    )?;
    Ok(0)
}

/// Converts a loaded APK to the requested format, writing a new zip.
pub fn convert_apk(
    apk: &LoadedApk,
    output_path: &Path,
    output_format: OutputFormat,
    keep_raw_values: bool,
    sparse: bool,
    compact: bool,
    diag: &Diagnostics,
) -> Result<()> {
    if apk.format == ApkFormat::None {
        bail!("{}: APK has no resource table", apk.source);
    }

    let file = std::fs::File::create(output_path)?;
    let mut writer = zip::ZipWriter::new(file);

    for (name, data) in apk.entries() {
        // Table files are re-emitted below.
        if name == TABLE_BINARY_PATH || name == TABLE_PROTO_PATH {
            continue;
        }
        let converted: Option<Vec<u8>> = if name == MANIFEST_PATH || is_compiled_xml(apk, name) {
            // Convert the XML format if needed.
            let root = match apk.format {
                ApkFormat::Proto => crate::xml::decode_pb_xml(data).ok(),
                _ => crate::xml::axml::parse_binary_xml(data).ok(),
            };
            match (root, output_format) {
                (Some(root), OutputFormat::Binary) => Some(flatten_xml(
                    &root,
                    &XmlFlattenerOptions {
                        keep_raw_values,
                        use_utf16: true,
                    },
                )),
                (Some(root), OutputFormat::Proto) => Some(crate::xml::encode_pb_xml(&root)),
                (None, _) => {
                    diag.warn(format!(
                        "{name}: failed to parse compiled XML; copying as-is"
                    ));
                    None
                }
            }
        } else {
            None
        };

        let payload = converted.as_deref().unwrap_or(data);
        let stored = name == "resources.arsc" || !should_compress_entry(name);
        let mut options = zip::write::SimpleFileOptions::default();
        options = if stored {
            options
                .compression_method(zip::CompressionMethod::Stored)
                .with_alignment(4)
        } else {
            options.compression_method(zip::CompressionMethod::Deflated)
        };
        writer.start_file(name, options)?;
        writer.write_all(payload)?;
    }

    // The resource table: file types inside must reflect the new format.
    let mut table = match apk.format {
        ApkFormat::Proto => crate::pb::decode_table(
            apk.entry(TABLE_PROTO_PATH)
                .ok_or_else(|| anyhow!("missing {TABLE_PROTO_PATH}"))?,
        )?,
        _ => crate::binary::arsc_parser::parse_table(
            apk.entry(TABLE_BINARY_PATH)
                .ok_or_else(|| anyhow!("missing {TABLE_BINARY_PATH}"))?,
        )?,
    };
    retarget_file_types(&mut table, output_format);

    match output_format {
        OutputFormat::Binary => {
            let arsc = crate::binary::arsc_flattener::flatten_table(
                &table,
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
        }
        OutputFormat::Proto => {
            let table_pb =
                crate::pb::encode_table(&table, &crate::pb::SerializeTableOptions::default());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file(TABLE_PROTO_PATH, options)?;
            writer.write_all(&table_pb)?;
        }
    }
    writer.finish()?;
    Ok(())
}

fn is_compiled_xml(apk: &LoadedApk, name: &str) -> bool {
    if !name.starts_with("res/") || !name.ends_with(".xml") {
        return false;
    }
    // Raw XML under res/raw is not compiled.
    !name.starts_with("res/raw")
        && apk.entry(name).is_some_and(|data| {
            data.starts_with(&[0x03, 0x00]) || matches!(apk.format, ApkFormat::Proto)
        })
}

fn should_compress_entry(name: &str) -> bool {
    // Sound/video formats are stored by convention.
    const STORED: &[&str] = &[
        ".jpg", ".jpeg", ".png", ".gif", ".opus", ".wav", ".mp2", ".mp3", ".ogg", ".aac", ".mpg",
        ".mpeg", ".mid", ".midi", ".smf", ".jet", ".rtttl", ".imy", ".xmf", ".mp4", ".m4a", ".m4v",
        ".3gp", ".3gpp", ".3g2", ".3gpp2", ".amr", ".awb", ".wma", ".wmv", ".webm", ".mkv",
    ];
    !STORED.iter().any(|ext| name.ends_with(ext))
}

fn retarget_file_types(table: &mut crate::res::table::ResourceTable, format: OutputFormat) {
    use crate::res::value::{FileType, Item, ValueKind};
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                for config_value in &mut entry.values {
                    if let Some(value) = &mut config_value.value {
                        if let ValueKind::Item(Item::FileReference(file)) = &mut value.kind {
                            if file.path.ends_with(".xml") {
                                file.file_type = match format {
                                    OutputFormat::Binary => FileType::BinaryXml,
                                    OutputFormat::Proto => FileType::ProtoXml,
                                };
                            }
                        }
                    }
                }
            }
        }
    }
}
