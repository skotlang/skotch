//! `aapt2 compile` — turns source resources into compiled containers.
//!
//! Port of `cmd/Compile.cpp`. Each input resource file becomes one
//! `.flat` container: values XML compiles to a proto resource table,
//! other XML compiles to proto XML (plus any `<aapt:attr>` inline
//! documents), PNGs are processed, and everything else is copied.

pub mod inline_xml;
pub mod ninepatch;
pub mod png;
pub mod pseudolocale;
pub mod values_parser;

use crate::container::ContainerWriter;
use crate::diag::Diagnostics;
use crate::pb;
use crate::res::config::ConfigDescription;
use crate::res::table::{ResourceTable, VisibilityLevel};
use crate::res::utils::parse_reference;
use crate::res::value::{FileType, Styleable, ValueKind};
use crate::res::{
    FeatureFlagAttribute, FlagStatus, ResourceFile, ResourceName, ResourceType, Source,
    SourcedResourceName,
};
use crate::xml::Element;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Feature flag properties from `--feature-flags` (defined alongside
/// the values parser, re-exported as the canonical type).
pub use values_parser::FeatureFlagProperties;

pub type FeatureFlagValues = HashMap<String, FeatureFlagProperties>;

/// Options for [`compile`]. Mirrors `aapt::CompileOptions`.
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// `--dir`: treat as a res directory and compile every file.
    pub res_dir: Option<PathBuf>,
    /// `--zip`: a zip containing a res directory.
    pub res_zip: Option<PathBuf>,
    /// `--output-text-symbols`: write an R.txt next to the output.
    pub generate_text_symbols_path: Option<PathBuf>,
    /// `--pseudo-localize`.
    pub pseudolocalize: bool,
    /// `--pseudo-localize-gender-values` (default "f,m,n").
    pub pseudo_localize_gender_values: Option<String>,
    /// `--pseudo-localize-gender-ratio` (default 1.0).
    pub pseudo_localize_gender_ratio: Option<String>,
    /// `--no-crunch`.
    pub no_png_crunch: bool,
    /// `--png-compression-level` (0-9, default 9).
    pub png_compression_level: u8,
    /// `--legacy`: treat some errors as warnings.
    pub legacy_mode: bool,
    /// `--preserve-visibility-of-styleables`.
    pub preserve_visibility_of_styleables: bool,
    /// `--visibility`: force visibility of all compiled resources.
    pub visibility: Option<VisibilityLevel>,
    /// `--source-path`: override the recorded source path.
    pub source_path: Option<String>,
    /// `--filter-product`.
    pub product: Option<String>,
    /// `--feature-flags`.
    pub feature_flag_values: FeatureFlagValues,
    pub verbose: bool,
}

impl CompileOptions {
    pub fn new() -> Self {
        CompileOptions {
            png_compression_level: 9,
            ..Default::default()
        }
    }
}

/// Path information extracted from `res/<type>[-config]/<name>.<ext>`.
/// Mirrors `aapt::ResourcePathData`.
#[derive(Debug, Clone, Default)]
pub struct ResourcePathData {
    pub source: Source,
    /// e.g. "values", "layout", "drawable".
    pub resource_dir: String,
    pub name: String,
    /// "xml", "png", "9.png", "" …
    pub extension: String,
    /// From a `flag(name)` path segment.
    pub flag_name: String,
    pub config_str: String,
    pub config: ConfigDescription,
}

/// Parses `[!]name` flag text. Mirrors `aapt::ParseFlag`.
pub fn parse_flag(flag_text: &str) -> Option<FeatureFlagAttribute> {
    if flag_text.is_empty() {
        return None;
    }
    Some(match flag_text.strip_prefix('!') {
        Some(name) => FeatureFlagAttribute {
            name: name.to_string(),
            negated: true,
        },
        None => FeatureFlagAttribute {
            name: flag_text.to_string(),
            negated: false,
        },
    })
}

/// Computes a flag's status against the command-line flag values.
/// Mirrors `aapt::GetFlagStatus`.
pub fn get_flag_status(
    flag: &Option<FeatureFlagAttribute>,
    feature_flag_values: &FeatureFlagValues,
) -> Result<FlagStatus, String> {
    let Some(flag) = flag else {
        return Ok(FlagStatus::NoFlag);
    };
    let Some(properties) = feature_flag_values.get(&flag.name) else {
        return Err(format!("Resource flag value undefined: {}", flag.name));
    };
    if !properties.read_only {
        return Err(format!(
            "Only read only flags may be used with resources: {}",
            flag.name
        ));
    }
    let Some(enabled) = properties.enabled else {
        return Err(format!(
            "Only flags with a value may be used with resources: {}",
            flag.name
        ));
    };
    Ok(if enabled != flag.negated {
        FlagStatus::Enabled
    } else {
        FlagStatus::Disabled
    })
}

/// Parses one `--feature-flags` argument:
/// `flag1=true,flag2:ro=false,flag3=`. Mirrors
/// `aapt::ParseFeatureFlagsParameter`.
pub fn parse_feature_flags_parameter(arg: &str, out: &mut FeatureFlagValues) -> Result<(), String> {
    if arg.is_empty() {
        return Ok(());
    }
    for flag_and_value in arg.split(',') {
        if flag_and_value.is_empty() {
            continue;
        }
        let parts: Vec<&str> = flag_and_value.split('=').collect();
        if parts.len() > 2 {
            return Err(format!(
                "Invalid feature flag and optional value '{flag_and_value}'. Must be in the \
                 format 'flag_name[:ro][=true|false]"
            ));
        }
        let flag_name = crate::util::trim_whitespace(parts[0]);
        if flag_name.is_empty() {
            return Err(format!("No name given for one or more flags in: {arg}"));
        }
        let name_parts: Vec<&str> = flag_name.split(':').collect();
        if name_parts.len() > 2 {
            return Err(format!(
                "Invalid feature flag and optional value '{flag_and_value}'. Must be in the \
                 format 'flag_name[:READ_ONLY|READ_WRITE][=true|false]"
            ));
        }
        let name = name_parts[0].to_string();
        let read_only = match name_parts.get(1) {
            Some(&"ro") | Some(&"READ_ONLY") => true,
            Some(&"READ_WRITE") | None => false,
            Some(other) => {
                return Err(format!(
                    "Invalid feature flag permission '{other}' in: {arg}"
                ))
            }
        };
        let enabled = match parts.get(1).map(|v| crate::util::trim_whitespace(v)) {
            None | Some("") => None,
            Some("true") => Some(true),
            Some("false") => Some(false),
            Some(other) => {
                return Err(format!(
                    "Invalid value '{other}' for feature flag '{flag_name}'"
                ))
            }
        };
        // Verify it doesn't conflict with any existing value.
        if let Some(existing) = out.get(&name) {
            if existing.enabled.is_some() && enabled.is_some() && existing.enabled != enabled {
                return Err(format!("Conflicting values for feature flag '{name}'"));
            }
        }
        let entry = out.entry(name).or_default();
        entry.read_only |= read_only;
        if enabled.is_some() {
            entry.enabled = enabled;
        }
    }
    Ok(())
}

/// Splits a path into [`ResourcePathData`]. Mirrors
/// `ExtractResourcePathData`.
pub fn extract_resource_path_data(
    path: &str,
    source_path_override: Option<&str>,
) -> Result<ResourcePathData, String> {
    let mut parts: Vec<&str> = path.split(['/', '\\']).collect();

    let mut flag_name = String::new();
    parts.retain(|part| {
        if part.starts_with("flag(") && part.ends_with(')')
            && flag_name.is_empty() {
                flag_name = part[5..part.len() - 1].to_string();
                return false;
            }
            // A second flag directory is an error, detected below by
            // leaving the marker in place.
        true
    });
    if parts
        .iter()
        .any(|p| p.starts_with("flag(") && p.ends_with(')'))
    {
        return Err("resource path cannot contain more than one flag directory".to_string());
    }

    if parts.len() < 2 {
        return Err("bad resource path".to_string());
    }
    let dir = parts[parts.len() - 2];
    let filename = parts[parts.len() - 1];

    let (dir_str, config_str, config) = match dir.find('-') {
        Some(dash) => {
            let config_str = &dir[dash + 1..];
            let config = ConfigDescription::parse(config_str)
                .ok_or_else(|| format!("invalid configuration '{config_str}'"))?;
            (&dir[..dash], config_str.to_string(), config)
        }
        None => (dir, String::new(), ConfigDescription::default()),
    };

    const NINE_PNG: &str = ".9.png";
    let (name, extension) = if filename.len() > NINE_PNG.len() && filename.ends_with(NINE_PNG) {
        (
            &filename[..filename.len() - NINE_PNG.len()],
            "9.png".to_string(),
        )
    } else {
        match filename.rfind('.') {
            Some(dot) => (&filename[..dot], filename[dot + 1..].to_string()),
            None => (filename, String::new()),
        }
    };

    let source = Source::new(source_path_override.unwrap_or(path));
    Ok(ResourcePathData {
        source,
        resource_dir: dir_str.to_string(),
        name: name.to_string(),
        extension,
        flag_name,
        config_str,
        config,
    })
}

/// The output container name for a compiled resource, e.g.
/// `values-de_strings.arsc.flat`. Mirrors
/// `BuildIntermediateContainerFilename`.
pub fn build_output_filename(data: &ResourcePathData) -> String {
    let mut name = data.resource_dir.clone();
    if !data.config_str.is_empty() {
        name.push('-');
        name.push_str(&data.config_str);
    }
    name.push('_');
    name.push_str(&data.name);
    if !data.flag_name.is_empty() {
        name.push_str(&format!(".({})", data.flag_name));
    }
    if !data.extension.is_empty() {
        name.push('.');
        name.push_str(&data.extension);
    }
    name.push_str(".flat");
    name
}

/// One compiled output artifact: the container name and its bytes.
#[derive(Debug, Clone)]
pub struct CompiledArtifact {
    pub name: String,
    pub data: Vec<u8>,
    /// R.txt lines contributed by this artifact (`--output-text-symbols`).
    pub text_symbols: Vec<String>,
}

/// Compiles a single resource file already loaded into memory. This is
/// the core library entry point — the CLI and the directory walker both
/// funnel through here.
pub fn compile_data(
    path: &str,
    data: &[u8],
    options: &CompileOptions,
    diag: &Diagnostics,
) -> Result<CompiledArtifact> {
    let mut path_data = extract_resource_path_data(path, options.source_path.as_deref())
        .map_err(|e| anyhow!("{path}: {e}"))?;

    enum Kind {
        Table,
        Xml,
        Png,
        File,
    }

    let mut kind = Kind::File;
    if path_data.resource_dir == "values" && path_data.extension == "xml" {
        kind = Kind::Table;
        path_data.extension = "arsc".to_string();
    } else if let Some(ty) = ResourceType::parse(&path_data.resource_dir) {
        if ty != ResourceType::Raw {
            if ty == ResourceType::Xml || path_data.extension == "xml" {
                kind = Kind::Xml;
            } else if (!options.no_png_crunch && path_data.extension == "png")
                || path_data.extension == "9.png"
            {
                kind = Kind::Png;
            }
        }
    } else {
        bail!("invalid file path '{}'", path_data.source);
    }

    // Periods are reserved in file names (legacy AAPT allowed them).
    if !matches!(kind, Kind::File) && !options.legacy_mode && path_data.name.contains('.') {
        bail!(
            "{}: file name cannot contain '.' other than for specifying the extension",
            path_data.source
        );
    }

    let output_name = build_output_filename(&path_data);
    let mut artifact = match kind {
        Kind::Table => compile_table(&path_data, data, options, diag)?,
        Kind::Xml => compile_xml(&path_data, data, options, diag)?,
        Kind::Png => compile_png(&path_data, data, options, diag)?,
        Kind::File => compile_file(&path_data, data, options)?,
    };
    artifact.name = output_name;
    Ok(artifact)
}

fn compile_table(
    path_data: &ResourcePathData,
    data: &[u8],
    options: &CompileOptions,
    diag: &Diagnostics,
) -> Result<CompiledArtifact> {
    // Filenames starting with "donottranslate" are not localizable.
    let translatable_file = !path_data.name.starts_with("donottranslate");

    let text = std::str::from_utf8(data)
        .map_err(|_| anyhow!("{}: file is not valid UTF-8", path_data.source))?;
    let doc = crate::xml::parse_source_xml(&path_data.source.path, text)?;

    let mut table = ResourceTable::new();

    let flag = parse_flag(&path_data.flag_name);
    let flag_status = get_flag_status(&flag, &options.feature_flag_values)
        .map_err(|e| anyhow!("{}: {e}", path_data.source))?;

    let parser_options = values_parser::ResourceParserOptions {
        translatable: translatable_file,
        error_on_positional_arguments: !options.legacy_mode,
        preserve_visibility_of_styleables: options.preserve_visibility_of_styleables,
        visibility: options.visibility,
        feature_flags: options.feature_flag_values.clone(),
        flag,
        flag_status,
    };

    let mut parser = values_parser::ResourceParser::new(
        &mut table,
        path_data.source.clone(),
        path_data.config,
        parser_options,
    );
    if let Err(errors) = parser.parse(&doc) {
        for error in &errors {
            diag.error(error.clone());
        }
        bail!("failed to compile values file {}", path_data.source);
    }
    drop(parser);

    if let Some(product) = &options.product {
        product_filter(&mut table, product);
    }

    if options.pseudolocalize && translatable_file {
        pseudolocale::generate_pseudolocales(
            &mut table,
            options
                .pseudo_localize_gender_values
                .as_deref()
                .unwrap_or("f,m,n"),
            options
                .pseudo_localize_gender_ratio
                .as_deref()
                .unwrap_or("1.0"),
        )?;
    }

    let table_pb = pb::encode_table(&table, &pb::SerializeTableOptions::default());
    let mut writer = ContainerWriter::new(1);
    writer.add_res_table(&table_pb);

    let text_symbols = if options.generate_text_symbols_path.is_some() {
        table_text_symbols(&table)
    } else {
        Vec::new()
    };

    Ok(CompiledArtifact {
        name: String::new(),
        data: writer.finish()?,
        text_symbols,
    })
}

/// R.txt lines for a compiled values table. Mirrors the
/// `--output-text-symbols` block in `CompileTable`.
fn table_text_symbols(table: &ResourceTable) -> Vec<String> {
    let mut lines = Vec::new();
    for package in &table.packages {
        // Only resources defined locally (empty package name).
        if !package.name.is_empty() {
            continue;
        }
        for ty in &package.types {
            for entry in &ty.entries {
                let visibility = match entry.visibility.level {
                    VisibilityLevel::Undefined => "default",
                    VisibilityLevel::Public => "public",
                    VisibilityLevel::Private => "private",
                };
                if ty.named_type.ty != ResourceType::Styleable {
                    lines.push(format!("{visibility} int {} {}", ty.named_type, entry.name));
                } else {
                    lines.push(format!("{visibility} int[] styleable {}", entry.name));
                    if let Some(value) = entry.values.first().and_then(|v| v.value.as_ref()) {
                        if let ValueKind::Styleable(Styleable { entries }) = &value.kind {
                            for attr in entries {
                                if let Some(name) = &attr.name {
                                    let mut line = format!("default int styleable {}", entry.name);
                                    if !name.package.is_empty() {
                                        line.push('_');
                                        line.push_str(&name.package.replace('.', "_"));
                                    }
                                    line.push('_');
                                    line.push_str(&name.entry);
                                    lines.push(line);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    lines
}

/// Port of `ProductFilter` with `remove_default_config_values = true`
/// (the only mode `compile --filter-product` uses).
fn product_filter(table: &mut ResourceTable, product: &str) {
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                // Group values by config; keep only the product match.
                let mut kept = Vec::new();
                let values = std::mem::take(&mut entry.values);
                let mut by_config: BTreeMap<Vec<u8>, Vec<crate::res::table::ResourceConfigValue>> =
                    BTreeMap::new();
                for value in values {
                    by_config
                        .entry(value.config.to_bytes())
                        .or_default()
                        .push(value);
                }
                for (_, group) in by_config {
                    if let Some(mut selected) = group.into_iter().find(|v| v.product == product) {
                        selected.product = String::new();
                        kept.push(selected);
                    }
                }
                entry.values = kept;
            }
            ty.entries.retain(|e| !e.values.is_empty());
        }
        package.types.retain(|t| !t.entries.is_empty());
    }
    table.packages.retain(|p| !p.types.is_empty());
}

fn compile_xml(
    path_data: &ResourcePathData,
    data: &[u8],
    options: &CompileOptions,
    diag: &Diagnostics,
) -> Result<CompiledArtifact> {
    let text = std::str::from_utf8(data)
        .map_err(|_| anyhow!("{}: file is not valid UTF-8", path_data.source))?;
    let parsed = crate::xml::parse_source_xml(&path_data.source.path, text)?;

    let ty = ResourceType::parse(&path_data.resource_dir)
        .ok_or_else(|| anyhow!("invalid resource type '{}'", path_data.resource_dir))?;

    let flag = parse_flag(&path_data.flag_name);
    let flag_status = get_flag_status(&flag, &options.feature_flag_values)
        .map_err(|e| anyhow!("{}: {e}", path_data.source))?;

    let mut xmlres = CompiledXml {
        file: ResourceFile {
            name: ResourceName::new("", ty, &path_data.name),
            config: path_data.config,
            file_type: FileType::ProtoXml,
            source: path_data.source.clone(),
            exported_symbols: Vec::new(),
            flag_status,
            flag,
        },
        root: parsed
            .root
            .ok_or_else(|| anyhow!("{}: no root element", path_data.source))?,
    };

    // Collect @+id exported symbols.
    collect_exported_ids(&xmlres.root, &mut xmlres.file.exported_symbols, diag);

    // Extract <aapt:attr> inline documents.
    let inline_docs = inline_xml::extract_inline_xml(&mut xmlres.root, &xmlres.file)?;

    let mut writer = ContainerWriter::new(1 + inline_docs.len());
    add_xml_entry(&mut writer, &xmlres.file, &xmlres.root);
    for doc in &inline_docs {
        add_xml_entry(&mut writer, &doc.file, &doc.root);
    }

    let mut text_symbols = Vec::new();
    if options.generate_text_symbols_path.is_some() {
        for symbol in &xmlres.file.exported_symbols {
            text_symbols.push(format!("default int id {}", symbol.name.entry));
        }
        text_symbols.push(format!(
            "default int {} {}",
            path_data.resource_dir, path_data.name
        ));
    }

    Ok(CompiledArtifact {
        name: String::new(),
        data: writer.finish()?,
        text_symbols,
    })
}

/// A compiled XML document: metadata plus DOM.
#[derive(Debug)]
pub struct CompiledXml {
    pub file: ResourceFile,
    pub root: Element,
}

fn add_xml_entry(writer: &mut ContainerWriter, file: &ResourceFile, root: &Element) {
    let compiled_file_pb = pb::encode_compiled_file(file);
    let xml_pb = crate::xml::encode_pb_xml(root);
    writer.add_res_file(&compiled_file_pb, &xml_pb);
}

/// Walks the element tree collecting `@+id/name` references defined in
/// attribute values. Port of `XmlIdCollector`. Also used by `link` for
/// the manifest's exported symbols.
pub fn collect_exported_ids(
    element: &Element,
    out: &mut Vec<SourcedResourceName>,
    diag: &Diagnostics,
) {
    for attr in &element.attributes {
        if let Some(parsed) = parse_reference(&attr.value) {
            if parsed.create && parsed.name.ty.ty == ResourceType::Id {
                if crate::res::table::first_invalid_entry_name_char(&parsed.name.entry).is_some() {
                    diag.error(format!("id '{}' has an invalid entry name", parsed.name));
                } else {
                    let position = out.binary_search_by(|s| s.name.cmp(&parsed.name));
                    if let Err(index) = position {
                        out.insert(
                            index,
                            SourcedResourceName {
                                name: parsed.name.clone(),
                                line: element.line_number,
                            },
                        );
                    }
                }
            }
        }
    }
    for child in element.child_elements() {
        collect_exported_ids(child, out, diag);
    }
}

fn compile_png(
    path_data: &ResourcePathData,
    data: &[u8],
    options: &CompileOptions,
    diag: &Diagnostics,
) -> Result<CompiledArtifact> {
    let flag = parse_flag(&path_data.flag_name);
    let flag_status = get_flag_status(&flag, &options.feature_flag_values)
        .map_err(|e| anyhow!("{}: {e}", path_data.source))?;

    let file = ResourceFile {
        name: ResourceName::new(
            "",
            ResourceType::parse(&path_data.resource_dir)
                .ok_or_else(|| anyhow!("invalid resource type '{}'", path_data.resource_dir))?,
            &path_data.name,
        ),
        config: path_data.config,
        file_type: FileType::Png,
        source: path_data.source.clone(),
        exported_symbols: Vec::new(),
        flag_status,
        flag,
    };

    let processed = if path_data.extension == "9.png" {
        png::process_nine_patch(data, options.png_compression_level)
            .map_err(|e| anyhow!("{}: {e}", path_data.source))?
    } else {
        png::crunch_png(data, options.png_compression_level)
            .map_err(|e| anyhow!("{}: {e}", path_data.source))?
    };
    if options.verbose {
        diag.note_at(
            path_data.source.clone(),
            format!("compiled PNG: {} -> {} bytes", data.len(), processed.len()),
        );
    }

    let compiled_file_pb = pb::encode_compiled_file(&file);
    let mut writer = ContainerWriter::new(1);
    writer.add_res_file(&compiled_file_pb, &processed);
    Ok(CompiledArtifact {
        name: String::new(),
        data: writer.finish()?,
        text_symbols: vec![],
    })
}

fn compile_file(
    path_data: &ResourcePathData,
    data: &[u8],
    _options: &CompileOptions,
) -> Result<CompiledArtifact> {
    let ty = ResourceType::parse(&path_data.resource_dir)
        .ok_or_else(|| anyhow!("invalid file path '{}'", path_data.source))?;
    let file = ResourceFile {
        name: ResourceName::new("", ty, &path_data.name),
        config: path_data.config,
        file_type: FileType::Unknown,
        source: path_data.source.clone(),
        exported_symbols: Vec::new(),
        flag_status: FlagStatus::NoFlag,
        flag: None,
    };
    let compiled_file_pb = pb::encode_compiled_file(&file);
    let mut writer = ContainerWriter::new(1);
    writer.add_res_file(&compiled_file_pb, data);
    Ok(CompiledArtifact {
        name: String::new(),
        data: writer.finish()?,
        text_symbols: vec![],
    })
}

// ───────────────────────── driver ─────────────────────────

/// Whether a path component marks the file as hidden (mirrors
/// `file::IsHidden`).
fn is_hidden(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// Compiles a set of inputs, returning the artifacts in input order.
/// `inputs` are `(path-as-given, file bytes)` pairs whose paths must
/// follow the `…/<type>[-config]/<file>` layout.
pub fn compile_inputs(
    inputs: &[(String, Vec<u8>)],
    options: &CompileOptions,
    diag: &Diagnostics,
) -> Result<Vec<CompiledArtifact>> {
    let mut artifacts = Vec::new();
    let mut error = false;
    for (path, data) in inputs {
        if is_hidden(path) {
            continue;
        }
        match compile_data(path, data, options, diag) {
            Ok(artifact) => artifacts.push(artifact),
            Err(e) => {
                diag.error(format!("{e}"));
                diag.error(format!("{path}: file failed to compile"));
                error = true;
            }
        }
    }
    if error {
        bail!("compile failed with {} error(s)", diag.error_count());
    }
    Ok(artifacts)
}

/// The `aapt2 compile` entry point: gathers inputs from files, a res
/// dir, or a res zip, and writes `.flat` containers to `output_path`
/// (a directory if it exists as one, otherwise a flat zip).
pub fn compile(
    file_args: &[PathBuf],
    output_path: &Path,
    options: &CompileOptions,
    diag: &Diagnostics,
) -> Result<()> {
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();

    if options.res_dir.is_some() && options.res_zip.is_some() {
        bail!("only one of --dir and --zip can be specified");
    }
    if let Some(res_dir) = &options.res_dir {
        if !file_args.is_empty() {
            bail!("files given but --dir specified");
        }
        let mut paths = Vec::new();
        collect_res_dir(res_dir, res_dir, &mut paths)?;
        paths.sort();
        for (rel, abs) in paths {
            let data =
                std::fs::read(&abs).with_context(|| format!("failed to open {}", abs.display()))?;
            inputs.push((rel, data));
        }
    } else if let Some(res_zip) = &options.res_zip {
        if !file_args.is_empty() {
            bail!("files given but --zip specified");
        }
        let file = std::fs::File::open(res_zip)
            .with_context(|| format!("failed to open {}", res_zip.display()))?;
        let mut archive = zip::ZipArchive::new(file)?;
        let mut names: Vec<String> = archive.file_names().map(String::from).collect();
        names.sort();
        for name in names {
            let mut entry = archive.by_name(&name)?;
            if entry.is_dir() {
                continue;
            }
            let mut data = Vec::new();
            std::io::copy(&mut entry, &mut data)?;
            inputs.push((name, data));
        }
    } else {
        if options.source_path.is_some() && file_args.len() > 1 {
            bail!("Cannot use an overriding source path with multiple files.");
        }
        let mut sorted: Vec<&PathBuf> = file_args.iter().collect();
        sorted.sort();
        for path in sorted {
            if !path.is_file() && !path.is_symlink() {
                if path.is_dir() {
                    bail!("{}: resource file cannot be a directory", path.display());
                }
                bail!("{}: file not found", path.display());
            }
            let data = std::fs::read(path)
                .with_context(|| format!("failed to open {}", path.display()))?;
            inputs.push((path.to_string_lossy().into_owned(), data));
        }
    }

    let artifacts = compile_inputs(&inputs, options, diag)?;

    // Write text symbols if requested.
    if let Some(symbols_path) = &options.generate_text_symbols_path {
        let mut out = String::new();
        for artifact in &artifacts {
            for line in &artifact.text_symbols {
                out.push_str(line);
                out.push('\n');
            }
        }
        std::fs::write(symbols_path, out)
            .with_context(|| format!("failed writing to '{}'", symbols_path.display()))?;
    }

    write_artifacts(&artifacts, output_path)
}

/// Recursively collects `res/<type>/<file>` paths (two levels).
fn collect_res_dir(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_res_dir(root, &path, out)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            // Prefix with the res dir's name so path extraction sees
            // <type>/<file> as the trailing components.
            out.push((relative, path));
        }
    }
    Ok(())
}

/// Writes artifacts to a directory (if `output_path` is an existing
/// directory) or to a flat zip (entries stored uncompressed, matching
/// aapt2).
pub fn write_artifacts(artifacts: &[CompiledArtifact], output_path: &Path) -> Result<()> {
    if output_path.is_dir() {
        for artifact in artifacts {
            std::fs::write(output_path.join(&artifact.name), &artifact.data)?;
        }
        return Ok(());
    }
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = std::fs::File::create(output_path)
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    let mut writer = zip::ZipWriter::new(file);
    let zip_options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for artifact in artifacts {
        writer.start_file(&*artifact.name, zip_options)?;
        writer.write_all(&artifact.data)?;
    }
    writer.finish()?;
    Ok(())
}

/// Reads compiled artifacts back from a flat zip or directory —
/// the inverse of [`write_artifacts`], used by `link`.
pub fn read_artifacts(path: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "flat"))
            .collect();
        entries.sort();
        for entry in entries {
            out.push((
                entry.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&entry)?,
            ));
        }
    } else {
        let file = std::fs::File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let mut data = Vec::new();
            std::io::copy(&mut entry, &mut data)?;
            out.push((entry.name().to_string(), data));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_data_extraction() {
        let data = extract_resource_path_data("res/values-de/strings.xml", None).unwrap();
        assert_eq!(data.resource_dir, "values");
        assert_eq!(data.config_str, "de");
        assert_eq!(data.name, "strings");
        assert_eq!(data.extension, "xml");

        let data = extract_resource_path_data("res/drawable-hdpi/icon.9.png", None).unwrap();
        assert_eq!(data.extension, "9.png");
        assert_eq!(data.name, "icon");

        let data = extract_resource_path_data("res/flag(my.flag)/values/bools.xml", None).unwrap();
        assert_eq!(data.flag_name, "my.flag");
        assert_eq!(data.resource_dir, "values");

        // "qqq" is a valid 3-letter language qualifier; use something
        // that can never parse as a configuration.
        assert!(extract_resource_path_data("res/values-notaqualifier/x.xml", None).is_err());
        assert!(extract_resource_path_data("strings.xml", None).is_err());
    }

    #[test]
    fn output_filenames() {
        let data = extract_resource_path_data("res/values-de/strings.xml", None).unwrap();
        let mut renamed = data.clone();
        renamed.extension = "arsc".to_string();
        assert_eq!(
            build_output_filename(&renamed),
            "values-de_strings.arsc.flat"
        );

        let data = extract_resource_path_data("res/layout/main.xml", None).unwrap();
        assert_eq!(build_output_filename(&data), "layout_main.xml.flat");

        let data = extract_resource_path_data("res/drawable-hdpi/icon.9.png", None).unwrap();
        assert_eq!(
            build_output_filename(&data),
            "drawable-hdpi_icon.9.png.flat"
        );
    }

    #[test]
    fn feature_flag_parsing() {
        let mut flags = FeatureFlagValues::new();
        parse_feature_flags_parameter("one=true,two:ro=false,three=", &mut flags).unwrap();
        assert_eq!(
            flags.get("one"),
            Some(&FeatureFlagProperties {
                read_only: false,
                enabled: Some(true)
            })
        );
        assert_eq!(
            flags.get("two"),
            Some(&FeatureFlagProperties {
                read_only: true,
                enabled: Some(false)
            })
        );
        assert_eq!(
            flags.get("three"),
            Some(&FeatureFlagProperties {
                read_only: false,
                enabled: None
            })
        );
        assert!(parse_feature_flags_parameter("one=false", &mut flags).is_err());
    }
}
