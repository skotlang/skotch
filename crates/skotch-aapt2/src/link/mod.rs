//! `aapt2 link` — merges compiled resources into a final APK.
//!
//! Port of `cmd/Link.cpp`. The pipeline (mirroring `Linker::Run`):
//!
//! 1. parse the manifest, extract the app package;
//! 2. load framework/library symbols from `-I` includes;
//! 3. fix the manifest (inject versions, sdk levels, …);
//! 4. merge every input container (tables + compiled files), with
//!    overlay semantics for `-R` inputs;
//! 5. assign resource IDs (honoring `--stable-ids`);
//! 6. remove resources with no default value, link all references,
//!    filter products, auto-version styles, collapse versions,
//!    exclude configs, dedupe;
//! 7. link + flatten each XML file, process the manifest, flatten the
//!    table (binary `resources.arsc` or proto `resources.pb`), and
//!    write the APK;
//! 8. emit R.java, proguard rules, and text symbols.

pub mod apk_writer;
pub mod id_assigner;
pub mod java_gen;
pub mod manifest_fixer;
pub mod reference_linker;
pub mod symbol_table;
pub mod table_merger;
pub mod transforms;

use crate::apk::LoadedApk;
use crate::compile::FeatureFlagValues;
use crate::container::{read_container, ContainerEntry};
use crate::diag::Diagnostics;
use crate::res::config::ConfigDescription;
use crate::res::table::ResourceTable;
use crate::res::value::FileType;
use crate::res::{ResourceId, ResourceName, Source, APP_PACKAGE_ID};
use anyhow::{anyhow, bail, Context, Result};
use manifest_fixer::{AppInfo, ManifestFixerOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use symbol_table::SymbolTable;

/// What kind of package is being linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PackageType {
    #[default]
    App,
    SharedLib,
    StaticLib,
}

/// Output serialization format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Binary `resources.arsc` + binary XML.
    #[default]
    Binary,
    /// `resources.pb` + proto XML (`--proto-format`).
    Proto,
}

/// Options for [`link`]. Mirrors `LinkOptions`.
#[derive(Debug, Default)]
pub struct LinkOptions {
    pub output_path: PathBuf,
    pub manifest_path: PathBuf,
    /// `-I` includes (framework APKs / android.jar).
    pub include_paths: Vec<PathBuf>,
    /// `-R` overlay inputs.
    pub overlay_files: Vec<PathBuf>,
    /// `-A` asset directories.
    pub assets_dirs: Vec<PathBuf>,
    pub package_type: PackageType,
    pub output_format: OutputFormat,
    /// `--output-to-dir`.
    pub output_to_directory: bool,
    /// `--package-id`.
    pub package_id: Option<u8>,
    pub allow_reserved_package_id: bool,
    /// `--java`.
    pub generate_java_class_path: Option<PathBuf>,
    /// `--custom-package`.
    pub custom_java_package: Option<String>,
    /// `--extra-packages`.
    pub extra_java_packages: Vec<String>,
    /// `--add-javadoc-annotation`.
    pub javadoc_annotations: Vec<String>,
    /// `--private-symbols`.
    pub private_symbols: Option<String>,
    /// `--proguard`.
    pub generate_proguard_rules_path: Option<PathBuf>,
    /// `--proguard-main-dex`.
    pub generate_main_dex_proguard_rules_path: Option<PathBuf>,
    pub generate_conditional_proguard_rules: bool,
    pub generate_minimal_proguard_rules: bool,
    pub no_proguard_location_reference: bool,
    /// `--output-text-symbols`.
    pub generate_text_symbols_path: Option<PathBuf>,
    /// `--auto-add-overlay`.
    pub auto_add_overlay: bool,
    pub override_styles_instead_of_overlaying: bool,
    pub strict_visibility: bool,
    /// `--rename-resources-package`.
    pub rename_resources_package: Option<String>,
    /// `--no-static-lib-packages`.
    pub no_static_lib_packages: bool,
    pub no_auto_version: bool,
    pub no_version_vectors: bool,
    pub no_version_transitions: bool,
    pub no_resource_deduping: bool,
    pub no_resource_removal: bool,
    pub no_xml_namespaces: bool,
    /// `-c` configuration filter list.
    pub configs: Vec<String>,
    /// `--preferred-density`.
    pub preferred_density: Option<String>,
    /// `--product`.
    pub products: Vec<String>,
    /// `--exclude-configs`.
    pub exclude_configs: Vec<String>,
    /// `-0` extensions not to compress (empty extension = "" matches all).
    pub extensions_to_not_compress: Vec<String>,
    /// `--no-compress`.
    pub do_not_compress_anything: bool,
    /// `--no-compress-regex` (ECMAScript regex source).
    pub regex_to_not_compress: Option<String>,
    /// `--keep-raw-values`.
    pub keep_raw_values: bool,
    pub enable_sparse_encoding: bool,
    pub force_sparse_encoding: bool,
    pub enable_compact_entries: bool,
    /// `-z`: require localization of 'suggested' strings.
    pub require_localization: bool,
    /// `--merge-only`.
    pub merge_only: bool,
    /// `--exclude-sources`.
    pub exclude_sources: bool,
    /// `--stable-ids` content.
    pub stable_id_map: HashMap<ResourceName, ResourceId>,
    /// `--emit-ids` output path.
    pub resource_id_map_path: Option<PathBuf>,
    /// `--feature-flags`.
    pub feature_flag_values: FeatureFlagValues,
    pub manifest_fixer_options: ManifestFixerOptions,
    pub verbose: bool,
}

/// Everything `link` produced beyond the APK itself — exposed for the
/// skotch build pipeline to consume in-process.
#[derive(Debug, Default)]
pub struct LinkOutputs {
    /// (java package, R.java source) pairs.
    pub r_java: Vec<(String, String)>,
    pub proguard_rules: Option<String>,
    pub text_symbols: Option<String>,
    /// name → assigned ID for every resource in the final table.
    pub resource_ids: HashMap<ResourceName, ResourceId>,
}

/// A merged compiled file pending packaging.
pub struct PendingFile {
    pub name: ResourceName,
    pub config: ConfigDescription,
    /// Destination path inside the APK (`res/…`).
    pub dest_path: String,
    pub file_type: FileType,
    pub contents: std::sync::Arc<Vec<u8>>,
    pub source: Source,
}

/// Link state threaded through the pipeline stages.
pub struct LinkContext {
    pub compilation_package: String,
    pub package_id: u8,
    pub package_type: PackageType,
    pub min_sdk_version: i32,
    pub symbols: SymbolTable,
    pub app_info: AppInfo,
}

/// The `aapt2 link` entry point.
pub fn link(
    input_files: &[PathBuf],
    options: &LinkOptions,
    diag: &Diagnostics,
) -> Result<LinkOutputs> {
    // ── 1. Manifest ────────────────────────────────────────────────
    let manifest_text = std::fs::read_to_string(&options.manifest_path)
        .with_context(|| format!("failed to read {}", options.manifest_path.display()))?;
    let manifest_doc = crate::xml::parse_source_xml(
        &options.manifest_path.to_string_lossy(),
        &manifest_text,
    )?;
    let mut manifest = manifest_doc
        .root
        .ok_or_else(|| anyhow!("AndroidManifest.xml has no root element"))?;
    if manifest.name != "manifest" || !manifest.namespace_uri.is_empty() {
        bail!(
            "{}: root tag must be <manifest>",
            options.manifest_path.display()
        );
    }

    let app_info = manifest_fixer::extract_app_info(&manifest)?;
    let mut compilation_package = app_info.package.clone();
    let mut custom_java_package = options.custom_java_package.clone();
    if let Some(renamed) = &options.rename_resources_package {
        if custom_java_package.is_none() {
            custom_java_package = Some(compilation_package.clone());
        }
        compilation_package = renamed.clone();
    }

    let package_id = match (compilation_package.as_str(), options.package_id) {
        ("android", _) => 0x01,
        (_, Some(id)) => id,
        _ => APP_PACKAGE_ID,
    };
    if let Some(id) = options.package_id {
        if options.package_type != PackageType::App {
            bail!("--package-id cannot be used with --static-lib or --shared-lib");
        }
        if id < APP_PACKAGE_ID && !options.allow_reserved_package_id {
            bail!(
                "invalid package ID 0x{id:02x}. Must be in the range 0x7f-0xff. Use \
                 --allow-reserved-package-id to allow IDs in the range 0x02-0x7e"
            );
        }
    }

    // ── 2. Includes / symbols ──────────────────────────────────────
    let mut symbols = SymbolTable::new();
    for include in &options.include_paths {
        let apk = LoadedApk::load(include, diag)
            .with_context(|| format!("failed to load include path {}", include.display()))?;
        symbols.add_include(apk);
    }

    // ── 3. Manifest fixing ─────────────────────────────────────────
    manifest_fixer::fix_manifest(&mut manifest, &options.manifest_fixer_options, diag)?;
    let app_info = manifest_fixer::extract_app_info(&manifest)?;
    let min_sdk_version = app_info.min_sdk_version.unwrap_or(0);

    let mut context = LinkContext {
        compilation_package: compilation_package.clone(),
        package_id,
        package_type: options.package_type,
        min_sdk_version,
        symbols,
        app_info,
    };

    if options.verbose {
        diag.note(format!(
            "linking package '{compilation_package}' using package ID {package_id:02x}"
        ));
    }

    // ── 4. Merge inputs ────────────────────────────────────────────
    let mut table = ResourceTable::new_unvalidated();
    let merger_options = table_merger::TableMergerOptions {
        auto_add_overlay: options.auto_add_overlay,
        override_styles_instead_of_overlaying: options.override_styles_instead_of_overlaying,
        strict_visibility: options.strict_visibility,
    };

    // Symbols exported from the manifest itself (@+id refs).
    {
        let mut exported = Vec::new();
        let manifest_diag = Diagnostics::collecting();
        crate::compile::collect_exported_ids(&manifest, &mut exported, &manifest_diag);
        table_merger::merge_exported_symbols(
            &mut table,
            &compilation_package,
            &Source::new(options.manifest_path.to_string_lossy()),
            exported,
        )?;
    }

    for (input, overlay) in input_files
        .iter()
        .map(|p| (p, false))
        .chain(options.overlay_files.iter().map(|p| (p, true)))
    {
        merge_path(input, overlay, &mut table, &context, &merger_options, diag)
            .with_context(|| format!("failed parsing {}", input.display()))?;
    }

    table_merger::verify_no_external_packages(&table, &compilation_package)?;

    // ── 5/6. Transform pipeline ────────────────────────────────────
    let mut outputs = LinkOutputs::default();
    if context.package_type != PackageType::StaticLib {
        if package_id == 0x01 {
            transforms::move_private_attrs(&mut table)?;
        }
        id_assigner::assign_ids(&mut table, package_id, &options.stable_id_map)?;

        for package in &table.packages {
            for ty in &package.types {
                for entry in &ty.entries {
                    if let Some(id) = entry.id {
                        outputs.resource_ids.insert(
                            ResourceName::with_named_type(
                                &package.name,
                                ty.named_type.clone(),
                                &entry.name,
                            ),
                            id,
                        );
                    }
                }
            }
        }
        if let Some(path) = &options.resource_id_map_path {
            write_stable_id_map(path, &outputs.resource_ids)?;
        }
    }

    context
        .symbols
        .rebuild_self_index(&table, package_id, &compilation_package);

    if !options.no_resource_removal {
        transforms::remove_no_default_resources(&mut table, diag)?;
    }

    if !options.merge_only {
        reference_linker::link_table_references(
            &mut table,
            &context.symbols,
            &compilation_package,
            diag,
        )?;
    }

    if context.package_type != PackageType::StaticLib {
        transforms::filter_products(&mut table, &options.products, diag)?;
    }

    if !options.no_auto_version {
        transforms::auto_version(&mut table)?;
    }

    if context.package_type != PackageType::StaticLib && min_sdk_version > 0 {
        transforms::collapse_versions(&mut table, min_sdk_version)?;
    }

    if !options.exclude_configs.is_empty() {
        let mut configs = Vec::new();
        for config_str in &options.exclude_configs {
            configs.push(
                ConfigDescription::parse(config_str)
                    .ok_or_else(|| anyhow!("failed to parse --excluded-configs {config_str}"))?,
            );
        }
        transforms::exclude_configs(&mut table, &configs)?;
    }

    if !options.no_resource_deduping {
        transforms::dedupe_resources(&mut table)?;
    }

    // ── 7. Write the APK ───────────────────────────────────────────
    let writer_options = apk_writer::ApkWriterOptions {
        output_format: options.output_format,
        output_to_directory: options.output_to_directory,
        keep_raw_values: options.keep_raw_values,
        no_xml_namespaces: options.no_xml_namespaces,
        extensions_to_not_compress: options.extensions_to_not_compress.clone(),
        do_not_compress_anything: options.do_not_compress_anything,
        regex_to_not_compress: options.regex_to_not_compress.clone(),
        enable_sparse_encoding: options.enable_sparse_encoding
            && (min_sdk_version >= crate::res::config::SDK_O as i32),
        force_sparse_encoding: options.force_sparse_encoding,
        enable_compact_entries: options.enable_compact_entries,
        merge_only: options.merge_only,
    };
    let mut keep_set = java_gen::KeepSet::new(options.generate_conditional_proguard_rules);
    apk_writer::write_apk(
        &options.output_path,
        &mut table,
        &mut manifest,
        &context,
        &writer_options,
        &options.assets_dirs,
        &mut keep_set,
        diag,
    )?;

    // ── 8. Java + proguard + text symbols ──────────────────────────
    if let Some(java_dir) = &options.generate_java_class_path {
        let java_options = java_gen::JavaGenOptions {
            custom_package: custom_java_package.clone(),
            extra_packages: options.extra_java_packages.clone(),
            javadoc_annotations: options.javadoc_annotations.clone(),
            private_symbols: options.private_symbols.clone(),
            non_final_ids: options.package_type == PackageType::StaticLib,
            rename_manifest_package: options
                .manifest_fixer_options
                .rename_manifest_package
                .clone(),
        };
        let classes = java_gen::generate_java_classes(
            &table,
            &manifest,
            &context,
            &java_options,
            diag,
        )?;
        for (package, source) in &classes {
            let mut out_path = java_dir.clone();
            for part in package.split('.') {
                out_path.push(part);
            }
            std::fs::create_dir_all(&out_path)?;
            out_path.push("R.java");
            std::fs::write(&out_path, source)?;
        }
        outputs.r_java = classes;
    }

    if let Some(proguard_path) = &options.generate_proguard_rules_path {
        let rules = java_gen::generate_proguard_rules(
            &keep_set,
            options.no_proguard_location_reference,
        );
        std::fs::write(proguard_path, &rules)?;
        outputs.proguard_rules = Some(rules);
    }

    if let Some(symbols_path) = &options.generate_text_symbols_path {
        let text = java_gen::generate_text_symbols(&table);
        std::fs::write(symbols_path, &text)?;
        outputs.text_symbols = Some(text);
    }

    if diag.has_errors() {
        bail!("link failed with {} error(s)", diag.error_count());
    }
    Ok(outputs)
}

/// Merges one input path: a `.flat`/`.apc` container, a zip of
/// containers (`.zip`/`.flata`/`.jar`/`.jack`), or an `.apk` (static
/// library). Mirrors `Linker::MergePath`.
fn merge_path(
    path: &Path,
    overlay: bool,
    table: &mut ResourceTable,
    context: &LinkContext,
    options: &table_merger::TableMergerOptions,
    diag: &Diagnostics,
) -> Result<()> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "flata" | "zip" | "jar" | "jack" => {
            for (name, data) in crate::compile::read_artifacts(path)? {
                if name.ends_with(".flat") || name.ends_with(".apc") {
                    merge_container(&name, &data, overlay, table, context, options, diag)?;
                }
            }
            Ok(())
        }
        "apk" => bail!(
            "{}: merging static library APKs is not yet supported by skotch aapt2",
            path.display()
        ),
        _ => {
            let data = std::fs::read(path)
                .with_context(|| format!("failed to open {}", path.display()))?;
            merge_container(&path.to_string_lossy(), &data, overlay, table, context, options, diag)
        }
    }
}

fn merge_container(
    source: &str,
    data: &[u8],
    overlay: bool,
    table: &mut ResourceTable,
    context: &LinkContext,
    options: &table_merger::TableMergerOptions,
    diag: &Diagnostics,
) -> Result<()> {
    for entry in read_container(data).with_context(|| format!("invalid container {source}"))? {
        match entry {
            ContainerEntry::ResTable { table_pb } => {
                let incoming = crate::pb::decode_table(&table_pb)
                    .with_context(|| format!("invalid resource table in {source}"))?;
                table_merger::merge_table(
                    table,
                    &context.compilation_package,
                    incoming,
                    overlay,
                    options,
                    diag,
                )?;
            }
            ContainerEntry::ResFile { compiled_file_pb, data } => {
                let file = crate::pb::decode_compiled_file(&compiled_file_pb)
                    .with_context(|| format!("invalid compiled file in {source}"))?;
                table_merger::merge_compiled_file(
                    table,
                    &context.compilation_package,
                    file,
                    data,
                    overlay,
                    options,
                    diag,
                )?;
            }
        }
    }
    Ok(())
}

fn write_stable_id_map(
    path: &Path,
    ids: &HashMap<ResourceName, ResourceId>,
) -> Result<()> {
    let mut entries: Vec<(&ResourceName, &ResourceId)> = ids.iter().collect();
    entries.sort();
    let mut out = String::new();
    for (name, id) in entries {
        out.push_str(&format!("{name} = {id}\n"));
    }
    std::fs::write(path, out).with_context(|| format!("failed to write {}", path.display()))
}

/// Parses a `--stable-ids` file: lines of `package:type/name = 0xPPTTEEEE`.
pub fn load_stable_id_map(content: &str) -> Result<HashMap<ResourceName, ResourceId>> {
    let mut map = HashMap::new();
    for (line_number, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name_str, id_str) = line
            .split_once('=')
            .ok_or_else(|| anyhow!("line {}: missing '='", line_number + 1))?;
        let (name, _) = crate::res::parse_resource_name(name_str.trim())
            .ok_or_else(|| anyhow!("line {}: invalid resource name", line_number + 1))?;
        let id = crate::res::utils::parse_resource_id(id_str.trim())
            .ok_or_else(|| anyhow!("line {}: invalid resource ID", line_number + 1))?;
        map.insert(name, id);
    }
    Ok(map)
}
