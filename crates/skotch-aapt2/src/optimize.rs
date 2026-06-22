//! `aapt2 optimize` — post-link APK size optimizations.
//!
//! Port of `cmd/Optimize.cpp`. Currently covers: config filtering
//! (`-c`), target-density stripping (`--target-densities`), value
//! deduplication (`--deduplicate-entry-values` reuses the link-phase
//! deduper), and re-flattening with sparse/compact encoding. Resource
//! name collapsing and path shortening are not yet ported.

use crate::apk::{ApkFormat, LoadedApk};
use crate::cli::ParsedArgs;
use crate::diag::Diagnostics;
use crate::res::config::ConfigDescription;
use anyhow::{anyhow, bail, Result};
use std::path::Path;

pub fn run(args: &[String], diag: &Diagnostics) -> Result<i32> {
    let parsed = ParsedArgs::parse(
        args,
        &[
            "-o",
            "-d",
            "-x",
            "--target-densities",
            "--resources-config-path",
            "-c",
            "--split",
            "--keep-artifacts",
            "--resource-path-shortening-map",
            "--save-obfuscation-map",
        ],
        &[
            "-p",
            "--enable-sparse-encoding",
            "--force-sparse-encoding",
            "--enable-compact-entries",
            "--collapse-resource-names",
            "--shorten-resource-paths",
            "--deduplicate-entry-values",
            "-v",
        ],
    )?;

    if parsed.has("--collapse-resource-names") || parsed.has("--shorten-resource-paths") {
        bail!("resource name collapsing / path shortening are not yet supported by skotch aapt2");
    }
    if !parsed.values("--split").is_empty() {
        bail!("--split is not yet supported by skotch aapt2 optimize");
    }

    let output = parsed
        .value("-o")
        .ok_or_else(|| anyhow!("-o flag is required"))?;
    if parsed.positional.len() != 1 {
        bail!("must have one APK as argument");
    }
    let input = Path::new(&parsed.positional[0]);
    let mut apk = LoadedApk::load(input, diag)?;
    if apk.format != ApkFormat::Binary {
        bail!("{}: optimize only supports binary-format APKs", apk.source);
    }

    let mut table = std::mem::take(&mut apk.table);

    // Config filtering: keep only values matching one of the -c configs
    // (a config matches when it's compatible with a requested one).
    let mut keep_configs: Vec<ConfigDescription> = Vec::new();
    for list in parsed.values("-c") {
        for config_str in list.split(',') {
            keep_configs.push(
                ConfigDescription::parse(config_str)
                    .ok_or_else(|| anyhow!("invalid config '{config_str}' for -c option"))?,
            );
        }
    }
    if !keep_configs.is_empty() {
        for package in &mut table.packages {
            for ty in &mut package.types {
                for entry in &mut ty.entries {
                    entry.values.retain(|value| {
                        value.config.is_default()
                            || keep_configs.iter().any(|keep| value.config.matches(keep))
                    });
                }
            }
        }
    }

    // Density stripping: for each (entry, density-stripped config
    // group), keep the best density for each target.
    let mut target_densities: Vec<u16> = Vec::new();
    if let Some(densities) = parsed.value("--target-densities") {
        for density_str in densities.split(',') {
            let config = ConfigDescription::parse(density_str)
                .filter(|c| c.density != 0)
                .ok_or_else(|| {
                    anyhow!("invalid density '{density_str}' for --target-densities option")
                })?;
            target_densities.push(config.density);
        }
    }
    if !target_densities.is_empty() {
        strip_densities(&mut table, &target_densities);
    }

    if parsed.has("--deduplicate-entry-values") {
        crate::link::transforms::dedupe_resources(&mut table)?;
    }

    // Re-pack the APK with the optimized table.
    apk.write_with_table(
        &table,
        Path::new(output),
        parsed.has("--enable-sparse-encoding") || parsed.has("--force-sparse-encoding"),
        parsed.has("--enable-compact-entries"),
    )?;
    Ok(0)
}

/// Keeps, per config-group, only the densities closest to each target.
fn strip_densities(table: &mut crate::res::table::ResourceTable, targets: &[u16]) {
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                let values = std::mem::take(&mut entry.values);
                // Group by config-without-density.
                let mut groups: Vec<(Vec<u8>, Vec<crate::res::table::ResourceConfigValue>)> =
                    Vec::new();
                for value in values {
                    let mut key_config = value.config;
                    key_config.density = 0;
                    let key = key_config.to_bytes();
                    match groups.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, group)) => group.push(value),
                        None => groups.push((key, vec![value])),
                    }
                }
                let mut kept = Vec::new();
                for (_, group) in groups {
                    if group.len() == 1 || group.iter().all(|v| v.config.density == 0) {
                        kept.extend(group);
                        continue;
                    }
                    let mut keep_indices = std::collections::BTreeSet::new();
                    for &target in targets {
                        // Closest density wins (preferring higher).
                        let mut best: Option<(usize, i64)> = None;
                        for (index, value) in group.iter().enumerate() {
                            let density = value.config.density;
                            if density == 0 {
                                continue;
                            }
                            let score = if density >= target {
                                (density - target) as i64
                            } else {
                                2 * (target - density) as i64
                            };
                            if best.is_none() || score < best.unwrap().1 {
                                best = Some((index, score));
                            }
                        }
                        if let Some((index, _)) = best {
                            keep_indices.insert(index);
                        }
                    }
                    for (index, value) in group.into_iter().enumerate() {
                        if value.config.density == 0 || keep_indices.contains(&index) {
                            kept.push(value);
                        }
                    }
                }
                kept.sort_by(|a, b| {
                    a.config
                        .cmp(&b.config)
                        .then_with(|| a.product.cmp(&b.product))
                });
                entry.values = kept;
            }
        }
    }
}
