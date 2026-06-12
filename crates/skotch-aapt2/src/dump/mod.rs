//! `aapt2 dump` — prints information from APKs and containers.
//!
//! Port of `cmd/Dump.cpp`/`Dump.h` (dispatch + flags),
//! `dump/DumpManifest.cpp` (badging/permissions), `Debug.cpp`
//! (table/xml/chunk printers), and `cmd/Diff.cpp` (`aapt2 diff`).

mod chunks;
mod diff;
pub mod manifest;
mod printer;
mod table_printer;
mod values;
mod xmltree;

use crate::apk::{ApkFormat, LoadedApk};
use crate::cli::ParsedArgs;
use crate::diag::Diagnostics;
use crate::res::config::ConfigDescription;
use crate::res::string_pool::BinaryStringPool;
use crate::res::value::FileType;
use anyhow::{bail, Result};
use manifest::{dump_manifest, DumpManifestOptions};
use printer::Printer;
use std::path::Path;
use table_printer::DebugPrintTableOptions;

/// The badger easter egg shown when users type `dump badger`.
const BADGER_DATA: &str = include_str!("badger.txt");

pub fn run(args: &[String], diag: &Diagnostics) -> Result<i32> {
    let Some((subcommand, rest)) = args.split_first() else {
        bail!("no dump subcommand specified");
    };

    // Output is accumulated and flushed even when a command fails
    // midway, mirroring aapt2's buffered stdout printer.
    let mut out = String::new();
    let result = {
        let mut printer = Printer::new(&mut out);
        run_subcommand(subcommand, rest, &mut printer, diag)
    };
    print!("{out}");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    result
}

fn run_subcommand(
    subcommand: &str,
    rest: &[String],
    printer: &mut Printer,
    diag: &Diagnostics,
) -> Result<i32> {
    match subcommand {
        "apc" => run_apc(rest, printer, diag),
        "badging" => {
            let parsed = ParsedArgs::parse(rest, &[], &["--include-meta-data", "--proto"])?;
            if parsed.has("--proto") {
                bail!("'dump badging --proto' output is not supported by skotch aapt2");
            }
            let options = DumpManifestOptions {
                include_meta_data: parsed.has("--include-meta-data"),
                only_permissions: false,
            };
            for_each_apk(&parsed, diag, printer, |apk, printer, diag| {
                dump_manifest(apk, options, printer, diag)
            })
        }
        "configurations" => {
            let parsed = ParsedArgs::parse(rest, &[], &[])?;
            for_each_apk(&parsed, diag, printer, |apk, printer, _diag| {
                dump_configurations(apk, printer)
            })
        }
        "packagename" => {
            let parsed = ParsedArgs::parse(rest, &[], &[])?;
            for_each_apk(&parsed, diag, printer, |apk, printer, diag| {
                match get_package_name(apk, diag) {
                    Some(package_name) => {
                        printer.println(package_name);
                        0
                    }
                    None => 1,
                }
            })
        }
        "permissions" => {
            let parsed = ParsedArgs::parse(rest, &[], &[])?;
            let options = DumpManifestOptions {
                include_meta_data: false,
                only_permissions: true,
            };
            for_each_apk(&parsed, diag, printer, |apk, printer, diag| {
                dump_manifest(apk, options, printer, diag)
            })
        }
        "strings" => {
            let parsed = ParsedArgs::parse(rest, &[], &[])?;
            for_each_apk(&parsed, diag, printer, |apk, printer, diag| {
                dump_strings(apk, printer, diag)
            })
        }
        "styleparents" => {
            let parsed = ParsedArgs::parse(rest, &["--style"], &[])?;
            let Some(style) = parsed.value("--style").map(str::to_string) else {
                bail!("flag '--style' is required");
            };
            for_each_apk(&parsed, diag, printer, |apk, printer, diag| {
                let Some(package_name) = get_package_name(apk, diag) else {
                    return 1;
                };
                let target_style = table_printer::style_resource_name(&package_name, &style);
                if apk.table.find_resource(&target_style).is_none() {
                    diag.error(format!("Target style \"{}\" does not exist", target_style.entry));
                    return 1;
                }
                table_printer::print_style_graph(&apk.table, target_style, printer);
                0
            })
        }
        "resources" => {
            let parsed = ParsedArgs::parse(rest, &[], &["--no-values", "-v"])?;
            let options = DebugPrintTableOptions {
                show_sources: true,
                show_values: !parsed.has("--no-values"),
            };
            for_each_apk(&parsed, diag, printer, |apk, printer, _diag| {
                if apk.format == ApkFormat::Proto {
                    printer.println("Proto APK");
                } else {
                    printer.println("Binary APK");
                }
                table_printer::print_table(&apk.table, options, printer);
                0
            })
        }
        "chunks" => {
            let parsed = ParsedArgs::parse(rest, &[], &[])?;
            for_each_apk(&parsed, diag, printer, |apk, printer, diag| {
                let Some(data) = apk.entry(crate::apk::TABLE_BINARY_PATH) else {
                    diag.error("Failed to find resources.arsc in APK");
                    return 1;
                };
                chunks::dump_chunks(data, printer, diag);
                0
            })
        }
        "xmlstrings" => {
            let parsed = ParsedArgs::parse(rest, &["--file"], &[])?;
            let files: Vec<String> = parsed.values("--file").iter().map(|s| s.to_string()).collect();
            for_each_apk(&parsed, diag, printer, |apk, printer, diag| {
                dump_xml_strings(apk, &files, printer, diag)
            })
        }
        "xmltree" => {
            let parsed = ParsedArgs::parse(rest, &["--file"], &[])?;
            let files: Vec<String> = parsed.values("--file").iter().map(|s| s.to_string()).collect();
            for_each_apk(&parsed, diag, printer, |apk, printer, diag| {
                for file in &files {
                    let Some(root) = load_xml(apk, file) else {
                        diag.error(format!("failed to load file '{file}' in APK"));
                        return 1;
                    };
                    xmltree::dump_xml(&root, printer);
                }
                0
            })
        }
        "overlayable" => {
            let parsed = ParsedArgs::parse(rest, &[], &[])?;
            for_each_apk(&parsed, diag, printer, |apk, printer, _diag| {
                table_printer::dump_overlayable(&apk.table, printer);
                0
            })
        }
        "badger" => {
            // Easter egg preserved from aapt2.
            printer.print(BADGER_DATA);
            printer.print("Did you mean \"aapt2 dump badging\"?\n");
            Ok(1)
        }
        other => bail!("unknown subcommand '{other}'"),
    }
}

/// `DumpApkCommand::GetPackageName`.
fn get_package_name(apk: &LoadedApk, diag: &Diagnostics) -> Option<String> {
    let Some(manifest) = &apk.manifest else {
        diag.error("No AndroidManifest.");
        return None;
    };
    match manifest.attr_value("", "package") {
        Some(package) => Some(package.to_string()),
        None => {
            diag.error("No package name.");
            None
        }
    }
}

/// `DumpApkCommand::Action`: loads each positional APK and applies `f`.
fn for_each_apk(
    parsed: &ParsedArgs,
    diag: &Diagnostics,
    printer: &mut Printer,
    mut f: impl FnMut(&LoadedApk, &mut Printer, &Diagnostics) -> i32,
) -> Result<i32> {
    if parsed.positional.is_empty() {
        diag.error("No dump apk specified.");
        return Ok(1);
    }
    let mut error = false;
    for path in &parsed.positional {
        match LoadedApk::load(Path::new(path), diag) {
            Ok(apk) => error |= f(&apk, printer, diag) != 0,
            Err(e) => {
                diag.error(format!("{e:#}"));
                error = true;
            }
        }
    }
    Ok(if error { 1 } else { 0 })
}

/// Loads an XML file from an APK in either binary or proto form
/// (`LoadedApk::LoadXml`).
fn load_xml(apk: &LoadedApk, path: &str) -> Option<crate::xml::Element> {
    let data = apk.entry(path)?;
    match apk.format {
        ApkFormat::Proto => crate::xml::decode_pb_xml(data).ok(),
        _ => crate::xml::axml::parse_binary_xml(data).ok(),
    }
}

/// `DumpConfigsCommand::Dump`.
fn dump_configurations(apk: &LoadedApk, printer: &mut Printer) -> i32 {
    let mut configs: Vec<ConfigDescription> = Vec::new();
    for package in &apk.table.packages {
        for ty in &package.types {
            for entry in &ty.entries {
                for value in &entry.values {
                    configs.push(value.config);
                }
            }
        }
    }
    configs.sort_by(|a, b| a.compare(b));
    configs.dedup_by(|a, b| a.compare(b) == std::cmp::Ordering::Equal);
    for config in &configs {
        printer.print(format!("{config}\n"));
    }
    0
}

/// `DumpStringsCommand::Dump`. For binary APKs the global value string
/// pool of `resources.arsc` is printed; proto APKs carry no flattened
/// pool, which aapt2 reconstructs in memory — unsupported here.
fn dump_strings(apk: &LoadedApk, printer: &mut Printer, diag: &Diagnostics) -> i32 {
    match apk.format {
        ApkFormat::Binary => {
            let Some(data) = apk.entry(crate::apk::TABLE_BINARY_PATH) else {
                diag.error("Failed to retrieve resource table");
                return 1;
            };
            // Skip the ResTable_header and find the global string pool.
            let header_size = data
                .get(2..4)
                .map(|b| u16::from_le_bytes(b.try_into().unwrap()) as usize)
                .unwrap_or(12);
            let Some(chunk) = table_printer::find_string_pool_chunk(data, header_size) else {
                printer.print("String pool is uninitialized.\n");
                return 0;
            };
            match BinaryStringPool::parse(chunk) {
                Some(pool) => {
                    table_printer::dump_res_string_pool(&pool, chunk.len(), printer);
                    0
                }
                None => {
                    printer.print("String pool is corrupt/invalid.\n");
                    0
                }
            }
        }
        _ => {
            diag.error("dump strings is only supported for binary-format APKs");
            1
        }
    }
}

/// `DumpXmlStringsCommand::Dump`.
fn dump_xml_strings(
    apk: &LoadedApk,
    files: &[String],
    printer: &mut Printer,
    diag: &Diagnostics,
) -> i32 {
    let mut error = false;
    for xml_file in files {
        let data: Option<Vec<u8>> = match apk.format {
            ApkFormat::Proto => {
                // Flatten the proto XML to get a binary representation.
                match load_xml(apk, xml_file) {
                    Some(root) => Some(crate::xml::flatten::flatten_xml(
                        &root,
                        &crate::xml::flatten::XmlFlattenerOptions {
                            keep_raw_values: true,
                            ..Default::default()
                        },
                    )),
                    None => {
                        diag.error(format!("failed to load file '{xml_file}' in APK"));
                        error = true;
                        continue;
                    }
                }
            }
            ApkFormat::Binary => match apk.entry(xml_file) {
                Some(data) => Some(data.to_vec()),
                None => {
                    diag.error(format!("File '{xml_file}' not found in APK"));
                    error = true;
                    continue;
                }
            },
            ApkFormat::None => {
                diag.error(format!("{}: Unknown APK format", apk.source));
                error = true;
                continue;
            }
        };
        let Some(data) = data else { continue };
        // The string pool is the first chunk after the 8-byte XML header.
        match table_printer::find_string_pool_chunk(&data, 8)
            .and_then(|chunk| BinaryStringPool::parse(chunk).map(|p| (p, chunk.len())))
        {
            Some((pool, size)) => table_printer::dump_res_string_pool(&pool, size, printer),
            None => {
                printer.print("String pool is uninitialized.\n");
            }
        }
    }
    if error {
        1
    } else {
        0
    }
}

// ───────────────────────── dump apc ─────────────────────────

fn resource_file_type_to_string(file_type: FileType) -> &'static str {
    match file_type {
        FileType::Png => "PNG",
        FileType::BinaryXml => "BINARY_XML",
        FileType::ProtoXml => "PROTO_XML",
        FileType::Unknown => "UNKNOWN",
    }
}

/// `DumpAPCCommand::Action`: walks AAPT2 container (`.apc`/`.flat`)
/// entries, tracking the payload offsets that `DumpCompiledFile`
/// reports.
fn run_apc(args: &[String], printer: &mut Printer, diag: &Diagnostics) -> Result<i32> {
    let parsed = ParsedArgs::parse(args, &[], &["--no-values", "-v"])?;
    let print_options = DebugPrintTableOptions {
        show_sources: true,
        show_values: !parsed.has("--no-values"),
    };

    if parsed.positional.is_empty() {
        diag.error("No dump container specified");
        return Ok(1);
    }

    let mut error = false;
    for container in &parsed.positional {
        let data = match std::fs::read(container) {
            Ok(data) => data,
            Err(e) => {
                diag.error(format!("{container}: failed to open file: {e}"));
                error = true;
                continue;
            }
        };
        if dump_container(container, &data, print_options, printer, diag).is_err() {
            error = true;
        }
    }
    Ok(if error { 1 } else { 0 })
}

fn dump_container(
    container: &str,
    data: &[u8],
    print_options: DebugPrintTableOptions,
    printer: &mut Printer,
    diag: &Diagnostics,
) -> Result<()> {
    let read_u32 = |offset: usize| -> Option<u32> {
        Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
    };
    let read_u64 = |offset: usize| -> Option<u64> {
        Some(u64::from_le_bytes(data.get(offset..offset + 8)?.try_into().ok()?))
    };

    let magic = read_u32(0).unwrap_or(0);
    if magic != crate::container::CONTAINER_MAGIC {
        diag.error(format!(
            "{container}: failed to read container: magic value is 0x{magic:08x} but AAPT expects 0x{:08x}",
            crate::container::CONTAINER_MAGIC
        ));
        bail!("bad magic");
    }
    let version = read_u32(4).unwrap_or(0);
    if version > crate::container::CONTAINER_VERSION {
        diag.error(format!(
            "{container}: failed to read container: container version is 0x{version:08x} but AAPT expects version 0x{:08x} or lower",
            crate::container::CONTAINER_VERSION
        ));
        bail!("bad version");
    }
    let entry_count = read_u32(8).unwrap_or(0);

    printer.println("AAPT2 Container (APC)");

    let mut error = false;
    let mut pos = 12usize;
    for _ in 0..entry_count {
        if pos % 4 != 0 {
            pos += 4 - pos % 4;
        }
        if pos + 12 > data.len() {
            break;
        }
        let entry_type = read_u32(pos).unwrap_or(u32::MAX);
        let entry_length = read_u64(pos + 4).unwrap_or(0) as usize;
        let content = pos + 12;
        match entry_type {
            crate::container::ENTRY_RES_TABLE => {
                printer.println("kResTable");
                let Some(table_pb) = data.get(content..content + entry_length) else {
                    break;
                };
                match crate::pb::decode_table(table_pb) {
                    Ok(table) => {
                        printer.indent();
                        table_printer::print_table(&table, print_options, printer);
                        printer.undent();
                    }
                    Err(e) => {
                        diag.error(format!("{container}: failed to parse table: {e}"));
                        error = true;
                    }
                }
            }
            crate::container::ENTRY_RES_FILE => {
                printer.println("kResFile");
                let header_size = read_u32(content).unwrap_or(0) as usize;
                let data_size = read_u64(content + 4).unwrap_or(0) as usize;
                let header_start = content + 12;
                let header_padding = (4 - header_size % 4) % 4;
                let data_offset = header_start + header_size + header_padding;
                let Some(header) = data.get(header_start..header_start + header_size) else {
                    break;
                };
                match crate::pb::decode_compiled_file(header) {
                    Ok(file) => {
                        printer.indent();
                        // `DumpCompiledFile`.
                        printer.print("Resource: ");
                        printer.println(file.name.to_string());
                        printer.print("Config:   ");
                        printer.println(file.config.to_string());
                        printer.print("Source:   ");
                        printer.println(file.source.to_string());
                        printer.print("Type:     ");
                        printer.println(resource_file_type_to_string(file.file_type));
                        printer.println(format!("Data:     offset={data_offset} length={data_size}"));
                        printer.undent();
                    }
                    Err(e) => {
                        diag.warn(format!("{container}: failed to parse compiled file: {e}"));
                        error = true;
                    }
                }
            }
            other => {
                diag.error(format!(
                    "{container}: failed to read container: entry type 0x{other:08x} is invalid"
                ));
                error = true;
                break;
            }
        }
        pos += 12 + entry_length;
    }
    if error {
        bail!("container had errors");
    }
    Ok(())
}

// ───────────────────────── diff ─────────────────────────

pub fn run_diff(args: &[String], diag: &Diagnostics) -> Result<i32> {
    let parsed = ParsedArgs::parse(args, &[], &["-v"])?;
    if parsed.positional.len() != 2 {
        eprintln!("must have two apks as arguments.\n");
        return Ok(1);
    }
    let apk_a = match LoadedApk::load(Path::new(&parsed.positional[0]), diag) {
        Ok(apk) => apk,
        Err(e) => {
            diag.error(format!("{e:#}"));
            return Ok(1);
        }
    };
    let apk_b = match LoadedApk::load(Path::new(&parsed.positional[1]), diag) {
        Ok(apk) => apk,
        Err(e) => {
            diag.error(format!("{e:#}"));
            return Ok(1);
        }
    };
    Ok(diff::diff_apks(apk_a, apk_b))
}

// ───────────────────────── golden tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::manifest::{dump_manifest, DumpManifestOptions};
    use super::printer::Printer;
    use crate::apk::LoadedApk;
    use crate::diag::Diagnostics;
    use std::path::{Path, PathBuf};

    const DUMP_TEST_DIR: &str =
        "/opt/src/github/skotlang/android/base/tools/aapt2/integration-tests/DumpTest";

    fn dump_test_path(name: &str) -> Option<PathBuf> {
        let path = Path::new(DUMP_TEST_DIR).join(name);
        path.exists().then_some(path)
    }

    fn badging_to_string(apk_path: &Path, options: DumpManifestOptions) -> String {
        let diag = Diagnostics::collecting();
        let apk = LoadedApk::load(apk_path, &diag).expect("failed to load APK");
        let mut out = String::new();
        let mut printer = Printer::new(&mut out);
        let code = dump_manifest(&apk, options, &mut printer, &diag);
        assert_eq!(code, 0, "dump_manifest failed: {:?}", diag.take());
        out
    }

    fn assert_badging_golden(apk_name: &str, expected_name: &str, options: DumpManifestOptions) {
        // Skip silently when the golden APKs are not available.
        let (Some(apk_path), Some(expected_path)) =
            (dump_test_path(apk_name), dump_test_path(expected_name))
        else {
            return;
        };
        let actual = badging_to_string(&apk_path, options);
        let expected = std::fs::read_to_string(&expected_path).expect("read golden");
        if actual != expected {
            let diff = similar::TextDiff::from_lines(&expected, &actual);
            panic!(
                "badging output for {apk_name} differs from {expected_name}:\n{}",
                diff.unified_diff()
                    .context_radius(3)
                    .header("expected", "actual")
            );
        }
    }

    #[test]
    fn badging_minimal() {
        assert_badging_golden(
            "minimal.apk",
            "minimal_expected.txt",
            DumpManifestOptions::default(),
        );
    }

    #[test]
    fn badging_components_with_meta_data() {
        assert_badging_golden(
            "components.apk",
            "components_expected.txt",
            DumpManifestOptions { include_meta_data: true, only_permissions: false },
        );
    }

    #[test]
    fn badging_components_only_permissions() {
        assert_badging_golden(
            "components.apk",
            "components_permissions_expected.txt",
            DumpManifestOptions { include_meta_data: false, only_permissions: true },
        );
    }

    #[test]
    fn badging_built_with_aapt() {
        assert_badging_golden(
            "built_with_aapt.apk",
            "built_with_aapt_expected.txt",
            DumpManifestOptions::default(),
        );
    }

    #[test]
    fn badging_multiple_uses_sdk() {
        assert_badging_golden(
            "multiple_uses_sdk.apk",
            "multiple_uses_sdk_expected.txt",
            DumpManifestOptions::default(),
        );
    }
}
