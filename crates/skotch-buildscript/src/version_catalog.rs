//! Parser for Gradle Version Catalog files (`libs.versions.toml`).
//!
//! The version catalog is a TOML file with three well-known tables:
//! `[versions]`, `[libraries]`, and `[plugins]`.  This module provides
//! a lightweight hand-rolled parser that extracts the information the
//! build system needs without pulling in a full TOML library.
//!
//! Library entries reference a version through `version.ref = "key"`,
//! which is resolved against the `[versions]` table during parsing.

use std::collections::HashMap;
use std::path::Path;

// ── Public types ────────────────────────────────────────────────────────

/// A parsed Gradle version catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionCatalog {
    pub versions: HashMap<String, String>,
    pub libraries: HashMap<String, LibraryDef>,
    pub plugins: HashMap<String, PluginDef>,
}

/// A resolved library dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryDef {
    pub group: String,
    pub name: String,
    /// The concrete version string, already resolved from `version.ref`.
    pub version: String,
}

impl LibraryDef {
    /// Returns the Maven coordinate `group:name:version`.
    pub fn coordinate(&self) -> String {
        format!("{}:{}:{}", self.group, self.name, self.version)
    }
}

/// A resolved plugin declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDef {
    pub id: String,
    pub version: String,
}

// ── Entry point ─────────────────────────────────────────────────────────

/// Read and parse a version catalog file from disk.
pub fn parse_version_catalog(path: &Path) -> Result<VersionCatalog, CatalogError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| CatalogError::Io(path.to_path_buf(), e))?;
    parse_version_catalog_str(&text)
}

/// Parse a version catalog from an in-memory string.
pub fn parse_version_catalog_str(src: &str) -> Result<VersionCatalog, CatalogError> {
    let mut catalog = VersionCatalog::default();
    let mut section = Section::None;

    for (line_no, raw_line) in src.lines().enumerate() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        // Section header: `[versions]`, `[libraries]`, `[plugins]`, etc.
        if let Some(header) = parse_section_header(line) {
            section = match header {
                "versions" => Section::Versions,
                "libraries" => Section::Libraries,
                "plugins" => Section::Plugins,
                _ => Section::Unknown,
            };
            continue;
        }

        match section {
            Section::Versions => {
                let (key, value) = parse_kv(line)
                    .ok_or_else(|| CatalogError::Syntax(line_no + 1, raw_line.to_string()))?;
                catalog.versions.insert(key, value);
            }
            Section::Libraries => {
                let (key, raw_value) = parse_kv_raw(line)
                    .ok_or_else(|| CatalogError::Syntax(line_no + 1, raw_line.to_string()))?;
                let lib = parse_library_value(&raw_value, &catalog.versions)
                    .ok_or_else(|| CatalogError::Syntax(line_no + 1, raw_line.to_string()))?;
                catalog.libraries.insert(key, lib);
            }
            Section::Plugins => {
                let (key, raw_value) = parse_kv_raw(line)
                    .ok_or_else(|| CatalogError::Syntax(line_no + 1, raw_line.to_string()))?;
                let plugin = parse_plugin_value(&raw_value, &catalog.versions)
                    .ok_or_else(|| CatalogError::Syntax(line_no + 1, raw_line.to_string()))?;
                catalog.plugins.insert(key, plugin);
            }
            Section::None | Section::Unknown => {
                // Silently skip unknown sections / top-level keys.
            }
        }
    }

    Ok(catalog)
}

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum CatalogError {
    Io(std::path::PathBuf, std::io::Error),
    Syntax(usize, String),
    MissingVersionRef(String),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Io(path, err) => {
                write!(f, "failed to read {}: {}", path.display(), err)
            }
            CatalogError::Syntax(line, text) => {
                write!(f, "syntax error on line {}: {}", line, text)
            }
            CatalogError::MissingVersionRef(key) => {
                write!(f, "unresolved version.ref: \"{}\"", key)
            }
        }
    }
}

impl std::error::Error for CatalogError {}

// ── Internal types ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Versions,
    Libraries,
    Plugins,
    Unknown,
}

// ── Low-level parsing helpers ───────────────────────────────────────────

/// Strip a TOML line comment (`# ...`) that is not inside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            '#' if !in_quote => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Parse `[section-name]` and return the inner name.
fn parse_section_header(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('[') && line.ends_with(']') && !line.starts_with("[[") {
        Some(line[1..line.len() - 1].trim())
    } else {
        None
    }
}

/// Parse a simple `key = "value"` line, returning unquoted strings.
fn parse_kv(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim().to_string();
    let value = unquote(value.trim())?;
    Some((key, value))
}

/// Parse `key = <rest>` returning the key and the raw (untrimmed) RHS.
fn parse_kv_raw(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim().to_string();
    let value = value.trim().to_string();
    Some((key, value))
}

/// Remove surrounding double quotes from a TOML string value.
fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

/// Parse an inline table value for a library entry.
///
/// Supports two forms:
///   1. `{ group = "...", name = "...", version.ref = "..." }`
///   2. `{ group = "...", name = "...", version = "..." }`
///   3. `"group:name:version"` (module notation)
fn parse_library_value(raw: &str, versions: &HashMap<String, String>) -> Option<LibraryDef> {
    let raw = raw.trim();

    // Module notation: "group:name:version"
    if raw.starts_with('"') {
        let module = unquote(raw)?;
        let parts: Vec<&str> = module.splitn(3, ':').collect();
        if parts.len() == 3 {
            return Some(LibraryDef {
                group: parts[0].to_string(),
                name: parts[1].to_string(),
                version: parts[2].to_string(),
            });
        }
        return None;
    }

    // Inline table: { group = "...", name = "...", version.ref = "..." }
    let inner = strip_braces(raw)?;
    let fields = parse_inline_table(inner);

    let group = fields.get("group").cloned()?;
    let name = fields.get("name").cloned()?;

    // `version.ref` → look up in [versions]; `version` → literal.
    let version = if let Some(ref_key) = fields.get("version.ref") {
        versions
            .get(ref_key)
            .cloned()
            .unwrap_or_else(|| ref_key.clone())
    } else if let Some(v) = fields.get("version") {
        v.clone()
    } else {
        String::new()
    };

    Some(LibraryDef {
        group,
        name,
        version,
    })
}

/// Parse an inline table value for a plugin entry.
///
/// Supports:
///   1. `{ id = "...", version.ref = "..." }`
///   2. `{ id = "...", version = "..." }`
fn parse_plugin_value(raw: &str, versions: &HashMap<String, String>) -> Option<PluginDef> {
    let raw = raw.trim();
    let inner = strip_braces(raw)?;
    let fields = parse_inline_table(inner);

    let id = fields.get("id").cloned()?;

    let version = if let Some(ref_key) = fields.get("version.ref") {
        versions
            .get(ref_key)
            .cloned()
            .unwrap_or_else(|| ref_key.clone())
    } else if let Some(v) = fields.get("version") {
        v.clone()
    } else {
        String::new()
    };

    Some(PluginDef { id, version })
}

/// Strip the outer `{` and `}` from an inline table.
fn strip_braces(s: &str) -> Option<&str> {
    let s = s.trim();
    if s.starts_with('{') && s.ends_with('}') {
        Some(&s[1..s.len() - 1])
    } else {
        None
    }
}

/// Parse the comma-separated `key = "value"` pairs inside an inline table.
///
/// Handles dotted keys like `version.ref` as a single key string.
fn parse_inline_table(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in split_inline_fields(s) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            let key = k.trim().to_string();
            if let Some(val) = unquote(v.trim()) {
                map.insert(key, val);
            }
        }
    }
    map
}

/// Split inline table content by commas, respecting quoted strings.
fn split_inline_fields(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_quote = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            ',' if !in_quote => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        parts.push(&s[start..]);
    }
    parts
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_catalog() {
        let src = r#"
[versions]
kotlin = "2.0.0"
compose-bom = "2024.06.00"
activity = "1.9.0"

[libraries]
kotlin-stdlib = { group = "org.jetbrains.kotlin", name = "kotlin-stdlib", version.ref = "kotlin" }
compose-bom = { group = "androidx.compose", name = "compose-bom", version.ref = "compose-bom" }
activity-compose = { group = "androidx.activity", name = "activity-compose", version.ref = "activity" }

[plugins]
kotlin-android = { id = "org.jetbrains.kotlin.android", version.ref = "kotlin" }
"#;
        let catalog = parse_version_catalog_str(src).unwrap();

        // Versions
        assert_eq!(catalog.versions.len(), 3);
        assert_eq!(catalog.versions["kotlin"], "2.0.0");
        assert_eq!(catalog.versions["compose-bom"], "2024.06.00");
        assert_eq!(catalog.versions["activity"], "1.9.0");

        // Libraries
        assert_eq!(catalog.libraries.len(), 3);
        let stdlib = &catalog.libraries["kotlin-stdlib"];
        assert_eq!(stdlib.group, "org.jetbrains.kotlin");
        assert_eq!(stdlib.name, "kotlin-stdlib");
        assert_eq!(stdlib.version, "2.0.0");

        let bom = &catalog.libraries["compose-bom"];
        assert_eq!(bom.group, "androidx.compose");
        assert_eq!(bom.name, "compose-bom");
        assert_eq!(bom.version, "2024.06.00");

        let activity = &catalog.libraries["activity-compose"];
        assert_eq!(activity.group, "androidx.activity");
        assert_eq!(activity.name, "activity-compose");
        assert_eq!(activity.version, "1.9.0");
        assert_eq!(
            activity.coordinate(),
            "androidx.activity:activity-compose:1.9.0"
        );

        // Plugins
        assert_eq!(catalog.plugins.len(), 1);
        let plugin = &catalog.plugins["kotlin-android"];
        assert_eq!(plugin.id, "org.jetbrains.kotlin.android");
        assert_eq!(plugin.version, "2.0.0");
    }

    #[test]
    fn parse_module_notation() {
        let src = r#"
[versions]

[libraries]
guava = "com.google.guava:guava:33.0.0-jre"
"#;
        let catalog = parse_version_catalog_str(src).unwrap();
        let guava = &catalog.libraries["guava"];
        assert_eq!(guava.group, "com.google.guava");
        assert_eq!(guava.name, "guava");
        assert_eq!(guava.version, "33.0.0-jre");
    }

    #[test]
    fn parse_literal_version() {
        let src = r#"
[libraries]
junit = { group = "junit", name = "junit", version = "4.13.2" }
"#;
        let catalog = parse_version_catalog_str(src).unwrap();
        let junit = &catalog.libraries["junit"];
        assert_eq!(junit.group, "junit");
        assert_eq!(junit.name, "junit");
        assert_eq!(junit.version, "4.13.2");
    }

    #[test]
    fn comments_are_stripped() {
        let src = r#"
# This is a comment
[versions]
kotlin = "2.0.0"  # inline comment
"#;
        let catalog = parse_version_catalog_str(src).unwrap();
        assert_eq!(catalog.versions["kotlin"], "2.0.0");
    }

    #[test]
    fn unknown_sections_are_skipped() {
        let src = r#"
[metadata]
format.version = "1.1"

[versions]
kotlin = "2.0.0"

[bundles]
compose = ["compose-bom", "activity-compose"]
"#;
        let catalog = parse_version_catalog_str(src).unwrap();
        assert_eq!(catalog.versions.len(), 1);
        assert_eq!(catalog.versions["kotlin"], "2.0.0");
        assert!(catalog.libraries.is_empty());
        assert!(catalog.plugins.is_empty());
    }

    #[test]
    fn empty_catalog() {
        let catalog = parse_version_catalog_str("").unwrap();
        assert!(catalog.versions.is_empty());
        assert!(catalog.libraries.is_empty());
        assert!(catalog.plugins.is_empty());
    }
}
