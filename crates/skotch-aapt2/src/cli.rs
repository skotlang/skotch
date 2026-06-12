//! The `aapt2` command-line interface.
//!
//! Port of `cmd/Command.{h,cpp}` flag handling plus the per-command
//! flag tables from `cmd/Compile.h`, `cmd/Link.h`, `cmd/Dump.h`,
//! `cmd/Optimize.h`, and `cmd/Convert.h`. Legacy/deprecated options
//! are accepted only where build systems still pass them; truly
//! removed flags error out.

use crate::compile::CompileOptions;
use crate::diag::Diagnostics;
use crate::link::manifest_fixer::ManifestFixerOptions;
use crate::link::{LinkOptions, OutputFormat, PackageType};
use crate::res::table::VisibilityLevel;
use anyhow::{anyhow, bail, Result};
use std::io::BufRead;
use std::path::PathBuf;

/// Entry point: `args` excludes the program and `aapt2` itself.
/// Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let diag = Diagnostics::stderr();
    match run_impl(args, &diag) {
        Ok(code) => code,
        Err(e) => {
            // Mirror aapt2: errors go to stderr with "error: ".
            eprintln!("error: {e:#}");
            1
        }
    }
}

fn run_impl(args: &[String], diag: &Diagnostics) -> Result<i32> {
    let Some((command, rest)) = args.split_first() else {
        eprintln!("no subcommand specified");
        print_usage();
        return Ok(1);
    };
    match command.as_str() {
        "compile" | "c" => run_compile(rest, diag),
        "link" | "l" => run_link(rest, diag),
        "dump" | "d" => crate::dump::run(rest, diag),
        "diff" => crate::dump::run_diff(rest, diag),
        "optimize" => crate::optimize::run(rest, diag),
        "convert" => crate::convert::run(rest, diag),
        "version" => {
            eprintln!("Android Asset Packaging Tool (aapt) 2:aapt2-skotch-{}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "daemon" | "m" => run_daemon(diag),
        other => {
            eprintln!("unknown subcommand '{other}'");
            print_usage();
            Ok(1)
        }
    }
}

fn print_usage() {
    eprintln!("\nusage: aapt2 [compile|link|dump|diff|optimize|convert|version|daemon] ...");
}

/// Daemon mode: newline-separated args per command, blank line runs it.
/// Mirrors `DaemonCommand`.
fn run_daemon(diag: &Diagnostics) -> Result<i32> {
    println!("Ready");
    let stdin = std::io::stdin();
    let mut args: Vec<String> = Vec::new();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.is_empty() {
            if args.is_empty() {
                continue;
            }
            if args[0] == "quit" {
                break;
            }
            let code = run_impl(&args, diag).unwrap_or(1);
            if code != 0 {
                eprintln!("Error");
            }
            eprintln!("Done");
            args.clear();
        } else {
            args.push(line);
        }
    }
    if args.first().map(String::as_str) != Some("quit") && !args.is_empty() {
        // EOF mid-command: execute what we have, mirroring getline loop.
        let code = run_impl(&args, diag).unwrap_or(1);
        if code != 0 {
            eprintln!("Error");
        }
        eprintln!("Done");
    }
    println!("Exiting daemon");
    Ok(0)
}

// ───────────────────────── flag parsing ─────────────────────────

/// One parsed invocation: flag values + positional args.
pub struct ParsedArgs {
    pub positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl ParsedArgs {
    /// Parses according to a spec: `takes_value` lists flags expecting a
    /// value; `switches` lists boolean flags.
    pub fn parse(args: &[String], takes_value: &[&str], switches: &[&str]) -> Result<ParsedArgs> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut iter = args.iter().peekable();
        while let Some(arg) = iter.next() {
            if arg == "-h" || arg == "--help" {
                bail!("help requested");
            }
            if arg.starts_with('-') && arg.len() > 1 {
                let name = arg.as_str();
                if takes_value.contains(&name) {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow!("flag '{name}' is missing its argument"))?;
                    flags.push((name.to_string(), Some(value.clone())));
                } else if switches.contains(&name) {
                    flags.push((name.to_string(), None));
                } else {
                    bail!("unknown option '{name}'");
                }
            } else {
                positional.push(arg.clone());
            }
        }
        Ok(ParsedArgs { positional, flags })
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    pub fn values(&self, name: &str) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(n, _)| n == name)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }
}

// ───────────────────────── compile ─────────────────────────

fn run_compile(args: &[String], diag: &Diagnostics) -> Result<i32> {
    let parsed = ParsedArgs::parse(
        args,
        &[
            "-o",
            "--dir",
            "--zip",
            "--output-text-symbols",
            "--visibility",
            "--trace-folder",
            "--source-path",
            "--filter-product",
            "--feature-flags",
            "--png-compression-level",
            "--pseudo-localize-gender-values",
            "--pseudo-localize-gender-ratio",
        ],
        &[
            "--pseudo-localize",
            "--no-crunch",
            "--legacy",
            "--preserve-visibility-of-styleables",
            "--warn-manifest-validation",
            "-v",
        ],
    )?;

    let output = parsed
        .value("-o")
        .ok_or_else(|| anyhow!("-o flag is required"))?
        .to_string();

    let mut options = CompileOptions::new();
    options.res_dir = parsed.value("--dir").map(PathBuf::from);
    options.res_zip = parsed.value("--zip").map(PathBuf::from);
    options.generate_text_symbols_path =
        parsed.value("--output-text-symbols").map(PathBuf::from);
    options.pseudolocalize = parsed.has("--pseudo-localize");
    options.pseudo_localize_gender_values =
        parsed.value("--pseudo-localize-gender-values").map(String::from);
    options.pseudo_localize_gender_ratio =
        parsed.value("--pseudo-localize-gender-ratio").map(String::from);
    options.no_png_crunch = parsed.has("--no-crunch");
    options.legacy_mode = parsed.has("--legacy");
    options.preserve_visibility_of_styleables =
        parsed.has("--preserve-visibility-of-styleables");
    options.source_path = parsed.value("--source-path").map(String::from);
    options.product = parsed.value("--filter-product").map(String::from);
    options.verbose = parsed.has("-v");

    if let Some(level) = parsed.value("--png-compression-level") {
        let valid = level.len() == 1 && level.as_bytes()[0].is_ascii_digit();
        if !valid {
            bail!("PNG compression level should be a number in [0..9] range");
        }
        options.png_compression_level = level.as_bytes()[0] - b'0';
    }

    if let Some(visibility) = parsed.value("--visibility") {
        options.visibility = Some(match visibility {
            "public" => VisibilityLevel::Public,
            "private" => VisibilityLevel::Private,
            "default" => VisibilityLevel::Undefined,
            other => bail!(
                "Unrecognized visibility level passes to --visibility: '{other}'. \
                 Accepted levels: public, private, default"
            ),
        });
    }

    for arg in parsed.values("--feature-flags") {
        let expanded = expand_arg_file(arg)?;
        for chunk in &expanded {
            crate::compile::parse_feature_flags_parameter(chunk, &mut options.feature_flag_values)
                .map_err(|e| anyhow!(e))?;
        }
    }

    diag_verbose(diag, options.verbose);
    let files: Vec<PathBuf> = parsed.positional.iter().map(PathBuf::from).collect();
    crate::compile::compile(&files, &PathBuf::from(output), &options, diag)?;
    Ok(if diag.has_errors() { 1 } else { 0 })
}

fn diag_verbose(_diag: &Diagnostics, _verbose: bool) {
    // Diagnostics is shared immutably; verbosity is carried by options.
}

/// Expands `@file` arguments into their whitespace-separated contents.
fn expand_arg_file(arg: &str) -> Result<Vec<String>> {
    if let Some(path) = arg.strip_prefix('@') {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("failed to read argument file {path}: {e}"))?;
        Ok(content.split_whitespace().map(String::from).collect())
    } else {
        Ok(vec![arg.to_string()])
    }
}

// ───────────────────────── link ─────────────────────────

fn run_link(args: &[String], diag: &Diagnostics) -> Result<i32> {
    let parsed = ParsedArgs::parse(
        args,
        &[
            "-o",
            "--manifest",
            "-I",
            "-A",
            "-R",
            "--package-id",
            "--java",
            "--proguard",
            "--proguard-main-dex",
            "--custom-package",
            "--extra-packages",
            "--add-javadoc-annotation",
            "--output-text-symbols",
            "--min-sdk-version",
            "--target-sdk-version",
            "--version-code",
            "--version-code-major",
            "--version-name",
            "--revision-code",
            "--compile-sdk-version-code",
            "--compile-sdk-version-name",
            "--fingerprint-prefix",
            "--rename-manifest-package",
            "--rename-resources-package",
            "--rename-instrumentation-target-package",
            "--rename-overlay-target-package",
            "--rename-overlay-category",
            "-c",
            "--preferred-density",
            "--product",
            "--split",
            "--exclude-configs",
            "--stable-ids",
            "--emit-ids",
            "--private-symbols",
            "-0",
            "--no-compress-regex",
            "--feature-flags",
            "--trace-folder",
        ],
        &[
            "--proto-format",
            "--no-auto-version",
            "--no-version-vectors",
            "--no-version-transitions",
            "--no-resource-deduping",
            "--no-resource-removal",
            "-x",
            "-z",
            "--no-xml-namespaces",
            "--shared-lib",
            "--static-lib",
            "--proguard-conditional-keep-rules",
            "--proguard-minimal-keep-rules",
            "--no-proguard-location-reference",
            "--no-static-lib-packages",
            "--non-final-ids",
            "--auto-add-overlay",
            "--override-styles-instead-of-overlaying",
            "--no-compile-sdk-metadata",
            "--output-to-dir",
            "--allow-reserved-package-id",
            "--keep-raw-values",
            "--no-compress",
            "--enable-sparse-encoding",
            "--force-sparse-encoding",
            "--enable-compact-entries",
            "--warn-manifest-validation",
            "--strict-visibility",
            "--exclude-sources",
            "--merge-only",
            "--debug-mode",
            "--non-updatable-system",
            "--replace-version",
            "-v",
        ],
    )?;

    let mut options = LinkOptions {
        output_path: PathBuf::from(
            parsed.value("-o").ok_or_else(|| anyhow!("-o flag is required"))?,
        ),
        manifest_path: PathBuf::from(
            parsed
                .value("--manifest")
                .ok_or_else(|| anyhow!("--manifest flag is required"))?,
        ),
        ..Default::default()
    };

    options.include_paths = parsed.values("-I").iter().map(PathBuf::from).collect();
    options.overlay_files = parsed.values("-R").iter().map(PathBuf::from).collect();
    options.assets_dirs = parsed.values("-A").iter().map(PathBuf::from).collect();

    if parsed.has("--static-lib") && parsed.has("--shared-lib") {
        bail!("only one of --shared-lib and --static-lib can be defined");
    }
    options.package_type = if parsed.has("--static-lib") {
        PackageType::StaticLib
    } else if parsed.has("--shared-lib") {
        PackageType::SharedLib
    } else {
        PackageType::App
    };
    if options.package_type != PackageType::App {
        bail!(
            "--static-lib/--shared-lib outputs are not yet supported by skotch aapt2"
        );
    }

    options.output_format =
        if parsed.has("--proto-format") { OutputFormat::Proto } else { OutputFormat::Binary };
    options.output_to_directory = parsed.has("--output-to-dir");

    if let Some(package_id) = parsed.value("--package-id") {
        let value = crate::res::utils::string_to_int(package_id)
            .map(|v| v.data)
            .ok_or_else(|| anyhow!("invalid --package-id '{package_id}'"))?;
        if value > 0xff {
            bail!("invalid --package-id '{package_id}': must fit in a byte");
        }
        options.package_id = Some(value as u8);
    }
    if parsed.has("-x") {
        // Legacy: use package ID 0x01.
        options.package_id = Some(0x01);
        options.allow_reserved_package_id = true;
    }
    options.allow_reserved_package_id |= parsed.has("--allow-reserved-package-id");

    options.generate_java_class_path = parsed.value("--java").map(PathBuf::from);
    options.custom_java_package = parsed.value("--custom-package").map(String::from);
    for extra in parsed.values("--extra-packages") {
        options.extra_java_packages.push(extra.to_string());
    }
    for annotation in parsed.values("--add-javadoc-annotation") {
        options.javadoc_annotations.push(annotation.to_string());
    }
    options.private_symbols = parsed.value("--private-symbols").map(String::from);
    options.generate_proguard_rules_path = parsed.value("--proguard").map(PathBuf::from);
    options.generate_main_dex_proguard_rules_path =
        parsed.value("--proguard-main-dex").map(PathBuf::from);
    options.generate_conditional_proguard_rules =
        parsed.has("--proguard-conditional-keep-rules");
    options.generate_minimal_proguard_rules = parsed.has("--proguard-minimal-keep-rules");
    options.no_proguard_location_reference = parsed.has("--no-proguard-location-reference");
    options.generate_text_symbols_path =
        parsed.value("--output-text-symbols").map(PathBuf::from);

    options.auto_add_overlay = parsed.has("--auto-add-overlay");
    options.override_styles_instead_of_overlaying =
        parsed.has("--override-styles-instead-of-overlaying");
    options.strict_visibility = parsed.has("--strict-visibility");
    options.rename_resources_package =
        parsed.value("--rename-resources-package").map(String::from);
    options.no_static_lib_packages = parsed.has("--no-static-lib-packages");
    options.no_auto_version = parsed.has("--no-auto-version");
    options.no_version_vectors = parsed.has("--no-version-vectors");
    options.no_version_transitions = parsed.has("--no-version-transitions");
    options.no_resource_deduping = parsed.has("--no-resource-deduping");
    options.no_resource_removal = parsed.has("--no-resource-removal");
    options.no_xml_namespaces = parsed.has("--no-xml-namespaces");
    options.require_localization = parsed.has("-z");
    options.merge_only = parsed.has("--merge-only");
    options.exclude_sources = parsed.has("--exclude-sources");
    options.keep_raw_values = parsed.has("--keep-raw-values");
    options.do_not_compress_anything = parsed.has("--no-compress");
    options.regex_to_not_compress = parsed.value("--no-compress-regex").map(String::from);
    options.enable_sparse_encoding = parsed.has("--enable-sparse-encoding");
    options.force_sparse_encoding = parsed.has("--force-sparse-encoding");
    options.enable_compact_entries = parsed.has("--enable-compact-entries");
    options.verbose = parsed.has("-v");

    for extension in parsed.values("-0") {
        options.extensions_to_not_compress.push(extension.to_string());
    }
    for configs in parsed.values("-c") {
        for config in configs.split(',') {
            options.configs.push(config.to_string());
        }
    }
    options.preferred_density = parsed.value("--preferred-density").map(String::from);
    for products in parsed.values("--product") {
        for product in products.split(',') {
            options.products.push(product.to_string());
        }
    }
    for excluded in parsed.values("--exclude-configs") {
        for config in excluded.split(',') {
            options.exclude_configs.push(config.to_string());
        }
    }
    if !parsed.values("--split").is_empty() {
        bail!("--split is not yet supported by skotch aapt2");
    }

    if let Some(stable_ids_path) = parsed.value("--stable-ids") {
        let content = std::fs::read_to_string(stable_ids_path)
            .map_err(|e| anyhow!("failed to read --stable-ids file: {e}"))?;
        options.stable_id_map = crate::link::load_stable_id_map(&content)?;
    }
    options.resource_id_map_path = parsed.value("--emit-ids").map(PathBuf::from);

    for arg in parsed.values("--feature-flags") {
        for chunk in &expand_arg_file(arg)? {
            crate::compile::parse_feature_flags_parameter(chunk, &mut options.feature_flag_values)
                .map_err(|e| anyhow!(e))?;
        }
    }

    options.manifest_fixer_options = ManifestFixerOptions {
        min_sdk_version_default: parsed.value("--min-sdk-version").map(String::from),
        target_sdk_version_default: parsed.value("--target-sdk-version").map(String::from),
        version_code_default: parsed.value("--version-code").map(String::from),
        version_code_major_default: parsed.value("--version-code-major").map(String::from),
        version_name_default: parsed.value("--version-name").map(String::from),
        revision_code_default: parsed.value("--revision-code").map(String::from),
        replace_version: parsed.has("--replace-version"),
        compile_sdk_version: parsed.value("--compile-sdk-version-code").map(String::from),
        compile_sdk_version_codename: parsed
            .value("--compile-sdk-version-name")
            .map(String::from),
        no_compile_sdk_metadata: parsed.has("--no-compile-sdk-metadata"),
        rename_manifest_package: parsed.value("--rename-manifest-package").map(String::from),
        rename_instrumentation_target_package: parsed
            .value("--rename-instrumentation-target-package")
            .map(String::from),
        rename_overlay_target_package: parsed
            .value("--rename-overlay-target-package")
            .map(String::from),
        rename_overlay_category: parsed.value("--rename-overlay-category").map(String::from),
        debug_mode: parsed.has("--debug-mode"),
        warn_validation: parsed.has("--warn-manifest-validation"),
        non_updatable_system: parsed.has("--non-updatable-system"),
        fingerprint_prefixes: parsed
            .values("--fingerprint-prefix")
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };

    // Expand @argument-list files in the inputs (R.flata lists etc.).
    let mut inputs = Vec::new();
    for arg in &parsed.positional {
        for expanded in expand_arg_file(arg)? {
            inputs.push(PathBuf::from(expanded));
        }
    }

    crate::link::link(&inputs, &options, diag)?;
    Ok(if diag.has_errors() { 1 } else { 0 })
}
