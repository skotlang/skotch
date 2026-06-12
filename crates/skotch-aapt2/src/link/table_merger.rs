//! Merging of compiled tables and files into the final table.
//!
//! Port of `link/TableMerger.{h,cpp}`.
//!
//! STUB: signatures are final; the full port (visibility collision
//! rules, style merging, mangling for static libs) is in progress.

use crate::diag::Diagnostics;
use crate::res::config::ConfigDescription;
use crate::res::table::{NewResource, OnIdConflict, ResourceTable};
use crate::res::value::{FileReference, FileType, Item, Value};
use crate::res::{ResourceFile, ResourceName, Source, SourcedResourceName};
use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, Default)]
pub struct TableMergerOptions {
    /// `--auto-add-overlay`: new resources in overlays are allowed.
    pub auto_add_overlay: bool,
    /// `--override-styles-instead-of-overlaying`.
    pub override_styles_instead_of_overlaying: bool,
    /// `--strict-visibility`.
    pub strict_visibility: bool,
}

/// Merges `incoming` into `table` under the compilation package.
/// Mirrors `TableMerger::Merge`.
pub fn merge_table(
    table: &mut ResourceTable,
    compilation_package: &str,
    incoming: ResourceTable,
    overlay: bool,
    options: &TableMergerOptions,
    diag: &Diagnostics,
) -> Result<()> {
    let _ = options;
    for package in incoming.packages {
        // Resources are merged under the compilation package; an
        // explicitly named foreign package would need mangling
        // (static libraries) which is handled by the merger port.
        let target_package = if package.name.is_empty() || package.name == compilation_package {
            compilation_package.to_string()
        } else {
            package.name.clone()
        };
        for ty in package.types {
            for entry in ty.entries {
                let name = ResourceName::with_named_type(
                    target_package.clone(),
                    ty.named_type.clone(),
                    &entry.name,
                );
                // Entry-level state.
                let mut shell = NewResource::with_name(name.clone()).allow_mangled(true);
                if entry.visibility.level != Default::default() {
                    shell = shell.visibility(entry.visibility.clone());
                }
                if let Some(allow_new) = entry.allow_new.clone() {
                    shell = shell.allow_new(allow_new);
                }
                if let Some(overlayable) = entry.overlayable_item.clone() {
                    shell = shell.overlayable(overlayable);
                }
                if let Some(staged_id) = entry.staged_id {
                    shell = shell.staged_id(staged_id);
                }
                if let Some(id) = entry.id {
                    shell = shell.id_with_conflict(id, OnIdConflict::CreateEntry);
                }
                table.add_resource_overlay(shell).map_err(|e| {
                    diag.error(format!("{e}"));
                    anyhow::anyhow!("failed to merge resource {name}")
                })?;

                for config_value in entry.values {
                    let Some(value) = config_value.value else { continue };
                    let new_resource = NewResource::with_name(name.clone())
                        .config(config_value.config)
                        .product(config_value.product)
                        .value(value)
                        .allow_mangled(true);
                    let result = if overlay {
                        table.add_resource_overlay(new_resource)
                    } else {
                        table.add_resource(new_resource)
                    };
                    if let Err(e) = result {
                        diag.error(format!("{e}"));
                        bail!("failed to merge resource {name}");
                    }
                }
                for config_value in entry.flag_disabled_values {
                    let Some(value) = config_value.value else { continue };
                    let new_resource = NewResource::with_name(name.clone())
                        .config(config_value.config)
                        .product(config_value.product)
                        .value(value)
                        .allow_mangled(true);
                    if let Err(e) = table.add_resource(new_resource) {
                        diag.error(format!("{e}"));
                        bail!("failed to merge resource {name}");
                    }
                }
            }
        }
    }
    for overlayable in incoming.overlayables {
        if !table
            .overlayables
            .iter()
            .any(|o| o.name == overlayable.name && o.actor == overlayable.actor)
        {
            table.overlayables.push(overlayable);
        }
    }
    Ok(())
}

/// Merges one compiled file (layout/drawable/raw/PNG) into the table as
/// a `FileReference` value. Mirrors `TableMerger::MergeFile`.
pub fn merge_compiled_file(
    table: &mut ResourceTable,
    compilation_package: &str,
    file: ResourceFile,
    payload: Vec<u8>,
    overlay: bool,
    _options: &TableMergerOptions,
    diag: &Diagnostics,
) -> Result<()> {
    // Destination path: res/<type>[-config]/<entry>.<ext>
    let extension = extension_for(&file);
    let mut dest = format!("res/{}", file.name.ty.name);
    let config_str = file.config.to_string();
    if !config_str.is_empty() {
        dest.push('-');
        dest.push_str(&config_str);
    }
    dest.push('/');
    dest.push_str(&file.name.entry);
    if !extension.is_empty() {
        dest.push('.');
        dest.push_str(extension);
    }

    let mut value = Value::item(Item::FileReference(FileReference {
        path: dest,
        file_type: file.file_type,
        file_contents: Some(std::sync::Arc::new(payload)),
    }));
    value.meta.source = file.source.clone();
    value.meta.flag = file.flag.clone();
    value.meta.flag_status = file.flag_status;

    let name = ResourceName::with_named_type(
        compilation_package,
        file.name.ty.clone(),
        &file.name.entry,
    );
    let new_resource = NewResource::with_name(name.clone())
        .config(file.config.clone())
        .value(value)
        .allow_mangled(true);
    let result = if overlay {
        table.add_resource_overlay(new_resource)
    } else {
        table.add_resource(new_resource)
    };
    if let Err(e) = result {
        diag.error(format!("{e}"));
        bail!("failed to merge file {}", file.source);
    }

    merge_exported_symbols(table, compilation_package, &file.source, file.exported_symbols)
}

fn extension_for(file: &ResourceFile) -> &'static str {
    // The original extension travels in the source path.
    let path = &file.source.path;
    if path.ends_with(".9.png") {
        "9.png"
    } else if let Some(dot) = path.rfind('.') {
        match &path[dot + 1..] {
            "png" => "png",
            "xml" => "xml",
            "ttf" => "ttf",
            "otf" => "otf",
            "ttc" => "ttc",
            "txt" => "txt",
            "webp" => "webp",
            "jpg" => "jpg",
            "jpeg" => "jpeg",
            "gif" => "gif",
            "mp3" => "mp3",
            "mp4" => "mp4",
            "ogg" => "ogg",
            "json" => "json",
            _ => {
                // Preserve unknown extensions by leaking them is not
                // worth it; common ones are listed above. Fall back to
                // xml for compiled XML, none otherwise.
                if file.file_type == FileType::ProtoXml || file.file_type == FileType::BinaryXml {
                    "xml"
                } else {
                    ""
                }
            }
        }
    } else {
        ""
    }
}

/// Adds `@+id` symbols exported by a compiled file (or the manifest).
/// Mirrors `TableMerger::MergeExportedSymbols`.
pub fn merge_exported_symbols(
    table: &mut ResourceTable,
    compilation_package: &str,
    source: &Source,
    symbols: Vec<SourcedResourceName>,
) -> Result<()> {
    for symbol in symbols {
        let mut value = Value::item(Item::Id);
        value.meta.source = Source::with_line(&source.path, symbol.line);
        value.meta.weak = true;
        let name = ResourceName::with_named_type(
            compilation_package,
            symbol.name.ty.clone(),
            &symbol.name.entry,
        );
        table
            .add_resource(
                NewResource::with_name(name)
                    .config(ConfigDescription::default())
                    .value(value)
                    .allow_mangled(true),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(())
}

/// Ensures only the compilation package (or empty) is present.
/// Mirrors `Linker::VerifyNoExternalPackages`.
pub fn verify_no_external_packages(
    table: &ResourceTable,
    compilation_package: &str,
) -> Result<()> {
    for package in &table.packages {
        if !package.name.is_empty() && package.name != compilation_package {
            bail!(
                "package '{}' is not the compilation package '{compilation_package}'; \
                 did you forget --auto-add-overlay or a library include?",
                package.name
            );
        }
    }
    Ok(())
}
