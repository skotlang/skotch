//! Subset parser for `build.gradle.kts` and `settings.gradle.kts`.
//!
//! Reuses the skotch lexer to tokenize the file, then walks the token
//! stream looking for the small DSL patterns the build tool understands.
//! Anything unrecognised is silently skipped so real Gradle files can be
//! dropped in and progressively reconciled.
//!
//! This is a **token walker**, not a full parser, because Gradle DSL uses
//! receiver-lambda blocks (`android { ... }`) that the skotch parser
//! cannot handle yet. The walker only extracts configuration values.

pub mod version_catalog;

use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_lexer::LexedFile;
use skotch_span::{FileId, Span};
use skotch_syntax::TokenKind;

// ── Public types ────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildTarget {
    Jvm,
    Android,
    Native,
}

#[derive(Clone, Debug, Default)]
pub struct ProjectModel {
    /// Project name from `rootProject.name` in settings.gradle.kts,
    /// or derived from the project directory name. Used for JAR naming.
    pub project_name: Option<String>,
    pub group: Option<String>,
    pub version: Option<String>,
    pub target: Option<BuildTarget>,
    pub main_class: Option<String>,
    // Plugin flags — set by `plugins { }` block recognition.
    pub is_android: bool,
    pub is_kotlin: bool,
    pub is_compose: bool,
    // Android-specific
    pub application_id: Option<String>,
    pub namespace: Option<String>,
    pub min_sdk: Option<u32>,
    pub target_sdk: Option<u32>,
    pub compile_sdk: Option<u32>,
    pub version_code: Option<u32>,
    pub version_name: Option<String>,
    pub signing_config: Option<SigningConfig>,
    // Dependencies (for multi-module)
    pub project_deps: Vec<String>,
    /// External Maven dependencies: `implementation("org.example:lib:1.0")`.
    /// Each entry is a Maven coordinate string "group:artifact:version".
    pub external_deps: Vec<String>,
    /// Test dependencies: `testImplementation("org.junit.jupiter:...")`.
    pub test_deps: Vec<String>,
    /// Test framework: detected from `tasks.test { useJUnitPlatform() }`.
    pub test_framework: TestFramework,
    /// Custom test source directories from `sourceSets { test { ... } }`.
    pub test_source_dirs: Vec<String>,
    /// Custom main source directories from `sourceSets { main { ... } }`.
    pub source_dirs: Vec<String>,
    /// Maven repository URLs parsed from `repositories { ... }`.
    pub repositories: Vec<String>,
    /// Platform/BOM dependencies: `implementation(platform("group:artifact:version"))`.
    /// These provide version constraints for other versionless dependencies.
    pub platform_deps: Vec<String>,
    /// Whether this is a KMP project (kotlin("multiplatform") plugin detected).
    pub is_multiplatform: bool,
}

/// Which test framework the project uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TestFramework {
    /// No explicit test configuration found.
    #[default]
    None,
    /// `tasks.test { useJUnitPlatform() }` — JUnit 5 via Platform Launcher.
    JUnitPlatform,
    /// `tasks.test { useJUnit() }` — JUnit 4.
    JUnit4,
}

#[derive(Clone, Debug, Default)]
pub struct SigningConfig {
    pub store_file: Option<String>,
    pub store_password: Option<String>,
    pub key_alias: Option<String>,
    pub key_password: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SettingsModel {
    pub root_project_name: Option<String>,
    pub included_modules: Vec<String>,
}

/// Configuration extracted from `allprojects { }` or `subprojects { }` blocks.
/// Merged into child module `ProjectModel`s during multi-module builds.
#[derive(Clone, Debug, Default)]
pub struct SharedConfig {
    pub group: Option<String>,
    pub version: Option<String>,
    pub repositories: Vec<String>,
    pub external_deps: Vec<String>,
    pub test_deps: Vec<String>,
    pub is_kotlin: bool,
}

pub struct ParsedBuildFile {
    pub project: ProjectModel,
    /// Config from `allprojects { }` — applies to root + all children.
    pub allprojects_config: SharedConfig,
    /// Config from `subprojects { }` — applies to children only.
    pub subprojects_config: SharedConfig,
    pub diags: Diagnostics,
}

pub struct ParsedSettings {
    pub settings: SettingsModel,
    pub diags: Diagnostics,
}

// ── Entry points ────────────────────────────────────────────────────────

/// Parse a `build.gradle.kts` source string into a [`ProjectModel`].
pub fn parse_buildfile(src: &str, file: FileId, _interner: &mut Interner) -> ParsedBuildFile {
    parse_buildfile_with_catalog(src, file, _interner, None)
}

/// Parse a `build.gradle.kts` with optional version catalog resolution.
/// If `project_dir` is given, looks for `gradle/libs.versions.toml` and
/// resolves `libs.*` dependency references to Maven coordinates.
pub fn parse_buildfile_with_catalog(
    src: &str,
    file: FileId,
    _interner: &mut Interner,
    project_dir: Option<&std::path::Path>,
) -> ParsedBuildFile {
    let mut diags = Diagnostics::new();
    let lexed = skotch_lexer::lex(file, src, &mut diags);
    let mut walker = Walker::new(src, &lexed);
    walker.parse_build();
    let mut project = walker.project;

    // Try to load version catalog if project_dir is given.
    if let Some(dir) = project_dir {
        let catalog_path = dir.join("gradle/libs.versions.toml");
        if catalog_path.exists() {
            match version_catalog::parse_version_catalog(&catalog_path) {
                Ok(catalog) => resolve_catalog_deps(&mut project, &catalog),
                Err(e) => eprintln!("  WARNING: failed to parse version catalog: {e}"),
            }
        }
    }

    ParsedBuildFile {
        project,
        allprojects_config: walker.allprojects_config,
        subprojects_config: walker.subprojects_config,
        diags,
    }
}

/// Merge shared config from allprojects/subprojects into a child module's ProjectModel.
pub fn merge_shared_config(project: &mut ProjectModel, config: &SharedConfig) {
    if project.group.is_none() {
        project.group = config.group.clone();
    }
    if project.version.is_none() {
        project.version = config.version.clone();
    }
    if project.repositories.is_empty() {
        project.repositories = config.repositories.clone();
    }
    for dep in &config.external_deps {
        if !project.external_deps.contains(dep) {
            project.external_deps.push(dep.clone());
        }
    }
    for dep in &config.test_deps {
        if !project.test_deps.contains(dep) {
            project.test_deps.push(dep.clone());
        }
    }
    if config.is_kotlin && !project.is_kotlin {
        project.is_kotlin = true;
        project.target.get_or_insert(BuildTarget::Jvm);
    }
}

/// Resolve `libs.*` style dependency references using a version catalog.
fn resolve_catalog_deps(project: &mut ProjectModel, catalog: &version_catalog::VersionCatalog) {
    let resolve_list = |deps: &mut Vec<String>, catalog: &version_catalog::VersionCatalog| {
        let mut resolved = Vec::new();
        for dep in deps.drain(..) {
            if let Some(key) = dep.strip_prefix("libs.bundles.") {
                // Bundle: expand to constituent libraries.
                let bundle_key = key.replace('.', "-");
                if let Some(lib_keys) = catalog.bundles.get(&bundle_key) {
                    for lib_key in lib_keys {
                        if let Some(lib) = catalog.libraries.get(lib_key) {
                            resolved.push(lib.coordinate());
                        }
                    }
                }
            } else if let Some(key) = dep.strip_prefix("libs.") {
                // Direct library reference.
                let catalog_key = key.replace('.', "-");
                if let Some(lib) = catalog.libraries.get(&catalog_key) {
                    resolved.push(lib.coordinate());
                } else {
                    resolved.push(dep); // keep unresolved
                }
            } else {
                resolved.push(dep);
            }
        }
        *deps = resolved;
    };
    resolve_list(&mut project.external_deps, catalog);
    resolve_list(&mut project.test_deps, catalog);
    resolve_list(&mut project.platform_deps, catalog);

    // Resolve plugin flags from catalog — always check (don't gate on is_kotlin).
    for plugin in catalog.plugins.values() {
        match plugin.id.as_str() {
            "org.jetbrains.kotlin.jvm" => project.is_kotlin = true,
            "org.jetbrains.kotlin.android" => {
                project.is_kotlin = true;
                project.is_android = true;
                project.target = Some(BuildTarget::Android);
            }
            "org.jetbrains.kotlin.multiplatform" => {
                project.is_kotlin = true;
                project.is_multiplatform = true;
            }
            "com.android.application" | "com.android.library" => {
                project.is_android = true;
                project.is_kotlin = true;
                project.target = Some(BuildTarget::Android);
            }
            "org.jetbrains.kotlin.plugin.compose" | "org.jetbrains.compose" => {
                project.is_compose = true;
            }
            _ => {}
        }
    }
}

/// Parse a `settings.gradle.kts` source string into a [`SettingsModel`].
pub fn parse_settings(src: &str, file: FileId, interner: &mut Interner) -> ParsedSettings {
    let _ = interner; // reserved for future use
    let mut diags = Diagnostics::new();
    let lexed = skotch_lexer::lex(file, src, &mut diags);
    let mut walker = Walker::new(src, &lexed);
    walker.parse_settings_file();
    ParsedSettings {
        settings: walker.settings,
        diags,
    }
}

// ── Token walker ────────────────────────────────────────────────────────

struct Walker<'src, 'lf> {
    src: &'src str,
    lexed: &'lf LexedFile,
    pos: usize,
    project: ProjectModel,
    settings: SettingsModel,
    allprojects_config: SharedConfig,
    subprojects_config: SharedConfig,
    /// Local `val` assignments captured from dependencies blocks for
    /// string interpolation: `val ktor_version = "3.1.0"` → "ktor_version" → "3.1.0".
    dep_vals: std::collections::HashMap<String, String>,
}

impl<'src, 'lf> Walker<'src, 'lf> {
    fn new(src: &'src str, lexed: &'lf LexedFile) -> Self {
        Self {
            src,
            lexed,
            pos: 0,
            project: ProjectModel::default(),
            settings: SettingsModel::default(),
            allprojects_config: SharedConfig::default(),
            subprojects_config: SharedConfig::default(),
            dep_vals: std::collections::HashMap::new(),
        }
    }

    fn peek(&self) -> TokenKind {
        self.lexed.tokens[self.pos].kind
    }

    fn bump(&mut self) -> Span {
        let span = self.lexed.tokens[self.pos].span;
        self.pos += 1;
        span
    }

    fn at_end(&self) -> bool {
        self.peek() == TokenKind::Eof
    }

    fn skip_newlines(&mut self) {
        while self.peek() == TokenKind::Newline {
            self.pos += 1;
        }
    }

    fn text(&self, span: Span) -> &'src str {
        &self.src[span.start as usize..span.end as usize]
    }

    /// Check if the current token starts a string literal
    /// (`StringStart` or `StringLit`) and if so, consume the entire
    /// sequence and return the decoded string value.
    fn try_consume_string(&mut self) -> Option<String> {
        match self.peek() {
            TokenKind::StringLit => {
                let span = self.bump();
                Some(unquote_string_lit(self.text(span)))
            }
            TokenKind::StringStart => {
                self.bump(); // consume StringStart
                let mut value = String::new();
                loop {
                    match self.peek() {
                        TokenKind::StringChunk => {
                            let idx = self.pos;
                            self.bump();
                            // Get the decoded content from the payload.
                            if let Some(Some(skotch_lexer::TokenPayload::StringChunk(s))) =
                                self.lexed.payloads.get(idx)
                            {
                                value.push_str(s);
                            }
                        }
                        TokenKind::StringIdentRef => {
                            // $variable reference inside string template.
                            // Look up in dep_vals for simple substitution.
                            let idx = self.pos;
                            self.bump();
                            if let Some(Some(skotch_lexer::TokenPayload::StringIdentRef(
                                var_name,
                            ))) = self.lexed.payloads.get(idx)
                            {
                                if let Some(val) = self.dep_vals.get(var_name.as_str()) {
                                    value.push_str(val);
                                }
                            }
                        }
                        TokenKind::StringEnd => {
                            self.bump();
                            break;
                        }
                        TokenKind::Eof => break,
                        _ => {
                            self.bump();
                        }
                    }
                }
                Some(value)
            }
            _ => None,
        }
    }

    /// Check if we're at a string token.
    fn at_string(&self) -> bool {
        matches!(self.peek(), TokenKind::StringLit | TokenKind::StringStart)
    }

    // ── build.gradle.kts parsing ────────────────────────────────────

    fn parse_build(&mut self) {
        loop {
            self.skip_newlines();
            if self.at_end() {
                break;
            }
            if !self.parse_top_statement() {
                // Recovery: skip to next newline.
                while !self.at_end() && self.peek() != TokenKind::Newline {
                    self.bump();
                }
            }
        }
        if self.project.target.is_none() {
            self.project.target = Some(BuildTarget::Jvm);
        }
    }

    fn parse_top_statement(&mut self) -> bool {
        // Capture top-level `val xxx = "..."` for string interpolation.
        if self.peek() == TokenKind::KwVal {
            self.bump();
            if self.peek() == TokenKind::Ident {
                let name_span = self.bump();
                let var_name = self.text(name_span).to_string();
                // `val x = "..."` or `val x by extra("...")`
                if self.peek() == TokenKind::Eq {
                    self.bump();
                    if let Some(val) = self.try_consume_string() {
                        self.dep_vals.insert(var_name, val);
                    }
                } else if self.peek() == TokenKind::Ident {
                    // `val x by extra("...")`
                    let kw = self.bump();
                    if self.text(kw) == "by" {
                        // skip `extra(` or similar
                        if self.peek() == TokenKind::Ident {
                            self.bump(); // e.g. "extra"
                        }
                        if self.peek() == TokenKind::LParen {
                            self.bump();
                            if let Some(val) = self.try_consume_string() {
                                self.dep_vals.insert(var_name, val);
                            }
                            self.skip_past(TokenKind::RParen);
                        }
                    }
                }
            }
            self.skip_to_newline();
            return true;
        }
        if self.peek() != TokenKind::Ident {
            return false;
        }
        let span = self.bump();
        let ident = self.text(span);
        match ident {
            "plugins" => self.parse_plugins_block(),
            "group" => self.parse_string_assign("group"),
            "version" => self.parse_string_assign("version"),
            "repositories" => self.parse_repositories_block(),
            "dependencies" => self.parse_dependencies_block(),
            "application" => self.parse_application_block(),
            "android" => self.parse_android_block(),
            "tasks" => self.parse_tasks_block(),
            "sourceSets" => self.parse_source_sets_block(),
            "allprojects" => self.parse_shared_config_block(true),
            "subprojects" => self.parse_shared_config_block(false),
            "kotlin" | "java" | "buildscript" | "testing" => self.skip_brace_block(),
            _ => {
                self.skip_to_newline();
                true
            }
        }
    }

    fn parse_plugins_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return false;
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let name = self.text(span);
                match name {
                    "kotlin" if self.peek() == TokenKind::LParen => {
                        self.bump();
                        if let Some(val) = self.try_consume_string() {
                            self.project.is_kotlin = true;
                            match val.as_str() {
                                "jvm" => {
                                    self.project.target.get_or_insert(BuildTarget::Jvm);
                                }
                                "multiplatform" => {
                                    self.project.target.get_or_insert(BuildTarget::Jvm);
                                    self.project.is_multiplatform = true;
                                }
                                "android" => {
                                    self.project.is_android = true;
                                    self.project.target = Some(BuildTarget::Android);
                                }
                                _ => {}
                            }
                        }
                        self.skip_past(TokenKind::RParen);
                    }
                    "id" if self.peek() == TokenKind::LParen => {
                        self.bump();
                        if let Some(val) = self.try_consume_string() {
                            self.apply_plugin_id(&val);
                        }
                        self.skip_past(TokenKind::RParen);
                    }
                    "alias" if self.peek() == TokenKind::LParen => {
                        // alias(libs.plugins.kotlin.jvm) — version catalog plugin ref.
                        // The actual resolution happens post-parse via version catalog.
                        // For now, just skip the parens.
                        self.skip_past(TokenKind::RParen);
                    }
                    "`java-library`" | "java-library" => {
                        // java-library plugin — common in library projects
                    }
                    "application" => { /* marker plugin; nothing to record */ }
                    _ => {}
                }
                // Optional `version "..."` trailer on the same line.
                if self.peek() == TokenKind::Ident {
                    let saved = self.pos;
                    let next = self.bump();
                    if self.text(next) == "version" && self.at_string() {
                        self.try_consume_string();
                    } else {
                        self.pos = saved;
                    }
                }
            } else {
                self.bump();
            }
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    fn parse_application_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return false;
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                // mainClass.set("...")
                if self.text(span) == "mainClass" && self.peek() == TokenKind::Dot {
                    self.bump();
                    if self.peek() == TokenKind::Ident {
                        let setter = self.bump();
                        if self.text(setter) == "set" && self.peek() == TokenKind::LParen {
                            self.bump();
                            if let Some(val) = self.try_consume_string() {
                                self.project.main_class = Some(val);
                            }
                            self.skip_past(TokenKind::RParen);
                        }
                    }
                }
            } else {
                self.bump();
            }
            self.skip_to_newline();
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    fn parse_android_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return false;
        }
        self.bump();
        self.project.target.get_or_insert(BuildTarget::Android);
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let name = self.text(span).to_string();
                match name.as_str() {
                    "namespace" => {
                        self.parse_string_assign_into(&name);
                    }
                    "compileSdk" => {
                        self.parse_int_assign_into("compileSdk");
                    }
                    "defaultConfig" => {
                        self.parse_default_config_block();
                    }
                    "signingConfigs" => {
                        self.parse_signing_configs_block();
                    }
                    _ => {
                        // Skip unknown: buildTypes, buildFeatures, etc.
                        if self.peek() == TokenKind::LBrace {
                            self.skip_brace_block();
                        } else {
                            self.skip_to_newline();
                        }
                    }
                };
            } else {
                self.bump();
            }
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    fn parse_default_config_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return false;
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let name = self.text(span).to_string();
                match name.as_str() {
                    "applicationId" | "namespace" | "versionName" => {
                        self.parse_string_assign_into(&name);
                    }
                    "minSdk" | "targetSdk" | "versionCode" => {
                        self.parse_int_assign_into(&name);
                    }
                    _ => {
                        self.skip_to_newline();
                    }
                };
            } else {
                self.bump();
            }
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    fn parse_signing_configs_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return false;
        }
        self.bump();
        // We expect: configName { storeFile = ...; storePassword = ...; ... }
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let _config_name = self.bump();
                // Parse the inner block.
                if self.peek() == TokenKind::LBrace {
                    self.bump();
                    let mut sc = SigningConfig::default();
                    loop {
                        self.skip_newlines();
                        if self.peek() == TokenKind::RBrace || self.at_end() {
                            break;
                        }
                        if self.peek() == TokenKind::Ident {
                            let span = self.bump();
                            let key = self.text(span).to_string();
                            if self.peek() == TokenKind::Eq {
                                self.bump();
                                if let Some(val) = self.try_consume_string() {
                                    match key.as_str() {
                                        "storeFile" => sc.store_file = Some(val),
                                        "storePassword" => sc.store_password = Some(val),
                                        "keyAlias" => sc.key_alias = Some(val),
                                        "keyPassword" => sc.key_password = Some(val),
                                        _ => {}
                                    }
                                }
                            }
                        } else {
                            self.bump();
                        }
                        self.skip_to_newline();
                    }
                    if self.peek() == TokenKind::RBrace {
                        self.bump();
                    }
                    self.project.signing_config = Some(sc);
                }
            } else {
                self.bump();
            }
            self.skip_to_newline();
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    /// Parse `tasks { test { useJUnitPlatform() } }` or `tasks.test { useJUnitPlatform() }`.
    fn parse_tasks_block(&mut self) -> bool {
        // Handle `tasks.test { ... }` (dot access).
        if self.peek() == TokenKind::Dot {
            self.bump();
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let task_name = self.text(span).to_string();
                if task_name == "test" {
                    return self.parse_test_task_block();
                }
            }
            return self.skip_brace_block();
        }
        // Handle `tasks { test { ... } }` (nested block).
        if self.peek() != TokenKind::LBrace {
            return self.skip_to_newline_ret();
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let name = self.text(span).to_string();
                if name == "test" {
                    self.parse_test_task_block();
                } else {
                    self.skip_brace_block();
                }
            } else {
                self.bump();
            }
            self.skip_to_newline();
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    /// Parse the body of `test { useJUnitPlatform() }`.
    fn parse_test_task_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return self.skip_to_newline_ret();
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let name = self.text(span).to_string();
                match name.as_str() {
                    "useJUnitPlatform" => {
                        self.project.test_framework = TestFramework::JUnitPlatform;
                        // Consume optional `()`.
                        if self.peek() == TokenKind::LParen {
                            self.skip_past(TokenKind::RParen);
                        }
                    }
                    "useJUnit" => {
                        self.project.test_framework = TestFramework::JUnit4;
                        if self.peek() == TokenKind::LParen {
                            self.skip_past(TokenKind::RParen);
                        }
                    }
                    _ => {}
                }
            } else {
                self.bump();
            }
            self.skip_to_newline();
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    /// Parse `sourceSets { test { kotlin.srcDir("path") } }`.
    fn parse_source_sets_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return self.skip_to_newline_ret();
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let set_name = self.text(span).to_string();
                if (set_name == "test" || set_name == "main") && self.peek() == TokenKind::LBrace {
                    let is_test = set_name == "test";
                    self.bump();
                    // Inside test/main { ... }, look for kotlin.srcDir("path")
                    // or kotlin.srcDirs("path1", "path2").
                    loop {
                        self.skip_newlines();
                        if self.peek() == TokenKind::RBrace || self.at_end() {
                            break;
                        }
                        if self.peek() == TokenKind::Ident {
                            let inner = self.bump();
                            if self.text(inner) == "kotlin" && self.peek() == TokenKind::Dot {
                                self.bump(); // consume '.'
                                if self.peek() == TokenKind::Ident {
                                    let method = self.bump();
                                    let method_name = self.text(method).to_string();
                                    if (method_name == "srcDir" || method_name == "srcDirs")
                                        && self.peek() == TokenKind::LParen
                                    {
                                        self.bump();
                                        // Consume all string args (srcDirs can have multiple).
                                        loop {
                                            if let Some(dir) = self.try_consume_string() {
                                                let target = if is_test {
                                                    &mut self.project.test_source_dirs
                                                } else {
                                                    &mut self.project.source_dirs
                                                };
                                                target.push(dir);
                                            }
                                            if self.peek() == TokenKind::Comma {
                                                self.bump();
                                            } else {
                                                break;
                                            }
                                        }
                                        self.skip_past(TokenKind::RParen);
                                    }
                                }
                            }
                        } else {
                            self.bump();
                        }
                        self.skip_to_newline();
                    }
                    if self.peek() == TokenKind::RBrace {
                        self.bump();
                    }
                } else {
                    self.skip_brace_block();
                }
            } else {
                self.bump();
            }
            self.skip_to_newline();
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    fn skip_to_newline_ret(&mut self) -> bool {
        self.skip_to_newline();
        true
    }

    /// Parse `allprojects { }` or `subprojects { }` to extract shared config.
    fn parse_shared_config_block(&mut self, is_allprojects: bool) -> bool {
        if self.peek() != TokenKind::LBrace {
            return self.skip_to_newline_ret();
        }
        self.bump();
        // Temporarily swap project to capture config into a temp model.
        let saved = std::mem::take(&mut self.project);
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let ident = self.text(span);
                match ident {
                    "group" => {
                        self.parse_string_assign("group");
                    }
                    "version" => {
                        self.parse_string_assign("version");
                    }
                    "repositories" => {
                        self.parse_repositories_block();
                    }
                    "dependencies" => {
                        self.parse_dependencies_block();
                    }
                    "apply" => {
                        // apply(plugin = "org.jetbrains.kotlin.jvm")
                        if self.peek() == TokenKind::LParen {
                            self.bump();
                            // Look for plugin = "..."
                            loop {
                                self.skip_newlines();
                                if self.peek() == TokenKind::RParen || self.at_end() {
                                    break;
                                }
                                if self.peek() == TokenKind::Ident {
                                    let inner = self.bump();
                                    if self.text(inner) == "plugin" {
                                        if self.peek() == TokenKind::Eq {
                                            self.bump();
                                        }
                                        if let Some(plugin_id) = self.try_consume_string() {
                                            self.apply_plugin_id(&plugin_id);
                                        }
                                    }
                                } else {
                                    self.bump();
                                }
                            }
                            if self.peek() == TokenKind::RParen {
                                self.bump();
                            }
                        }
                    }
                    _ => {
                        if self.peek() == TokenKind::LBrace {
                            self.skip_brace_block();
                        }
                    }
                }
            } else {
                self.bump();
            }
            self.skip_to_newline();
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        // Extract the captured config and restore original project.
        let captured = std::mem::replace(&mut self.project, saved);
        let config = SharedConfig {
            group: captured.group,
            version: captured.version,
            repositories: captured.repositories,
            external_deps: captured.external_deps,
            test_deps: captured.test_deps,
            is_kotlin: captured.is_kotlin,
        };
        if is_allprojects {
            self.allprojects_config = config;
        } else {
            self.subprojects_config = config;
        }
        true
    }

    /// Parse `repositories { mavenCentral(); google(); maven("url") }`.
    fn parse_repositories_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return self.skip_to_newline_ret();
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let name = self.text(span).to_string();
                match name.as_str() {
                    "mavenCentral" => {
                        // mavenCentral() — skip parens
                        if self.peek() == TokenKind::LParen {
                            self.skip_past(TokenKind::RParen);
                        }
                        self.project
                            .repositories
                            .push("https://repo1.maven.org/maven2".to_string());
                    }
                    "google" => {
                        if self.peek() == TokenKind::LParen {
                            self.skip_past(TokenKind::RParen);
                        }
                        self.project
                            .repositories
                            .push("https://dl.google.com/dl/android/maven2".to_string());
                    }
                    "maven" => {
                        // maven("url") or maven { url = "..." } or maven { url = uri("...") }
                        if self.peek() == TokenKind::LParen {
                            self.bump();
                            if let Some(url) = self.try_consume_string() {
                                self.project.repositories.push(url);
                            }
                            self.skip_past(TokenKind::RParen);
                        } else if self.peek() == TokenKind::LBrace {
                            self.bump();
                            // Look for url = "..." or url = uri("...")
                            loop {
                                self.skip_newlines();
                                if self.peek() == TokenKind::RBrace || self.at_end() {
                                    break;
                                }
                                if self.peek() == TokenKind::Ident {
                                    let inner = self.bump();
                                    if self.text(inner) == "url" {
                                        // skip '=' or `.set(`
                                        if self.peek() == TokenKind::Eq {
                                            self.bump();
                                        }
                                        // url = "https://..." or url = uri("https://...")
                                        if let Some(url) = self.try_consume_string() {
                                            self.project.repositories.push(url);
                                        } else if self.peek() == TokenKind::Ident {
                                            let fn_span = self.bump();
                                            if self.text(fn_span) == "uri"
                                                && self.peek() == TokenKind::LParen
                                            {
                                                self.bump();
                                                if let Some(url) = self.try_consume_string() {
                                                    self.project.repositories.push(url);
                                                }
                                                self.skip_past(TokenKind::RParen);
                                            }
                                        }
                                    }
                                }
                                self.skip_to_newline();
                            }
                            if self.peek() == TokenKind::RBrace {
                                self.bump();
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                self.bump();
            }
            self.skip_to_newline();
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    fn parse_dependencies_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return false;
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.peek() == TokenKind::RBrace || self.at_end() {
                break;
            }
            // Capture `val xxx = "..."` local variable assignments for
            // string interpolation in dependency coordinates.
            // Also detect `val xxx = platform(libs.yyy)` → register as platform dep.
            if self.peek() == TokenKind::KwVal {
                self.bump();
                if self.peek() == TokenKind::Ident {
                    let name_span = self.bump();
                    let var_name = self.text(name_span).to_string();
                    if self.peek() == TokenKind::Eq {
                        self.bump();
                        // Check for platform(libs.xxx) or platform("g:a:v")
                        if self.peek() == TokenKind::Ident {
                            let fn_span = self.bump();
                            let fn_name = self.text(fn_span).to_string();
                            if fn_name == "platform" && self.peek() == TokenKind::LParen {
                                self.bump();
                                if self.peek() == TokenKind::Ident {
                                    let inner_span = self.bump();
                                    let inner = self.text(inner_span).to_string();
                                    if inner == "libs" {
                                        // libs.xxx.yyy
                                        let mut path = String::from("libs");
                                        while self.peek() == TokenKind::Dot {
                                            self.bump();
                                            if self.peek() == TokenKind::Ident {
                                                let seg = self.bump();
                                                path.push('.');
                                                path.push_str(self.text(seg));
                                            }
                                        }
                                        // Also store var for `implementation(composeBom)`.
                                        self.dep_vals.insert(var_name.clone(), path.clone());
                                        self.project.platform_deps.push(path);
                                    }
                                } else if let Some(coord) = self.try_consume_string() {
                                    if coord.contains(':') {
                                        self.project.platform_deps.push(coord.clone());
                                        self.dep_vals.insert(var_name.clone(), coord);
                                    }
                                }
                                self.skip_past(TokenKind::RParen);
                            }
                        } else if let Some(val) = self.try_consume_string() {
                            self.dep_vals.insert(var_name, val);
                        }
                    }
                }
                self.skip_to_newline();
                continue;
            }
            // Look for dependency declarations:
            //   implementation(project(":lib")) or implementation("g:a:v")
            //   testImplementation("org.junit.jupiter:...")
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let name = self.text(span).to_string();
                let is_main_dep = matches!(
                    name.as_str(),
                    "implementation"
                        | "api"
                        | "compileOnly"
                        | "runtimeOnly"
                        | "debugImplementation"
                        | "releaseImplementation"
                );
                let is_test_dep = matches!(
                    name.as_str(),
                    "testImplementation"
                        | "testRuntimeOnly"
                        | "testCompileOnly"
                        | "androidTestImplementation"
                        | "ksp"
                        | "kspTest"
                );
                if (is_main_dep || is_test_dep) && self.peek() == TokenKind::LParen {
                    self.bump();
                    if self.peek() == TokenKind::Ident {
                        let inner = self.bump();
                        let inner_text = self.text(inner).to_string();
                        if inner_text == "project" && self.peek() == TokenKind::LParen {
                            self.bump();
                            if let Some(dep) = self.try_consume_string() {
                                self.project.project_deps.push(dep);
                            }
                            self.skip_past(TokenKind::RParen);
                        } else if inner_text == "projects" {
                            // Typesafe project accessor: projects.lib → project(":lib")
                            if self.peek() == TokenKind::Dot {
                                self.bump();
                                if self.peek() == TokenKind::Ident {
                                    let mod_span = self.bump();
                                    let mod_name = self.text(mod_span).to_string();
                                    self.project.project_deps.push(format!(":{mod_name}"));
                                }
                            }
                        } else if inner_text == "libs" {
                            // Version catalog reference: libs.commons.math3
                            let mut path = String::from("libs");
                            while self.peek() == TokenKind::Dot {
                                self.bump();
                                if self.peek() == TokenKind::Ident {
                                    let seg = self.bump();
                                    path.push('.');
                                    path.push_str(self.text(seg));
                                }
                            }
                            // Store as "libs.xxx" — resolved post-parse via catalog.
                            let target = if is_test_dep {
                                &mut self.project.test_deps
                            } else {
                                &mut self.project.external_deps
                            };
                            target.push(path);
                        } else if inner_text == "platform" && self.peek() == TokenKind::LParen {
                            // platform("group:artifact:version") — BOM dependency
                            self.bump();
                            if let Some(coord) = self.try_consume_string() {
                                if coord.contains(':') {
                                    self.project.platform_deps.push(coord);
                                }
                            }
                            self.skip_past(TokenKind::RParen);
                        } else if inner_text == "kotlin" && self.peek() == TokenKind::LParen {
                            // kotlin("stdlib") → org.jetbrains.kotlin:kotlin-stdlib
                            self.bump();
                            if let Some(module) = self.try_consume_string() {
                                let coord = format!("org.jetbrains.kotlin:kotlin-{module}:1.9.22");
                                if is_test_dep {
                                    self.project.test_deps.push(coord);
                                } else {
                                    self.project.external_deps.push(coord);
                                }
                            }
                            self.skip_past(TokenKind::RParen);
                        }
                    } else if let Some(coord) = self.try_consume_string() {
                        if coord.contains(':') {
                            if is_test_dep {
                                self.project.test_deps.push(coord);
                            } else {
                                self.project.external_deps.push(coord);
                            }
                        }
                    }
                    self.skip_past(TokenKind::RParen);
                }
            } else {
                self.bump();
            }
            self.skip_to_newline();
        }
        if self.peek() == TokenKind::RBrace {
            self.bump();
        }
        true
    }

    // ── settings.gradle.kts parsing ─────────────────────────────────

    fn parse_settings_file(&mut self) {
        loop {
            self.skip_newlines();
            if self.at_end() {
                break;
            }
            if self.peek() != TokenKind::Ident {
                self.bump();
                continue;
            }
            let span = self.bump();
            let ident = self.text(span);
            match ident {
                // rootProject.name = "..."
                "rootProject" => {
                    if self.peek() == TokenKind::Dot {
                        self.bump();
                        if self.peek() == TokenKind::Ident {
                            let prop = self.bump();
                            if self.text(prop) == "name" && self.peek() == TokenKind::Eq {
                                self.bump();
                                if let Some(val) = self.try_consume_string() {
                                    self.settings.root_project_name = Some(val);
                                }
                            }
                        }
                    }
                }
                // include(":app", ":lib")
                "include" => {
                    if self.peek() == TokenKind::LParen {
                        self.bump();
                        loop {
                            self.skip_newlines();
                            if self.peek() == TokenKind::RParen || self.at_end() {
                                break;
                            }
                            if let Some(val) = self.try_consume_string() {
                                self.settings.included_modules.push(val);
                            } else {
                                self.bump();
                            }
                        }
                        if self.peek() == TokenKind::RParen {
                            self.bump();
                        }
                    }
                }
                // pluginManagement { ... }, dependencyResolutionManagement { ... }, etc.
                _ => {
                    if self.peek() == TokenKind::LBrace {
                        self.skip_brace_block();
                    } else {
                        self.skip_to_newline();
                    }
                }
            }
        }
    }

    // ── Plugin recognition ──────────────────────────────────────────

    /// Map a `plugins { id("...") }` string to project flags and target.
    fn apply_plugin_id(&mut self, plugin_id: &str) {
        match plugin_id {
            "com.android.application" | "com.android.library" => {
                self.project.is_android = true;
                self.project.target = Some(BuildTarget::Android);
            }
            "org.jetbrains.kotlin.android" => {
                self.project.is_kotlin = true;
                self.project.is_android = true;
                self.project.target = Some(BuildTarget::Android);
            }
            "org.jetbrains.kotlin.jvm" | "org.jetbrains.kotlin.multiplatform" => {
                self.project.is_kotlin = true;
                self.project.target.get_or_insert(BuildTarget::Jvm);
            }
            "org.jetbrains.compose" | "org.jetbrains.compose.desktop" => {
                self.project.is_compose = true;
            }
            _ => {}
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────

    fn parse_string_assign(&mut self, key: &str) -> bool {
        if self.peek() != TokenKind::Eq {
            return false;
        }
        self.bump();
        let val = match self.try_consume_string() {
            Some(v) => v,
            None => return false,
        };
        match key {
            "group" => self.project.group = Some(val),
            "version" => self.project.version = Some(val),
            _ => {}
        }
        self.skip_to_newline();
        true
    }

    fn parse_string_assign_into(&mut self, key: &str) -> bool {
        if self.peek() != TokenKind::Eq {
            return false;
        }
        self.bump();
        let val = match self.try_consume_string() {
            Some(v) => v,
            None => return false,
        };
        match key {
            "namespace" => self.project.namespace = Some(val),
            "applicationId" => self.project.application_id = Some(val),
            "versionName" => self.project.version_name = Some(val),
            _ => {}
        }
        self.skip_to_newline();
        true
    }

    fn parse_int_assign_into(&mut self, key: &str) -> bool {
        if self.peek() != TokenKind::Eq {
            return false;
        }
        self.bump();
        if self.peek() != TokenKind::IntLit {
            return false;
        }
        let s = self.bump();
        let val: u32 = self.text(s).parse().unwrap_or(0);
        match key {
            "compileSdk" => self.project.compile_sdk = Some(val),
            "minSdk" => self.project.min_sdk = Some(val),
            "targetSdk" => self.project.target_sdk = Some(val),
            "versionCode" => self.project.version_code = Some(val),
            _ => {}
        }
        self.skip_to_newline();
        true
    }

    fn skip_brace_block(&mut self) -> bool {
        if self.peek() != TokenKind::LBrace {
            return false;
        }
        let mut depth = 0i32;
        loop {
            match self.peek() {
                TokenKind::LBrace => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::RBrace => {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                }
                TokenKind::Eof => break,
                _ => {
                    self.bump();
                }
            }
        }
        true
    }

    fn skip_to_newline(&mut self) {
        while !self.at_end() && self.peek() != TokenKind::Newline {
            self.bump();
        }
    }

    fn skip_past(&mut self, kind: TokenKind) {
        while !self.at_end() && self.peek() != kind {
            self.bump();
        }
        if self.peek() == kind {
            self.bump();
        }
    }
}

// ── Utilities ───────────────────────────────────────────────────────────

/// Strip surrounding double quotes and process basic escape sequences.
fn unquote_string_lit(raw: &str) -> String {
    let inner = if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('$') => out.push('$'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jvm_buildfile() {
        let src = r#"
            plugins {
                kotlin("jvm")
                application
            }

            group = "com.example"
            version = "1.0.0"

            application {
                mainClass.set("HelloKt")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.target, Some(BuildTarget::Jvm));
        assert_eq!(parsed.project.group.as_deref(), Some("com.example"));
        assert_eq!(parsed.project.version.as_deref(), Some("1.0.0"));
        assert_eq!(parsed.project.main_class.as_deref(), Some("HelloKt"));
    }

    #[test]
    fn parse_android_buildfile() {
        let src = r#"
            plugins {
                id("com.android.application")
                kotlin("android")
            }

            android {
                namespace = "com.example.hello"
                compileSdk = 34
                defaultConfig {
                    applicationId = "com.example.hello"
                    minSdk = 24
                    targetSdk = 34
                    versionCode = 1
                    versionName = "1.0"
                }
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.target, Some(BuildTarget::Android));
        assert_eq!(
            parsed.project.namespace.as_deref(),
            Some("com.example.hello")
        );
        assert_eq!(
            parsed.project.application_id.as_deref(),
            Some("com.example.hello")
        );
        assert_eq!(parsed.project.min_sdk, Some(24));
        assert_eq!(parsed.project.target_sdk, Some(34));
        assert_eq!(parsed.project.compile_sdk, Some(34));
        assert_eq!(parsed.project.version_code, Some(1));
        assert_eq!(parsed.project.version_name.as_deref(), Some("1.0"));
    }

    #[test]
    fn parse_settings_basic() {
        let src = r#"
            rootProject.name = "myapp"
            include(":app", ":lib")
        "#;
        let mut interner = Interner::new();
        let parsed = parse_settings(src, FileId(0), &mut interner);
        assert_eq!(parsed.settings.root_project_name.as_deref(), Some("myapp"));
        assert_eq!(parsed.settings.included_modules, vec![":app", ":lib"]);
    }

    #[test]
    fn parse_project_deps() {
        let src = r#"
            plugins {
                kotlin("jvm")
            }
            dependencies {
                implementation(project(":lib"))
                implementation("org.jetbrains.kotlin:kotlin-stdlib:2.0.0")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.project_deps, vec![":lib"]);
        assert_eq!(
            parsed.project.external_deps,
            vec!["org.jetbrains.kotlin:kotlin-stdlib:2.0.0"]
        );
    }

    #[test]
    fn parse_signing_config() {
        let src = r#"
            plugins {
                id("com.android.application")
            }
            android {
                signingConfigs {
                    debug {
                        storeFile = "debug.keystore"
                        storePassword = "android"
                        keyAlias = "androiddebugkey"
                        keyPassword = "android"
                    }
                }
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        let sc = parsed.project.signing_config.as_ref().unwrap();
        assert_eq!(sc.store_file.as_deref(), Some("debug.keystore"));
        assert_eq!(sc.store_password.as_deref(), Some("android"));
        assert_eq!(sc.key_alias.as_deref(), Some("androiddebugkey"));
        assert_eq!(sc.key_password.as_deref(), Some("android"));
    }

    #[test]
    fn defaults_to_jvm() {
        let src = r#"
            group = "com.example"
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.target, Some(BuildTarget::Jvm));
    }

    #[test]
    fn plugin_flags_android_app() {
        let src = r#"
            plugins {
                id("com.android.application")
                id("org.jetbrains.kotlin.android")
                id("org.jetbrains.compose")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed.project.is_android);
        assert!(parsed.project.is_kotlin);
        assert!(parsed.project.is_compose);
        assert_eq!(parsed.project.target, Some(BuildTarget::Android));
    }

    #[test]
    fn plugin_flags_kotlin_shorthand() {
        let src = r#"
            plugins {
                kotlin("android")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed.project.is_android);
        assert!(parsed.project.is_kotlin);
        assert_eq!(parsed.project.target, Some(BuildTarget::Android));
    }

    #[test]
    fn plugin_flags_jvm_only() {
        let src = r#"
            plugins {
                id("org.jetbrains.kotlin.jvm")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(!parsed.project.is_android);
        assert!(parsed.project.is_kotlin);
        assert!(!parsed.project.is_compose);
        assert_eq!(parsed.project.target, Some(BuildTarget::Jvm));
    }

    #[test]
    fn plugin_flags_kotlin_jvm_shorthand() {
        let src = r#"
            plugins {
                kotlin("jvm")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(!parsed.project.is_android);
        assert!(parsed.project.is_kotlin);
        assert_eq!(parsed.project.target, Some(BuildTarget::Jvm));
    }

    #[test]
    fn plugin_flags_compose_desktop() {
        let src = r#"
            plugins {
                id("org.jetbrains.compose.desktop")
                id("org.jetbrains.kotlin.jvm")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed.project.is_compose);
        assert!(parsed.project.is_kotlin);
        assert!(!parsed.project.is_android);
    }

    #[test]
    fn plugin_flags_default_false() {
        let src = r#"
            group = "com.example"
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(!parsed.project.is_android);
        assert!(!parsed.project.is_kotlin);
        assert!(!parsed.project.is_compose);
    }

    #[test]
    fn parse_test_deps() {
        let src = r#"
            plugins { kotlin("jvm") }
            dependencies {
                implementation("org.example:lib:1.0")
                testImplementation("org.junit.jupiter:junit-jupiter:5.10.0")
                testRuntimeOnly("org.junit.platform:junit-platform-launcher")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.external_deps, vec!["org.example:lib:1.0"]);
        assert_eq!(
            parsed.project.test_deps,
            vec![
                "org.junit.jupiter:junit-jupiter:5.10.0",
                "org.junit.platform:junit-platform-launcher",
            ]
        );
    }

    #[test]
    fn parse_use_junit_platform() {
        let src = r#"
            plugins { kotlin("jvm") }
            tasks.test {
                useJUnitPlatform()
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.test_framework, TestFramework::JUnitPlatform);
    }

    #[test]
    fn parse_use_junit_platform_nested() {
        let src = r#"
            plugins { kotlin("jvm") }
            tasks {
                test {
                    useJUnitPlatform()
                }
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.test_framework, TestFramework::JUnitPlatform);
    }

    #[test]
    fn parse_test_source_dirs() {
        let src = r#"
            plugins { kotlin("jvm") }
            sourceSets {
                test {
                    kotlin.srcDir("src/integrationTest/kotlin")
                }
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(
            parsed.project.test_source_dirs,
            vec!["src/integrationTest/kotlin"]
        );
    }

    #[test]
    fn parse_test_framework_default() {
        let src = r#"
            plugins { kotlin("jvm") }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.test_framework, TestFramework::None);
    }

    #[test]
    fn parse_repositories_maven_central_and_google() {
        let src = r#"
            plugins { kotlin("jvm") }
            repositories {
                mavenCentral()
                google()
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.repositories.len(), 2);
        assert!(parsed.project.repositories[0].contains("repo1.maven.org"));
        assert!(parsed.project.repositories[1].contains("google.com"));
    }

    #[test]
    fn parse_repositories_maven_url_brace() {
        let src = r#"
            repositories {
                maven { url = "https://jitpack.io" }
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed
            .project
            .repositories
            .contains(&"https://jitpack.io".to_string()));
    }

    #[test]
    fn parse_repositories_maven_url_paren() {
        let src = r#"
            repositories {
                maven("https://custom.repo.com/maven")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed
            .project
            .repositories
            .contains(&"https://custom.repo.com/maven".to_string()));
    }

    #[test]
    fn parse_main_source_dirs() {
        let src = r#"
            plugins { kotlin("jvm") }
            sourceSets {
                main {
                    kotlin.srcDirs("src", "gen")
                }
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.source_dirs, vec!["src", "gen"]);
    }

    #[test]
    fn parse_main_source_dir_single() {
        let src = r#"
            sourceSets {
                main {
                    kotlin.srcDir("app/src")
                }
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.source_dirs, vec!["app/src"]);
    }

    #[test]
    fn parse_version_catalog_deps() {
        let src = r#"
            plugins { kotlin("jvm") }
            dependencies {
                implementation(libs.commons.math3)
                testImplementation(libs.junit)
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.external_deps, vec!["libs.commons.math3"]);
        assert_eq!(parsed.project.test_deps, vec!["libs.junit"]);
    }

    #[test]
    fn resolve_version_catalog_integration() {
        let catalog_src = r#"
[versions]
math = "3.6.1"

[libraries]
commons-math3 = "org.apache.commons:commons-math3:3.6.1"

[plugins]
kotlin-jvm = { id = "org.jetbrains.kotlin.jvm", version = "1.9.22" }
"#;
        let catalog = crate::version_catalog::parse_version_catalog_str(catalog_src).unwrap();
        let mut project = ProjectModel {
            external_deps: vec!["libs.commons.math3".to_string()],
            ..Default::default()
        };
        resolve_catalog_deps(&mut project, &catalog);
        assert_eq!(
            project.external_deps,
            vec!["org.apache.commons:commons-math3:3.6.1"]
        );
        // Plugin flags should be set from catalog.
        assert!(project.is_kotlin);
    }

    #[test]
    fn parse_alias_in_plugins() {
        let src = r#"
            plugins {
                alias(libs.plugins.kotlin.jvm)
            }
        "#;
        let mut interner = Interner::new();
        // Without catalog, alias is silently accepted (no crash).
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.target, Some(BuildTarget::Jvm));
    }

    #[test]
    fn parse_allprojects_subprojects() {
        let src = r#"
            allprojects {
                group = "org.example"
                version = "2.0.0"
                repositories {
                    mavenCentral()
                }
            }
            subprojects {
                apply(plugin = "org.jetbrains.kotlin.jvm")
                dependencies {
                    testImplementation("org.junit.jupiter:junit-jupiter:5.11.4")
                }
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(
            parsed.allprojects_config.group.as_deref(),
            Some("org.example")
        );
        assert_eq!(parsed.allprojects_config.version.as_deref(), Some("2.0.0"));
        assert!(!parsed.allprojects_config.repositories.is_empty());
        assert!(parsed.subprojects_config.is_kotlin);
        assert_eq!(
            parsed.subprojects_config.test_deps,
            vec!["org.junit.jupiter:junit-jupiter:5.11.4"]
        );
    }

    #[test]
    fn parse_platform_deps() {
        let src = r#"
            plugins { kotlin("jvm") }
            dependencies {
                implementation(platform("org.jetbrains.kotlinx:kotlinx-coroutines-bom:1.10.1"))
                implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(
            parsed.project.platform_deps,
            vec!["org.jetbrains.kotlinx:kotlinx-coroutines-bom:1.10.1"]
        );
        // Versionless dep is kept as-is (BOM resolves at build time).
        assert_eq!(
            parsed.project.external_deps,
            vec!["org.jetbrains.kotlinx:kotlinx-coroutines-core"]
        );
    }

    #[test]
    fn parse_runtime_only_and_debug_impl() {
        let src = r#"
            dependencies {
                runtimeOnly("com.h2database:h2:2.3.232")
                debugImplementation("com.example:debug-tools:1.0")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed
            .project
            .external_deps
            .contains(&"com.h2database:h2:2.3.232".to_string()));
        assert!(parsed
            .project
            .external_deps
            .contains(&"com.example:debug-tools:1.0".to_string()));
    }

    #[test]
    fn parse_multiplatform_flag() {
        let src = r#"
            plugins { kotlin("multiplatform") }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed.project.is_multiplatform);
        assert!(parsed.project.is_kotlin);
    }

    #[test]
    fn resolve_catalog_bundles() {
        let catalog_src = r#"
[versions]
junit = "5.11.4"
coroutines = "1.10.1"

[libraries]
junit-jupiter = "org.junit.jupiter:junit-jupiter:5.11.4"
coroutines-test = "org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.1"
mockk = "io.mockk:mockk:1.13.14"

[bundles]
testing = ["junit-jupiter", "coroutines-test", "mockk"]

[plugins]
"#;
        let catalog = crate::version_catalog::parse_version_catalog_str(catalog_src).unwrap();
        let mut project = ProjectModel {
            test_deps: vec!["libs.bundles.testing".to_string()],
            ..Default::default()
        };
        resolve_catalog_deps(&mut project, &catalog);
        assert_eq!(project.test_deps.len(), 3);
        assert!(project
            .test_deps
            .contains(&"org.junit.jupiter:junit-jupiter:5.11.4".to_string()));
        assert!(project
            .test_deps
            .contains(&"io.mockk:mockk:1.13.14".to_string()));
    }

    #[test]
    fn string_interpolation_in_deps() {
        let src = r#"
            plugins { kotlin("jvm") }
            dependencies {
                val ktor_version = "3.1.0"
                implementation("io.ktor:ktor-server-core:$ktor_version")
                implementation("io.ktor:ktor-server-netty:$ktor_version")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed
            .project
            .external_deps
            .contains(&"io.ktor:ktor-server-core:3.1.0".to_string()));
        assert!(parsed
            .project
            .external_deps
            .contains(&"io.ktor:ktor-server-netty:3.1.0".to_string()));
    }

    #[test]
    fn top_level_val_interpolation() {
        let src = r#"
            val myVersion = "2.0.0"
            plugins { kotlin("jvm") }
            dependencies {
                implementation("com.example:lib:$myVersion")
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert_eq!(parsed.project.external_deps, vec!["com.example:lib:2.0.0"]);
    }

    #[test]
    fn typesafe_project_accessors() {
        let src = r#"
            plugins { kotlin("jvm") }
            dependencies {
                implementation(projects.core)
                implementation(projects.utils)
            }
        "#;
        let mut interner = Interner::new();
        let parsed = parse_buildfile(src, FileId(0), &mut interner);
        assert!(parsed.project.project_deps.contains(&":core".to_string()));
        assert!(parsed.project.project_deps.contains(&":utils".to_string()));
    }

    #[test]
    fn parse_compose_sample_settings() {
        let src = r#"
val snapshotVersion : String? = System.getenv("COMPOSE_SNAPSHOT_ID")
pluginManagement {
    repositories {
        gradlePluginPortal()
        google()
        mavenCentral()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "Jetchat"
include(":app")
"#;
        let mut interner = Interner::new();
        let parsed = parse_settings(src, FileId(0), &mut interner);
        assert_eq!(
            parsed.settings.root_project_name.as_deref(),
            Some("Jetchat")
        );
        assert_eq!(parsed.settings.included_modules, vec![":app"]);
    }
}
