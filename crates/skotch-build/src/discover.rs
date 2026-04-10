//! Source file discovery and build file location.

use std::io;
use std::path::{Path, PathBuf};

/// Walk a directory tree collecting `.kt` source files, sorted by path.
pub fn discover_sources(root: &Path) -> io::Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension() {
                if ext == "kt" {
                    files.push(path.to_path_buf());
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Walk up from `start` looking for `build.gradle.kts`.
pub fn find_build_file(start: &Path) -> Option<PathBuf> {
    find_file_upward(start, "build.gradle.kts")
}

/// Walk up from `start` looking for `settings.gradle.kts`.
pub fn find_settings_file(start: &Path) -> Option<PathBuf> {
    find_file_upward(start, "settings.gradle.kts")
}

fn find_file_upward(start: &Path, name: &str) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    loop {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}
