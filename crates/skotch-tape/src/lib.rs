//! Maven dependency resolver for Kotlin scripts and Gradle builds.
//!
//! Parses `@file:Repository` and `@file:DependsOn` annotations from `.kts`
//! scripts, resolves transitive dependencies via POM files, downloads JARs,
//! and caches everything under the XDG data directory.
//!
//! ## Usage
//!
//! ```ignore
//! let deps = skotch_tape::resolve_script_deps(source_text)?;
//! println!("classpath: {}", deps.classpath_string());
//! ```

use anyhow::{Context, Result};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

// ─── Public types ───────────────────────────────────────────────────────────

/// A Maven coordinate: `group:artifact:version`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MavenCoord {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl MavenCoord {
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 3 {
            Some(MavenCoord {
                group: parts[0].to_string(),
                artifact: parts[1].to_string(),
                version: parts[2].to_string(),
            })
        } else {
            None
        }
    }

    /// Maven repository path: `org/jetbrains/kotlinx/kotlinx-html-jvm/0.7.3`
    pub fn path(&self) -> String {
        format!(
            "{}/{}/{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version
        )
    }

    /// JAR filename: `kotlinx-html-jvm-0.7.3.jar`
    pub fn jar_name(&self) -> String {
        format!("{}-{}.jar", self.artifact, self.version)
    }

    /// POM filename: `kotlinx-html-jvm-0.7.3.pom`
    pub fn pom_name(&self) -> String {
        format!("{}-{}.pom", self.artifact, self.version)
    }
}

impl std::fmt::Display for MavenCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)
    }
}

/// Resolved dependency set: a list of JAR paths.
#[derive(Clone, Debug, Default)]
pub struct ResolvedDeps {
    pub jars: Vec<PathBuf>,
}

impl ResolvedDeps {
    /// Join JAR paths with the platform separator (`:` on Unix, `;` on Windows).
    pub fn classpath_string(&self) -> String {
        let sep = if cfg!(windows) { ";" } else { ":" };
        self.jars
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(sep)
    }

    pub fn is_empty(&self) -> bool {
        self.jars.is_empty()
    }
}

// ─── Annotation parsing ─────────────────────────────────────────────────────

/// Parse `@file:Repository("url")` and `@file:DependsOn("g:a:v")` from script source.
/// Returns (repositories, dependencies) and the source with annotations stripped.
pub fn parse_script_annotations(source: &str) -> (Vec<String>, Vec<MavenCoord>, String) {
    let mut repos = Vec::new();
    let mut deps = Vec::new();
    let mut clean_lines = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(url) = extract_annotation(trimmed, "Repository") {
            repos.push(url);
        } else if let Some(coord_str) = extract_annotation(trimmed, "DependsOn") {
            if let Some(coord) = MavenCoord::parse(&coord_str) {
                deps.push(coord);
            }
        } else {
            clean_lines.push(line);
        }
    }

    let clean = clean_lines.join("\n");
    (repos, deps, clean)
}

fn extract_annotation(line: &str, name: &str) -> Option<String> {
    // Match: @file:Name("value") or @file:Name("value")
    let pattern = format!("@file:{name}(");
    let start = line.find(&pattern)?;
    let after = &line[start + pattern.len()..];
    // Find the quoted value.
    let q_start = after.find('"')?;
    let rest = &after[q_start + 1..];
    let q_end = rest.find('"')?;
    Some(rest[..q_end].to_string())
}

// ─── Cache ──────────────────────────────────────────────────────────────────

fn cache_dir() -> PathBuf {
    let base = directories::ProjectDirs::from("", "", "skotch")
        .map(|pd| pd.data_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".skotch")
        });
    base.join("cache").join("maven")
}

fn cached_path(coord: &MavenCoord, filename: &str) -> PathBuf {
    cache_dir()
        .join(coord.group.replace('.', "/"))
        .join(&coord.artifact)
        .join(&coord.version)
        .join(filename)
}

// ─── HTTP download ──────────────────────────────────────────────────────────

fn download_artifact(
    coord: &MavenCoord,
    filename: &str,
    repos: &[String],
    client: &reqwest::blocking::Client,
) -> Result<PathBuf> {
    let cached = cached_path(coord, filename);
    if cached.exists() {
        return Ok(cached);
    }

    // Try each repository in order, Maven Central as fallback.
    let mut all_repos: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
    all_repos.push("https://repo1.maven.org/maven2");

    let url_path = format!("{}/{}", coord.path(), filename);

    for repo in &all_repos {
        let repo_url = repo.trim_end_matches('/');
        let url = format!("{repo_url}/{url_path}");

        match client.get(&url).send() {
            Ok(resp) if resp.status().is_success() => {
                let bytes = resp.bytes().with_context(|| format!("reading {url}"))?;
                if let Some(parent) = cached.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&cached, &bytes)
                    .with_context(|| format!("writing {}", cached.display()))?;
                return Ok(cached);
            }
            _ => continue,
        }
    }

    anyhow::bail!("could not download {} from any repository", coord)
}

// ─── POM parsing ────────────────────────────────────────────────────────────

struct PomDep {
    coord: MavenCoord,
    scope: String,
    optional: bool,
}

fn parse_pom_deps(pom_xml: &str) -> Vec<PomDep> {
    let mut deps = Vec::new();
    let doc = match roxmltree::Document::parse(pom_xml) {
        Ok(d) => d,
        Err(_) => return deps,
    };

    // Collect properties for ${property} substitution.
    let mut props = std::collections::HashMap::new();

    // Get project-level groupId and version for ${project.groupId} etc.
    for child in doc.root_element().children() {
        if child.is_element() {
            match child.tag_name().name() {
                "groupId" => {
                    if let Some(t) = child.text() {
                        props.insert("project.groupId".to_string(), t.to_string());
                    }
                }
                "version" => {
                    if let Some(t) = child.text() {
                        props.insert("project.version".to_string(), t.to_string());
                    }
                }
                "properties" => {
                    for prop in child.children().filter(|n| n.is_element()) {
                        if let Some(t) = prop.text() {
                            props.insert(prop.tag_name().name().to_string(), t.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let subst = |s: &str| -> String {
        let mut result = s.to_string();
        for (k, v) in &props {
            result = result.replace(&format!("${{{k}}}"), v);
        }
        result
    };

    // Find <dependencies> section (not inside <dependencyManagement>).
    for child in doc.root_element().children() {
        if child.is_element() && child.tag_name().name() == "dependencies" {
            for dep_node in child.children().filter(|n| n.is_element()) {
                if dep_node.tag_name().name() != "dependency" {
                    continue;
                }
                let mut group = String::new();
                let mut artifact = String::new();
                let mut version = String::new();
                let mut scope = "compile".to_string();
                let mut optional = false;

                for field in dep_node.children().filter(|n| n.is_element()) {
                    let text = field.text().unwrap_or("").trim().to_string();
                    match field.tag_name().name() {
                        "groupId" => group = subst(&text),
                        "artifactId" => artifact = subst(&text),
                        "version" => version = subst(&text),
                        "scope" => scope = text,
                        "optional" => optional = text == "true",
                        _ => {}
                    }
                }

                if !group.is_empty() && !artifact.is_empty() && !version.is_empty() {
                    deps.push(PomDep {
                        coord: MavenCoord {
                            group,
                            artifact,
                            version,
                        },
                        scope,
                        optional,
                    });
                }
            }
        }
    }

    deps
}

// ─── Resolver ───────────────────────────────────────────────────────────────

/// Resolve a set of Maven coordinates + repositories into downloaded JAR paths.
pub fn resolve(
    roots: &[MavenCoord],
    repos: &[String],
    show_progress: bool,
) -> Result<ResolvedDeps> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("creating HTTP client")?;

    let pb = if show_progress && !roots.is_empty() {
        let pb = indicatif::ProgressBar::new(roots.len() as u64);
        pb.set_style(
            indicatif::ProgressStyle::default_bar()
                .template("  resolving [{bar:30}] {pos}/{len} {msg}")
                .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar()),
        );
        Some(pb)
    } else {
        None
    };

    let mut queue: VecDeque<MavenCoord> = roots.iter().cloned().collect();
    let mut visited: HashSet<MavenCoord> = HashSet::new();
    let mut jars: Vec<PathBuf> = Vec::new();

    while let Some(coord) = queue.pop_front() {
        if !visited.insert(coord.clone()) {
            continue;
        }

        if let Some(ref pb) = pb {
            pb.set_message(format!("{}", coord));
            pb.inc(0); // update display
        }

        // Download JAR.
        let jar_path = download_artifact(&coord, &coord.jar_name(), repos, &client)
            .with_context(|| format!("downloading {}", coord))?;
        jars.push(jar_path);

        // Download POM for transitive deps.
        if let Ok(pom_path) = download_artifact(&coord, &coord.pom_name(), repos, &client) {
            if let Ok(pom_xml) = std::fs::read_to_string(&pom_path) {
                for dep in parse_pom_deps(&pom_xml) {
                    if dep.scope == "compile" && !dep.optional && !visited.contains(&dep.coord) {
                        queue.push_back(dep.coord);
                    }
                }
            }
        }

        if let Some(ref pb) = pb {
            pb.set_length((visited.len() + queue.len()) as u64);
            pb.set_position(visited.len() as u64);
        }
    }

    if let Some(pb) = pb {
        pb.finish_with_message("done");
    }

    Ok(ResolvedDeps { jars })
}

// ─── High-level API ─────────────────────────────────────────────────────────

/// Parse `@file:` annotations from a `.kts` script and resolve all dependencies.
/// Returns the resolved JARs and the cleaned source (annotations stripped).
pub fn resolve_script_deps(source: &str) -> Result<(ResolvedDeps, String)> {
    let (repos, deps, clean_source) = parse_script_annotations(source);

    if deps.is_empty() {
        return Ok((ResolvedDeps::default(), clean_source));
    }

    let show_progress = atty_is_terminal();
    let resolved = resolve(&deps, &repos, show_progress)?;
    Ok((resolved, clean_source))
}

fn atty_is_terminal() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stderr())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_annotations() {
        let src = r#"@file:Repository("https://example.com/maven")
@file:DependsOn("org.example:lib:1.0")
@file:DependsOn("com.foo:bar:2.3.4")
import org.example.lib.*
println("hello")
"#;
        let (repos, deps, clean) = parse_script_annotations(src);
        assert_eq!(repos, vec!["https://example.com/maven"]);
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].group, "org.example");
        assert_eq!(deps[0].artifact, "lib");
        assert_eq!(deps[0].version, "1.0");
        assert_eq!(deps[1].group, "com.foo");
        assert!(clean.contains("println"));
        assert!(!clean.contains("@file:"));
    }

    #[test]
    fn maven_coord_paths() {
        let c = MavenCoord::parse("org.jetbrains.kotlinx:kotlinx-html-jvm:0.7.3").unwrap();
        assert_eq!(c.path(), "org/jetbrains/kotlinx/kotlinx-html-jvm/0.7.3");
        assert_eq!(c.jar_name(), "kotlinx-html-jvm-0.7.3.jar");
        assert_eq!(c.pom_name(), "kotlinx-html-jvm-0.7.3.pom");
    }

    #[test]
    fn parse_simple_pom() {
        let pom = r#"<?xml version="1.0"?>
<project>
  <groupId>com.example</groupId>
  <version>1.0</version>
  <dependencies>
    <dependency>
      <groupId>org.dep</groupId>
      <artifactId>core</artifactId>
      <version>2.0</version>
    </dependency>
    <dependency>
      <groupId>org.test</groupId>
      <artifactId>junit</artifactId>
      <version>4.0</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>"#;
        let deps = parse_pom_deps(pom);
        // Only compile-scope deps (test excluded from the returned list,
        // but the PomDep struct preserves scope for filtering by caller).
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].coord.artifact, "core");
        assert_eq!(deps[0].scope, "compile");
        assert_eq!(deps[1].scope, "test");
    }

    #[test]
    fn pom_property_substitution() {
        let pom = r#"<?xml version="1.0"?>
<project>
  <groupId>com.example</groupId>
  <version>3.0</version>
  <properties>
    <dep.version>1.5</dep.version>
  </properties>
  <dependencies>
    <dependency>
      <groupId>${project.groupId}</groupId>
      <artifactId>sub</artifactId>
      <version>${dep.version}</version>
    </dependency>
  </dependencies>
</project>"#;
        let deps = parse_pom_deps(pom);
        assert_eq!(deps[0].coord.group, "com.example");
        assert_eq!(deps[0].coord.version, "1.5");
    }

    #[test]
    fn no_annotations_returns_clean_source() {
        let src = "println(42)\n";
        let (repos, deps, clean) = parse_script_annotations(src);
        assert!(repos.is_empty());
        assert!(deps.is_empty());
        assert_eq!(clean, "println(42)");
    }

    #[test]
    fn classpath_string_format() {
        let deps = ResolvedDeps {
            jars: vec![PathBuf::from("/a.jar"), PathBuf::from("/b.jar")],
        };
        let cp = deps.classpath_string();
        assert!(cp.contains("/a.jar"));
        assert!(cp.contains("/b.jar"));
    }
}
