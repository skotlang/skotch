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
    pub group: Option<String>,
    pub version: Option<String>,
    pub target: Option<BuildTarget>,
    pub main_class: Option<String>,
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

pub struct ParsedBuildFile {
    pub project: ProjectModel,
    pub diags: Diagnostics,
}

pub struct ParsedSettings {
    pub settings: SettingsModel,
    pub diags: Diagnostics,
}

// ── Entry points ────────────────────────────────────────────────────────

/// Parse a `build.gradle.kts` source string into a [`ProjectModel`].
pub fn parse_buildfile(src: &str, file: FileId, _interner: &mut Interner) -> ParsedBuildFile {
    let mut diags = Diagnostics::new();
    let lexed = skotch_lexer::lex(file, src, &mut diags);
    let mut walker = Walker::new(src, &lexed);
    walker.parse_build();
    ParsedBuildFile {
        project: walker.project,
        diags,
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
}

impl<'src, 'lf> Walker<'src, 'lf> {
    fn new(src: &'src str, lexed: &'lf LexedFile) -> Self {
        Self {
            src,
            lexed,
            pos: 0,
            project: ProjectModel::default(),
            settings: SettingsModel::default(),
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
                        TokenKind::StringEnd => {
                            self.bump();
                            break;
                        }
                        TokenKind::Eof => break,
                        _ => {
                            // StringIdentRef, StringExprStart, etc. — skip
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
        if self.peek() != TokenKind::Ident {
            return false;
        }
        let span = self.bump();
        let ident = self.text(span);
        match ident {
            "plugins" => self.parse_plugins_block(),
            "group" => self.parse_string_assign("group"),
            "version" => self.parse_string_assign("version"),
            "repositories" => self.skip_brace_block(),
            "dependencies" => self.parse_dependencies_block(),
            "application" => self.parse_application_block(),
            "android" => self.parse_android_block(),
            "kotlin" | "java" | "tasks" | "sourceSets" | "allprojects" | "subprojects"
            | "buildscript" => self.skip_brace_block(),
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
                    "kotlin" => {
                        if self.peek() == TokenKind::LParen {
                            self.bump();
                            if let Some(val) = self.try_consume_string() {
                                match val.as_str() {
                                    "jvm" | "multiplatform" => {
                                        self.project.target.get_or_insert(BuildTarget::Jvm);
                                    }
                                    "android" => {
                                        self.project.target = Some(BuildTarget::Android);
                                    }
                                    _ => {}
                                }
                            }
                            self.skip_past(TokenKind::RParen);
                        }
                    }
                    "id" => {
                        if self.peek() == TokenKind::LParen {
                            self.bump();
                            if let Some(val) = self.try_consume_string() {
                                if val.contains("com.android.application") {
                                    self.project.target = Some(BuildTarget::Android);
                                }
                            }
                            self.skip_past(TokenKind::RParen);
                        }
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
            // Look for: implementation(project(":lib"))
            if self.peek() == TokenKind::Ident {
                let span = self.bump();
                let name = self.text(span).to_string();
                if (name == "implementation" || name == "api") && self.peek() == TokenKind::LParen {
                    self.bump();
                    if self.peek() == TokenKind::Ident {
                        let inner = self.bump();
                        if self.text(inner) == "project" && self.peek() == TokenKind::LParen {
                            self.bump();
                            if let Some(dep) = self.try_consume_string() {
                                self.project.project_deps.push(dep);
                            }
                            self.skip_past(TokenKind::RParen);
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
    let inner = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
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
}
