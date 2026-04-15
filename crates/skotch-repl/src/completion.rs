//! Tab completion for the skotch REPL.
//!
//! Provides completions for:
//!
//! - **REPL colon-commands** (`:quit`, `:help`, `:cpadd`, `:cplist`, …)
//! - **Kotlin keywords** and common stdlib names
//! - **Locally defined** variables, classes, and functions
//! - **Members** (methods / fields) on variables with known types,
//!   resolved via live JVM reflection through the embedded JVM
//! - **File paths** after `:cpadd`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use reedline::{Completer, Span, Suggestion};
use skotch_jvm::EmbeddedJvm;

// ── Constants ────────────────────────────────────────────────────────

/// REPL colon-commands with short descriptions.
const REPL_COMMANDS: &[(&str, &str)] = &[
    (":quit", "exit the REPL"),
    (":exit", "exit the REPL"),
    (":q", "exit the REPL"),
    (":help", "show help"),
    (":h", "show help"),
    (":?", "show help"),
    (":history", "show declarations"),
    (":hist", "show declarations"),
    (":reset", "clear state"),
    (":clear", "clear state"),
    (":type", "show type of expression"),
    (":cpadd", "add JAR/dir to classpath"),
    (":cplist", "list classpath entries"),
];

/// Kotlin language keywords.
const KOTLIN_KEYWORDS: &[&str] = &[
    "abstract",
    "annotation",
    "as",
    "break",
    "by",
    "catch",
    "class",
    "companion",
    "const",
    "constructor",
    "continue",
    "crossinline",
    "data",
    "do",
    "else",
    "enum",
    "expect",
    "external",
    "false",
    "field",
    "final",
    "finally",
    "for",
    "fun",
    "get",
    "if",
    "import",
    "in",
    "infix",
    "init",
    "inline",
    "inner",
    "interface",
    "internal",
    "is",
    "it",
    "lateinit",
    "noinline",
    "null",
    "object",
    "open",
    "operator",
    "out",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "reified",
    "return",
    "sealed",
    "set",
    "super",
    "suspend",
    "tailrec",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "vararg",
    "when",
    "where",
    "while",
];

/// Common Kotlin/Java stdlib types and functions.
const KOTLIN_BUILTINS: &[&str] = &[
    // I/O
    "println",
    "print",
    "readLine",
    "readln",
    // Collection factories
    "listOf",
    "mutableListOf",
    "arrayListOf",
    "mapOf",
    "mutableMapOf",
    "hashMapOf",
    "linkedMapOf",
    "setOf",
    "mutableSetOf",
    "hashSetOf",
    "linkedSetOf",
    "arrayOf",
    "intArrayOf",
    "doubleArrayOf",
    "floatArrayOf",
    "longArrayOf",
    "booleanArrayOf",
    "charArrayOf",
    "byteArrayOf",
    "shortArrayOf",
    "emptyList",
    "emptyMap",
    "emptySet",
    "buildList",
    "buildMap",
    "buildSet",
    // Scope functions
    "lazy",
    "require",
    "check",
    "error",
    "TODO",
    "repeat",
    "run",
    "with",
    "apply",
    "also",
    "let",
    "takeIf",
    "takeUnless",
    "maxOf",
    "minOf",
    // Primitive types
    "String",
    "Int",
    "Long",
    "Double",
    "Float",
    "Boolean",
    "Char",
    "Byte",
    "Short",
    "Unit",
    "Nothing",
    "Any",
    // Data structures
    "Pair",
    "Triple",
    "List",
    "Map",
    "Set",
    "MutableList",
    "MutableMap",
    "MutableSet",
    "Array",
    "IntArray",
    "LongArray",
    "DoubleArray",
    "Sequence",
    "Iterable",
    "Iterator",
    // Text
    "Regex",
    "StringBuilder",
    "Comparable",
    // Exceptions
    "Exception",
    "RuntimeException",
    "IllegalArgumentException",
    "IllegalStateException",
    "IndexOutOfBoundsException",
    "NullPointerException",
    "UnsupportedOperationException",
    "NumberFormatException",
    "ClassCastException",
    "ArithmeticException",
    // Result / Lazy
    "Result",
    "Lazy",
    // Common Java stdlib
    "System",
    "Math",
    "Thread",
    "Runtime",
    "File",
    "Path",
    "Paths",
    "Files",
    "BufferedReader",
    "InputStreamReader",
    "PrintWriter",
    "HashMap",
    "ArrayList",
    "LinkedList",
    "TreeMap",
    "TreeSet",
    "HashSet",
    "Collections",
    "Arrays",
    "Pattern",
    "Matcher",
    "UUID",
    "Random",
    "BigDecimal",
    "BigInteger",
    "LocalDate",
    "LocalTime",
    "LocalDateTime",
    "Instant",
    "Duration",
    "ZonedDateTime",
    "DateTimeFormatter",
    "URL",
    "URI",
    "Stream",
    "Collectors",
    "Optional",
    "Properties",
    "Integer",
    "Character",
];

// ── Kotlin extension methods by type ─────────────────────────────────

/// Return Kotlin-specific extension method/property names for a type.
/// These supplement the JVM reflection results which miss Kotlin
/// extensions (they're compiled as static helpers, not instance methods).
fn kotlin_extensions_for(kt_type: &str) -> &'static [&'static str] {
    let base = kt_type.split('<').next().unwrap_or(kt_type);
    match base {
        "kotlin.String" | "String" | "java.lang.String" => &[
            "length",
            "uppercase",
            "lowercase",
            "trim",
            "trimStart",
            "trimEnd",
            "startsWith",
            "endsWith",
            "contains",
            "indexOf",
            "lastIndexOf",
            "replace",
            "replaceFirst",
            "split",
            "lines",
            "substring",
            "take",
            "drop",
            "takeLast",
            "dropLast",
            "toInt",
            "toLong",
            "toDouble",
            "toFloat",
            "toBoolean",
            "toIntOrNull",
            "toLongOrNull",
            "toDoubleOrNull",
            "reversed",
            "repeat",
            "padStart",
            "padEnd",
            "isEmpty",
            "isNotEmpty",
            "isBlank",
            "isNotBlank",
            "first",
            "last",
            "firstOrNull",
            "lastOrNull",
            "toByteArray",
            "toCharArray",
            "toList",
            "compareTo",
            "removePrefix",
            "removeSuffix",
            "removeSurrounding",
            "substringBefore",
            "substringAfter",
            "substringBeforeLast",
            "substringAfterLast",
            "matches",
        ],
        "kotlin.Int" | "Int" | "java.lang.Integer" | "kotlin.Long" | "Long" | "java.lang.Long"
        | "kotlin.Double" | "Double" | "java.lang.Double" | "kotlin.Float" | "Float"
        | "java.lang.Float" | "kotlin.Byte" | "Byte" | "kotlin.Short" | "Short" => &[
            "toInt",
            "toLong",
            "toDouble",
            "toFloat",
            "toByte",
            "toShort",
            "toChar",
            "toString",
            "compareTo",
            "coerceAtLeast",
            "coerceAtMost",
            "coerceIn",
            "rangeTo",
            "rangeUntil",
            "plus",
            "minus",
            "times",
            "div",
            "rem",
            "inc",
            "dec",
            "unaryMinus",
            "unaryPlus",
        ],
        "kotlin.Boolean" | "Boolean" | "java.lang.Boolean" => {
            &["not", "and", "or", "xor", "toString", "compareTo"]
        }
        "kotlin.collections.List"
        | "List"
        | "java.util.List"
        | "kotlin.collections.MutableList"
        | "MutableList"
        | "java.util.ArrayList"
        | "kotlin.collections.Set"
        | "Set"
        | "java.util.Set"
        | "kotlin.collections.MutableSet"
        | "MutableSet"
        | "java.util.HashSet" => &[
            "size",
            "isEmpty",
            "isNotEmpty",
            "first",
            "last",
            "firstOrNull",
            "lastOrNull",
            "get",
            "indexOf",
            "lastIndexOf",
            "contains",
            "containsAll",
            "filter",
            "filterNot",
            "filterNotNull",
            "filterIsInstance",
            "map",
            "mapNotNull",
            "mapIndexed",
            "flatMap",
            "flatten",
            "forEach",
            "forEachIndexed",
            "onEach",
            "any",
            "all",
            "none",
            "count",
            "find",
            "findLast",
            "groupBy",
            "associate",
            "associateBy",
            "associateWith",
            "sorted",
            "sortedBy",
            "sortedByDescending",
            "sortedWith",
            "reversed",
            "distinct",
            "distinctBy",
            "take",
            "drop",
            "takeLast",
            "dropLast",
            "takeWhile",
            "dropWhile",
            "zip",
            "zipWithNext",
            "joinToString",
            "toList",
            "toMutableList",
            "toSet",
            "toMutableSet",
            "toMap",
            "sum",
            "sumOf",
            "average",
            "min",
            "max",
            "minOrNull",
            "maxOrNull",
            "minBy",
            "maxBy",
            "minByOrNull",
            "maxByOrNull",
            "plus",
            "minus",
            "chunked",
            "windowed",
            "fold",
            "foldRight",
            "reduce",
            "reduceOrNull",
            "partition",
            "withIndex",
            "indices",
            "single",
            "singleOrNull",
            "random",
            "randomOrNull",
            "shuffled",
            "asSequence",
            "iterator",
            "add",
            "addAll",
            "remove",
            "removeAll",
            "clear",
            "retainAll",
            "subList",
        ],
        "kotlin.collections.Map"
        | "Map"
        | "java.util.Map"
        | "kotlin.collections.MutableMap"
        | "MutableMap"
        | "java.util.HashMap" => &[
            "size",
            "isEmpty",
            "isNotEmpty",
            "get",
            "getOrDefault",
            "getOrElse",
            "getOrPut",
            "containsKey",
            "containsValue",
            "keys",
            "values",
            "entries",
            "forEach",
            "map",
            "mapKeys",
            "mapValues",
            "filter",
            "filterKeys",
            "filterValues",
            "filterNot",
            "any",
            "all",
            "none",
            "count",
            "toList",
            "toMap",
            "toMutableMap",
            "plus",
            "minus",
            "put",
            "putAll",
            "remove",
            "clear",
        ],
        _ => &[],
    }
}

// ── Scan mode ────────────────────────────────────────────────────────

/// Controls when the JDK / classpath class index is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    /// Scan in a background thread immediately after JVM init.
    /// The REPL prompt appears without waiting. (default)
    Background,
    /// Scan synchronously before the first prompt.
    Eager,
    /// Defer scanning until the first tab-completion request.
    Lazy,
    /// Never scan — only locally defined names are completed.
    None,
}

impl std::fmt::Display for ScanMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanMode::Background => write!(f, "background"),
            ScanMode::Eager => write!(f, "eager"),
            ScanMode::Lazy => write!(f, "lazy"),
            ScanMode::None => write!(f, "none"),
        }
    }
}

impl std::str::FromStr for ScanMode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "background" => Ok(ScanMode::Background),
            "eager" => Ok(ScanMode::Eager),
            "lazy" => Ok(ScanMode::Lazy),
            "none" => Ok(ScanMode::None),
            other => Err(format!(
                "unknown scan mode `{other}` (expected: background, eager, lazy, none)"
            )),
        }
    }
}

// ── Shared completion context ────────────────────────────────────────

/// Mutable state shared between the REPL loop and the tab-completer
/// via `Arc<Mutex<_>>`.
pub(crate) struct CompletionCtx {
    /// Local variable names with their known Kotlin types.
    local_vars: Vec<(String, Option<String>)>,
    /// Top-level declaration names with their kind (`"class"`, `"fun"`, …).
    top_names: Vec<(String, String)>,
    /// How many `resN` result variables exist.
    res_count: usize,
    /// Simple (unqualified) class names from classpath scanning.
    extra_classes: Vec<String>,
    /// Fully-qualified class names from classpath scanning, used for
    /// dotted package/class completion (e.g. `com.example.Foo`).
    extra_fqn_classes: Vec<String>,
    /// Cached member lists per Java class name.
    member_cache: HashMap<String, Vec<String>>,
    /// Whether a lazy system-class scan is still pending.
    pub lazy_scan_pending: bool,
}

impl CompletionCtx {
    pub fn new() -> Self {
        CompletionCtx {
            local_vars: Vec::new(),
            top_names: Vec::new(),
            res_count: 0,
            extra_classes: Vec::new(),
            extra_fqn_classes: Vec::new(),
            member_cache: HashMap::new(),
            lazy_scan_pending: false,
        }
    }

    /// Re-synchronize from current REPL declarations.
    pub fn sync_from_repl(
        &mut self,
        top_decls: &[String],
        local_decls: &[String],
        res_count: usize,
        known_types: &HashMap<String, String>,
    ) {
        self.local_vars.clear();
        for decl in local_decls {
            if let Some((name, ty)) = parse_local_decl(decl) {
                let final_type = known_types.get(&name).cloned().or(ty);
                self.local_vars.push((name, final_type));
            }
        }

        self.top_names.clear();
        for decl in top_decls {
            if let Some((name, kind)) = parse_top_decl(decl) {
                self.top_names.push((name, kind.to_string()));
            }
        }

        self.res_count = res_count;
    }

    /// Register class names discovered from a newly added classpath entry.
    pub fn add_extra_classes(&mut self, classes: Vec<String>) {
        for fqn in classes {
            // Store the fully-qualified name for dotted completion.
            if !self.extra_fqn_classes.contains(&fqn) {
                self.extra_fqn_classes.push(fqn.clone());
            }
            // Store the simple name for unqualified completion.
            if let Some(simple) = fqn.rsplit('.').next() {
                if !simple.is_empty() && simple.starts_with(|c: char| c.is_uppercase()) {
                    let s = simple.to_string();
                    if !self.extra_classes.contains(&s) {
                        self.extra_classes.push(s);
                    }
                }
            }
        }
        self.extra_classes.sort();
        self.extra_classes.dedup();
        self.extra_fqn_classes.sort();
        self.extra_fqn_classes.dedup();
    }

    /// Look up the Kotlin type of a named variable.
    fn type_of_var(&self, name: &str) -> Option<&str> {
        for (vname, vtype) in &self.local_vars {
            if vname == name {
                return vtype.as_deref();
            }
        }
        None
    }
}

// ── Completer ────────────────────────────────────────────────────────

pub(crate) struct SkotchCompleter {
    pub ctx: Arc<Mutex<CompletionCtx>>,
    pub verbose: bool,
}

impl SkotchCompleter {
    /// If a lazy scan is pending, perform it now (blocking).
    fn ensure_scanned(&self) {
        let needs_scan = {
            let ctx = self.ctx.lock().unwrap();
            ctx.lazy_scan_pending
        };
        if needs_scan {
            let t0 = std::time::Instant::now();
            if let Ok(jvm) = EmbeddedJvm::new() {
                if let Ok(classes) = jvm.scan_system_classes() {
                    let count = classes.len();
                    let secs = t0.elapsed().as_secs_f64();
                    let mut ctx = self.ctx.lock().unwrap();
                    ctx.add_extra_classes(classes);
                    ctx.lazy_scan_pending = false;
                    drop(ctx);
                    if self.verbose {
                        eprintln!("  classpath: {count} system classes indexed ({secs:.1}s, lazy)");
                    }
                }
            }
        }
    }
}

impl Completer for SkotchCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        self.ensure_scanned();
        let line = &line[..pos.min(line.len())];

        // 1) REPL colon-commands.
        if line.starts_with(':') {
            // File-path completion after `:cpadd `.
            if let Some(path_part) = line.strip_prefix(":cpadd ") {
                let span_start = line.len() - path_part.len();
                return self.complete_file_path(path_part, pos, span_start);
            }
            return self.complete_command(line, pos);
        }

        // 2) Extract the simple word at cursor (stops at dots).
        let (word, word_start) = extract_word_before(line, pos);

        // 3) Check for dot context.
        if word_start > 0 && line.as_bytes()[word_start - 1] == b'.' {
            let dot_pos = word_start - 1;

            // String literal receiver: `"hello".prefix`
            if dot_pos > 0 && line.as_bytes()[dot_pos - 1] == b'"' {
                return self.complete_members("kotlin.String", &word, pos, word_start);
            }

            // Walk back through dots + identifiers to get the full
            // receiver path (e.g. `java.util` from `java.util.Hash`).
            let (recv, recv_start) = extract_dotted_word_before(line, dot_pos);

            if !recv.is_empty() {
                // a) Simple variable member access: `myVar.method`
                if !recv.contains('.') {
                    if let Some(rtype) = self.resolve_type(&recv) {
                        return self.complete_members(&rtype, &word, pos, word_start);
                    }
                }

                // b) Single uppercase name — try as class (static members):
                //    `System.exit`, `Math.abs`
                if !recv.contains('.') && recv.starts_with(|c: char| c.is_uppercase()) {
                    let members = self.complete_members(&recv, &word, pos, word_start);
                    if !members.is_empty() {
                        return members;
                    }
                }

                // c) Qualified package / class name: `java.util.Hash`
                let full_prefix = if word.is_empty() {
                    format!("{recv}.")
                } else {
                    format!("{recv}.{word}")
                };
                let qualified = self.complete_qualified(&full_prefix, pos, recv_start);
                if !qualified.is_empty() {
                    return qualified;
                }

                // d) FQN class member access: `java.lang.String.method`
                if recv.contains('.') {
                    let last_seg = recv.rsplit('.').next().unwrap_or("");
                    if last_seg.starts_with(|c: char| c.is_uppercase()) {
                        return self.complete_members(&recv, &word, pos, word_start);
                    }
                }
            }
        }

        // 4) Regular identifier completion.
        if word.is_empty() {
            return Vec::new();
        }
        self.complete_identifier(&word, pos, word_start)
    }
}

impl SkotchCompleter {
    // ── REPL command completion ──────────────────────────────────────

    fn complete_command(&self, prefix: &str, pos: usize) -> Vec<Suggestion> {
        REPL_COMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(prefix))
            .map(|(cmd, desc)| Suggestion {
                value: cmd.to_string(),
                description: Some(desc.to_string()),
                span: Span::new(0, pos),
                ..Default::default()
            })
            .collect()
    }

    // ── File-path completion for :cpadd ──────────────────────────────

    fn complete_file_path(&self, partial: &str, pos: usize, span_start: usize) -> Vec<Suggestion> {
        let (dir_str, file_prefix) = match partial.rfind('/') {
            Some(sep) => (&partial[..=sep], &partial[sep + 1..]),
            None => ("", partial),
        };

        let dir_path = if dir_str.starts_with("~/") {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(dir_str.strip_prefix("~/").unwrap_or(dir_str))
        } else if dir_str.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(dir_str)
        };

        let mut results = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with(file_prefix) {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                // Only show JARs, ZIPs, and directories.
                if !is_dir && !name.ends_with(".jar") && !name.ends_with(".zip") {
                    continue;
                }
                let full = if dir_str.is_empty() {
                    name.clone()
                } else {
                    format!("{dir_str}{name}")
                };
                let value = if is_dir { format!("{full}/") } else { full };
                results.push(Suggestion {
                    value,
                    description: if is_dir {
                        Some("dir".into())
                    } else {
                        Some("jar".into())
                    },
                    span: Span::new(span_start, pos),
                    ..Default::default()
                });
            }
        }
        results.sort_by(|a, b| a.value.cmp(&b.value));
        results
    }

    // ── Identifier completion ────────────────────────────────────────

    fn complete_identifier(&self, prefix: &str, pos: usize, span_start: usize) -> Vec<Suggestion> {
        let lc = prefix.to_lowercase();
        let span = Span::new(span_start, pos);
        let mut suggestions: Vec<Suggestion> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        let ctx = self.ctx.lock().unwrap();

        // Local variables.
        for (name, _) in &ctx.local_vars {
            if name.to_lowercase().starts_with(&lc) && seen.insert(name.clone()) {
                suggestions.push(Suggestion {
                    value: name.clone(),
                    description: Some("var".into()),
                    span,
                    ..Default::default()
                });
            }
        }

        // Result variables (res0 .. resN-1).
        for i in 0..ctx.res_count {
            let name = format!("res{i}");
            if name.starts_with(prefix) && seen.insert(name.clone()) {
                suggestions.push(Suggestion {
                    value: name,
                    description: Some("result".into()),
                    span,
                    ..Default::default()
                });
            }
        }

        // Top-level declarations.
        for (name, kind) in &ctx.top_names {
            if name.to_lowercase().starts_with(&lc) && seen.insert(name.clone()) {
                suggestions.push(Suggestion {
                    value: name.clone(),
                    description: Some(kind.clone()),
                    span,
                    ..Default::default()
                });
            }
        }

        // Extra classes from classpath.
        for name in &ctx.extra_classes {
            if name.to_lowercase().starts_with(&lc) && seen.insert(name.clone()) {
                suggestions.push(Suggestion {
                    value: name.clone(),
                    description: Some("class".into()),
                    span,
                    ..Default::default()
                });
            }
        }

        drop(ctx);

        // Kotlin builtins.
        for &name in KOTLIN_BUILTINS {
            if name.to_lowercase().starts_with(&lc) && seen.insert(name.to_string()) {
                suggestions.push(Suggestion {
                    value: name.to_string(),
                    description: Some("builtin".into()),
                    span,
                    ..Default::default()
                });
            }
        }

        // Kotlin keywords.
        for &kw in KOTLIN_KEYWORDS {
            if kw.starts_with(prefix) && seen.insert(kw.to_string()) {
                suggestions.push(Suggestion {
                    value: kw.to_string(),
                    description: Some("keyword".into()),
                    span,
                    ..Default::default()
                });
            }
        }

        suggestions.sort_by(|a, b| a.value.cmp(&b.value));
        suggestions
    }

    // ── Member completion ────────────────────────────────────────────

    fn complete_members(
        &self,
        receiver_type: &str,
        prefix: &str,
        pos: usize,
        span_start: usize,
    ) -> Vec<Suggestion> {
        let members = self.get_or_query_members(receiver_type);
        let lc = prefix.to_lowercase();

        members
            .iter()
            .filter(|m| m.to_lowercase().starts_with(&lc))
            .map(|m| Suggestion {
                value: m.clone(),
                span: Span::new(span_start, pos),
                ..Default::default()
            })
            .collect()
    }

    /// Retrieve cached member names for a type, or query the JVM and cache.
    fn get_or_query_members(&self, receiver_type: &str) -> Vec<String> {
        let mut ctx = self.ctx.lock().unwrap();
        if let Some(cached) = ctx.member_cache.get(receiver_type) {
            return cached.clone();
        }

        // Map Kotlin type → Java class name for reflection.
        let java_class = kotlin_to_java_class(receiver_type)
            .unwrap_or(receiver_type)
            .to_string();

        let mut members = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Live JVM reflection.
        if let Ok(jvm) = EmbeddedJvm::new() {
            // Try the mapped name first; if it fails and differs from
            // the original, try the original too.
            let mut try_class = |name: &str| {
                if let Ok(jvm_members) = jvm.list_members(name) {
                    for (n, _kind) in jvm_members {
                        if seen.insert(n.clone()) {
                            members.push(n);
                        }
                    }
                }
            };
            try_class(&java_class);
            if java_class != receiver_type {
                try_class(receiver_type);
            }
        }

        // Merge Kotlin extension methods.
        for &ext in kotlin_extensions_for(receiver_type) {
            if seen.insert(ext.to_string()) {
                members.push(ext.to_string());
            }
        }

        members.sort();
        members.dedup();
        ctx.member_cache
            .insert(receiver_type.to_string(), members.clone());
        members
    }

    // ── Qualified name completion ────────────────────────────────────

    /// Complete a dotted package / class prefix like `java.util.Hash`.
    ///
    /// Matches against the dynamically discovered FQN class list
    /// (populated from JDK jmods/rt.jar at startup plus any user-added
    /// classpath entries). For each match, extracts the next path
    /// segment so that `java.` yields `java.lang`, `java.util`, …
    /// and `java.util.` yields `java.util.List`, `java.util.HashMap`, …
    fn complete_qualified(
        &self,
        dotted_prefix: &str,
        pos: usize,
        span_start: usize,
    ) -> Vec<Suggestion> {
        let lc = dotted_prefix.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut suggestions = Vec::new();

        let ctx = self.ctx.lock().unwrap();

        for fqn in ctx.extra_fqn_classes.iter().map(|s| s.as_str()) {
            if !fqn.to_lowercase().starts_with(&lc) || fqn.len() <= dotted_prefix.len() {
                continue;
            }
            // Extract the next segment after the prefix.
            let rest = &fqn[dotted_prefix.len()..];
            let value = match rest.find('.') {
                Some(d) => &fqn[..dotted_prefix.len() + d], // package
                None => fqn,                                // leaf class
            };
            if seen.insert(value.to_string()) {
                let is_class = !rest.contains('.');
                suggestions.push(Suggestion {
                    value: value.to_string(),
                    description: Some(if is_class { "class" } else { "package" }.into()),
                    span: Span::new(span_start, pos),
                    ..Default::default()
                });
            }
        }

        suggestions.sort_by(|a, b| a.value.cmp(&b.value));
        suggestions
    }

    // ── Type resolution ──────────────────────────────────────────────

    /// Resolve a receiver name to its Kotlin type.
    fn resolve_type(&self, name: &str) -> Option<String> {
        let ctx = self.ctx.lock().unwrap();
        if let Some(ty) = ctx.type_of_var(name) {
            return Some(ty.to_string());
        }
        drop(ctx);

        // Uppercase-initial names are treated as class names
        // (for static method completion like `System.exit`).
        if name.starts_with(|c: char| c.is_uppercase()) {
            return Some(name.to_string());
        }
        None
    }
}

// ── Parsing helpers ──────────────────────────────────────────────────

/// Extract a `val`/`var` declaration's name and optional type annotation.
pub(crate) fn parse_local_decl(decl: &str) -> Option<(String, Option<String>)> {
    let decl = decl.trim();
    let rest = decl
        .strip_prefix("val ")
        .or_else(|| decl.strip_prefix("var "))?;

    let name_end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let name = rest[..name_end].to_string();
    if name.is_empty() {
        return None;
    }

    let after_name = rest[name_end..].trim_start();

    // Explicit type annotation: `val x: String = …`
    if let Some(type_rest) = after_name.strip_prefix(':') {
        let type_rest = type_rest.trim_start();
        let type_end = type_rest.find('=').unwrap_or(type_rest.len());
        let type_name = type_rest[..type_end].trim().to_string();
        if !type_name.is_empty() {
            return Some((name, Some(type_name)));
        }
    }

    // Infer type from the RHS expression.
    if let Some(eq_rest) = after_name.strip_prefix('=') {
        let rhs = eq_rest.trim_start();
        return Some((name, infer_type_from_rhs(rhs)));
    }

    Some((name, None))
}

/// Extract a top-level declaration's name and kind.
pub(crate) fn parse_top_decl(decl: &str) -> Option<(String, &'static str)> {
    let decl = decl.trim();

    let (rest, kind) = if let Some(r) = decl.strip_prefix("data class ") {
        (r, "class")
    } else if let Some(r) = decl.strip_prefix("sealed class ") {
        (r, "class")
    } else if let Some(r) = decl.strip_prefix("abstract class ") {
        (r, "class")
    } else if let Some(r) = decl.strip_prefix("open class ") {
        (r, "class")
    } else if let Some(r) = decl.strip_prefix("enum class ") {
        (r, "class")
    } else if let Some(r) = decl.strip_prefix("inner class ") {
        (r, "class")
    } else if let Some(r) = decl.strip_prefix("class ") {
        (r, "class")
    } else if let Some(r) = decl.strip_prefix("interface ") {
        (r, "interface")
    } else if let Some(r) = decl.strip_prefix("object ") {
        (r, "object")
    } else if let Some(r) = decl.strip_prefix("fun ") {
        (r, "fun")
    } else if let Some(r) = decl.strip_prefix("typealias ") {
        (r, "typealias")
    } else {
        return None;
    };

    let name_end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let name = rest[..name_end].to_string();
    if name.is_empty() {
        return None;
    }
    Some((name, kind))
}

/// Best-effort type inference from the RHS of a `val`/`var`.
fn infer_type_from_rhs(rhs: &str) -> Option<String> {
    let rhs = rhs.trim();

    // String literal.
    if rhs.starts_with('"') {
        return Some("kotlin.String".to_string());
    }

    // Boolean literal.
    if rhs == "true" || rhs == "false" {
        return Some("kotlin.Boolean".to_string());
    }

    // Number literal.
    if rhs.starts_with(|c: char| c.is_ascii_digit())
        || (rhs.starts_with('-') && rhs.len() > 1 && rhs.as_bytes()[1].is_ascii_digit())
    {
        if rhs.ends_with('f') || rhs.ends_with('F') {
            return Some("kotlin.Float".to_string());
        }
        if rhs.contains('.') {
            return Some("kotlin.Double".to_string());
        }
        if rhs.ends_with('L') || rhs.ends_with('l') {
            return Some("kotlin.Long".to_string());
        }
        return Some("kotlin.Int".to_string());
    }

    // Collection factory functions.
    let factories: &[(&str, &str)] = &[
        ("listOf(", "kotlin.collections.List"),
        ("emptyList(", "kotlin.collections.List"),
        ("mutableListOf(", "kotlin.collections.MutableList"),
        ("arrayListOf(", "kotlin.collections.MutableList"),
        ("mapOf(", "kotlin.collections.Map"),
        ("emptyMap(", "kotlin.collections.Map"),
        ("mutableMapOf(", "kotlin.collections.MutableMap"),
        ("hashMapOf(", "kotlin.collections.MutableMap"),
        ("setOf(", "kotlin.collections.Set"),
        ("emptySet(", "kotlin.collections.Set"),
        ("mutableSetOf(", "kotlin.collections.MutableSet"),
        ("hashSetOf(", "kotlin.collections.MutableSet"),
        ("arrayOf(", "kotlin.Array"),
        ("StringBuilder(", "kotlin.text.StringBuilder"),
        ("Regex(", "kotlin.text.Regex"),
    ];
    for &(prefix, ty) in factories {
        if rhs.starts_with(prefix) {
            return Some(ty.to_string());
        }
    }

    // Constructor call: `ClassName(…)`.
    if let Some(paren) = rhs.find('(') {
        let before = rhs[..paren].trim();
        if !before.is_empty()
            && before.starts_with(|c: char| c.is_uppercase())
            && before
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            return Some(before.to_string());
        }
    }

    None
}

// ── Misc helpers ─────────────────────────────────────────────────────

/// Extract the identifier word ending at `pos` (walking backwards).
/// Stops at dots, whitespace, and other non-identifier characters.
fn extract_word_before(line: &str, pos: usize) -> (String, usize) {
    let bytes = line.as_bytes();
    let mut start = pos;
    while start > 0 && is_ident_char(bytes[start - 1]) {
        start -= 1;
    }
    (line[start..pos].to_string(), start)
}

/// Like [`extract_word_before`] but also walks through dots, so
/// `java.util` is extracted as a single token from `val x = java.util`.
fn extract_dotted_word_before(line: &str, pos: usize) -> (String, usize) {
    let bytes = line.as_bytes();
    let mut start = pos;
    while start > 0 && (is_ident_char(bytes[start - 1]) || bytes[start - 1] == b'.') {
        start -= 1;
    }
    // Trim a leading dot if present (e.g. from `.method` after a literal).
    if start < pos && bytes[start] == b'.' {
        start += 1;
    }
    (line[start..pos].to_string(), start)
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Map a Kotlin type name to the corresponding Java class name for
/// JVM reflection via `Class.forName()`.
fn kotlin_to_java_class(kt_type: &str) -> Option<&'static str> {
    // Strip generic parameters: `List<String>` → `List`.
    let base = kt_type.split('<').next().unwrap_or(kt_type).trim();

    match base {
        "Int" | "kotlin.Int" => Some("java.lang.Integer"),
        "Long" | "kotlin.Long" => Some("java.lang.Long"),
        "Double" | "kotlin.Double" => Some("java.lang.Double"),
        "Float" | "kotlin.Float" => Some("java.lang.Float"),
        "Boolean" | "kotlin.Boolean" => Some("java.lang.Boolean"),
        "Char" | "kotlin.Char" => Some("java.lang.Character"),
        "Byte" | "kotlin.Byte" => Some("java.lang.Byte"),
        "Short" | "kotlin.Short" => Some("java.lang.Short"),
        "String" | "kotlin.String" => Some("java.lang.String"),
        "List" | "kotlin.collections.List" => Some("java.util.List"),
        "MutableList" | "kotlin.collections.MutableList" => Some("java.util.ArrayList"),
        "Map" | "kotlin.collections.Map" => Some("java.util.Map"),
        "MutableMap" | "kotlin.collections.MutableMap" => Some("java.util.HashMap"),
        "Set" | "kotlin.collections.Set" => Some("java.util.Set"),
        "MutableSet" | "kotlin.collections.MutableSet" => Some("java.util.HashSet"),
        "Any" | "kotlin.Any" => Some("java.lang.Object"),
        "StringBuilder" | "kotlin.text.StringBuilder" => Some("java.lang.StringBuilder"),
        "Array" | "kotlin.Array" => Some("java.lang.Object"),
        "Regex" | "kotlin.text.Regex" => Some("java.util.regex.Pattern"),
        "System" => Some("java.lang.System"),
        "Math" => Some("java.lang.Math"),
        "Thread" => Some("java.lang.Thread"),
        "Runtime" => Some("java.lang.Runtime"),
        "Integer" => Some("java.lang.Integer"),
        "Character" => Some("java.lang.Character"),
        _ => None,
    }
}

/// Scan a directory tree for `.class` files and return fully-qualified
/// class names.
pub(crate) fn scan_dir_classes(dir: &std::path::Path) -> Vec<String> {
    let mut classes = Vec::new();
    fn walk(dir: &std::path::Path, prefix: &str, classes: &mut Vec<String>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() {
                    let new_prefix = if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}.{name}")
                    };
                    walk(&path, &new_prefix, classes);
                } else if name.ends_with(".class") && !name.contains('$') {
                    if let Some(stem) = name.strip_suffix(".class") {
                        let class_name = if prefix.is_empty() {
                            stem.to_string()
                        } else {
                            format!("{prefix}.{stem}")
                        };
                        if !class_name.ends_with("-info") {
                            classes.push(class_name);
                        }
                    }
                }
            }
        }
    }
    walk(dir, "", &mut classes);
    classes
}

/// Expand a leading `~/` to the user's home directory.
pub(crate) fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(rest)
    } else if path == "~" {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
    } else {
        PathBuf::from(path)
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_val_with_type() {
        let (name, ty) = parse_local_decl("val x: Int = 42").unwrap();
        assert_eq!(name, "x");
        assert_eq!(ty.as_deref(), Some("Int"));
    }

    #[test]
    fn parse_val_inferred_string() {
        let (name, ty) = parse_local_decl(r#"val s = "hello""#).unwrap();
        assert_eq!(name, "s");
        assert_eq!(ty.as_deref(), Some("kotlin.String"));
    }

    #[test]
    fn parse_val_inferred_int() {
        let (name, ty) = parse_local_decl("val n = 42").unwrap();
        assert_eq!(name, "n");
        assert_eq!(ty.as_deref(), Some("kotlin.Int"));
    }

    #[test]
    fn parse_val_constructor() {
        let (name, ty) = parse_local_decl("val p = Point(1, 2)").unwrap();
        assert_eq!(name, "p");
        assert_eq!(ty.as_deref(), Some("Point"));
    }

    #[test]
    fn parse_val_list_of() {
        let (name, ty) = parse_local_decl("val xs = listOf(1, 2, 3)").unwrap();
        assert_eq!(name, "xs");
        assert_eq!(ty.as_deref(), Some("kotlin.collections.List"));
    }

    #[test]
    fn parse_top_class() {
        let (name, kind) = parse_top_decl("data class Point(val x: Int, val y: Int)").unwrap();
        assert_eq!(name, "Point");
        assert_eq!(kind, "class");
    }

    #[test]
    fn parse_top_fun() {
        let (name, kind) = parse_top_decl("fun greet(name: String) = println(name)").unwrap();
        assert_eq!(name, "greet");
        assert_eq!(kind, "fun");
    }

    #[test]
    fn parse_top_object() {
        let (name, kind) = parse_top_decl("object Singleton { }").unwrap();
        assert_eq!(name, "Singleton");
        assert_eq!(kind, "object");
    }

    #[test]
    fn extract_word_basic() {
        let (w, s) = extract_word_before("println(myVar.toStr", 19);
        assert_eq!(w, "toStr");
        assert_eq!(s, 14);
    }

    #[test]
    fn extract_dotted_basic() {
        // `java.util` extracted as a single dotted token.
        let (w, s) = extract_dotted_word_before("java.util", 9);
        assert_eq!(w, "java.util");
        assert_eq!(s, 0);
    }

    #[test]
    fn extract_dotted_after_space() {
        // Only the dotted part after the space.
        let (w, s) = extract_dotted_word_before("val x = java.util", 17);
        assert_eq!(w, "java.util");
        assert_eq!(s, 8);
    }

    #[test]
    fn extract_dotted_strips_leading_dot() {
        // After a non-identifier like `"hello".`, the leading dot
        // should be stripped so we don't return `.method`.
        let (w, s) = extract_dotted_word_before("\"hello\".", 8);
        assert_eq!(w, "");
        assert_eq!(s, 8);
    }

    /// Build a completer pre-seeded with FQN class names so the
    /// qualified-completion tests don't depend on a JVM.
    fn seeded_completer() -> SkotchCompleter {
        let mut ctx = CompletionCtx::new();
        ctx.add_extra_classes(vec![
            "java.lang.String".into(),
            "java.lang.System".into(),
            "java.lang.StringBuilder".into(),
            "java.lang.Integer".into(),
            "java.lang.Object".into(),
            "java.util.List".into(),
            "java.util.HashMap".into(),
            "java.util.HashSet".into(),
            "java.util.concurrent.Future".into(),
            "java.util.stream.Stream".into(),
            "java.io.File".into(),
            "java.net.URL".into(),
            "java.time.Instant".into(),
        ]);
        SkotchCompleter {
            ctx: Arc::new(Mutex::new(ctx)),
            verbose: false,
        }
    }

    #[test]
    fn qualified_completion_java_dot() {
        let mut c = seeded_completer();
        let results = c.complete("java.", 5);
        let values: Vec<&str> = results.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"java.lang"),
            "expected java.lang in {values:?}"
        );
        assert!(
            values.contains(&"java.util"),
            "expected java.util in {values:?}"
        );
        assert!(
            values.contains(&"java.io"),
            "expected java.io in {values:?}"
        );
        assert!(
            values.contains(&"java.net"),
            "expected java.net in {values:?}"
        );
        assert!(
            values.contains(&"java.time"),
            "expected java.time in {values:?}"
        );
    }

    #[test]
    fn qualified_completion_java_util_dot() {
        let mut c = seeded_completer();
        let results = c.complete("java.util.", 10);
        let values: Vec<&str> = results.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"java.util.HashMap"),
            "expected HashMap in {values:?}"
        );
        assert!(
            values.contains(&"java.util.List"),
            "expected List in {values:?}"
        );
        // Sub-packages should also appear.
        assert!(
            values.contains(&"java.util.concurrent"),
            "expected concurrent in {values:?}"
        );
        assert!(
            values.contains(&"java.util.stream"),
            "expected stream in {values:?}"
        );
    }

    #[test]
    fn qualified_completion_prefix_filter() {
        let mut c = seeded_completer();
        let results = c.complete("java.lang.S", 11);
        let values: Vec<&str> = results.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&"java.lang.String"), "{values:?}");
        assert!(values.contains(&"java.lang.System"), "{values:?}");
        assert!(values.contains(&"java.lang.StringBuilder"), "{values:?}");
        // Integer should NOT appear.
        assert!(!values.contains(&"java.lang.Integer"), "{values:?}");
    }

    #[test]
    fn kotlin_type_mapping() {
        assert_eq!(
            kotlin_to_java_class("kotlin.String"),
            Some("java.lang.String")
        );
        assert_eq!(kotlin_to_java_class("List<Int>"), Some("java.util.List"));
        assert_eq!(kotlin_to_java_class("SomethingCustom"), None);
    }

    #[test]
    fn tilde_expansion() {
        let p = expand_tilde("~/foo/bar.jar");
        // On Windows join uses `\`, on Unix `/`.
        assert!(p.ends_with("foo/bar.jar") || p.ends_with("foo\\bar.jar"));
        assert!(!p.to_string_lossy().starts_with("~/"));

        let p2 = expand_tilde("/absolute/path.jar");
        assert_eq!(p2, PathBuf::from("/absolute/path.jar"));
    }
}
