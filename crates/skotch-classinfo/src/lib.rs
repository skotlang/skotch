//! Minimal `.class` file parser for extracting method and field signatures.
//!
//! Reads just enough of the class file format to build a registry of
//! available methods for Java interop — constant pool, field_info,
//! method_info, access flags, and the `Signature` attribute (which
//! carries the unerased generic type info). Does NOT parse bytecode
//! or annotations.

pub mod generic_signature;
pub mod kotlin_metadata;

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Process-wide cache of `(class_path → Option<ClassInfo>)` lookups so
/// `lookup_method_descriptor` doesn't re-open every JAR in CLASSPATH on
/// every call. Populated lazily on first miss and on `preload_*` calls.
/// `None` marks a known-absent class so we don't repeatedly re-scan.
static CLASSPATH_CACHE: Mutex<Option<HashMap<String, Option<ClassInfo>>>> = Mutex::new(None);

/// Process-wide cache of open ZIP archives keyed by absolute path. The
/// underlying `ZipArchive::new` call parses the entire central directory
/// (10s of MB for `java.base.jmod`), and a single MIR-lower run does
/// hundreds of class lookups — keeping each archive parsed once shaves
/// hundreds of ms off the cold compile path. The `File` handles stay
/// open for the life of the process; the OS reclaims them at exit.
static ARCHIVE_CACHE: Mutex<Option<HashMap<PathBuf, zip::ZipArchive<std::fs::File>>>> =
    Mutex::new(None);

/// Open `entry_path` from `archive_path`, reusing a previously-parsed
/// `ZipArchive` central directory when available. The caller must accept
/// that the returned bytes are owned (a copy of the entry contents) —
/// passing a borrowed `ZipFile` back out would require either GAT-style
/// lifetimes or a closure-based API; copying the entry bytes is cheaper
/// than the central-directory reparse it avoids.
fn read_archive_entry(archive_path: &Path, entry_path: &str) -> io::Result<Vec<u8>> {
    let mut guard = ARCHIVE_CACHE
        .lock()
        .map_err(|_| io::Error::other("ARCHIVE_CACHE mutex poisoned"))?;
    let cache = guard.get_or_insert_with(HashMap::new);
    if !cache.contains_key(archive_path) {
        let file = std::fs::File::open(archive_path)?;
        let archive = zip::ZipArchive::new(file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        cache.insert(archive_path.to_path_buf(), archive);
    }
    let archive = cache
        .get_mut(archive_path)
        .expect("just inserted if missing");
    let mut entry = archive.by_name(entry_path).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{entry_path} not in {}", archive_path.display()),
        )
    })?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Process-wide memo of [`load_jdk_class`] results (`class_path →
/// Option<ClassInfo>`). The underlying search opens `java.base.jmod`, then
/// iterates *every* jmod, then CLASSPATH jars, then the Kotlin stdlib jars —
/// re-reading each archive's central directory from scratch. MIR-lowering of
/// a Compose-heavy file resolves thousands of method descriptors, and for the
/// many classes that aren't in the JDK at all (Compose/AndroidX), the entire
/// futile search repeated per call. This made JetChat-scale builds take 10+
/// minutes. Caching both hits and misses (the classpath is fixed for a build)
/// bounds the search to once per distinct `class_path`.
static JDK_CLASS_CACHE: Mutex<Option<HashMap<String, Option<Arc<ClassInfo>>>>> = Mutex::new(None);

/// Pre-populate the classpath cache from a set of JAR paths. Idempotent —
/// existing entries aren't overwritten. Call this once per project build
/// after dep resolution; subsequent `lookup_method_descriptor` calls hit
/// the in-memory cache instead of re-opening JARs.
pub fn preload_classpath_cache(jars: &[PathBuf]) {
    let scanned = scan_jars(jars);
    if let Ok(mut guard) = CLASSPATH_CACHE.lock() {
        let cache = guard.get_or_insert_with(HashMap::new);
        for (k, v) in scanned {
            cache.entry(k).or_insert(Some(v));
        }
    }
}

fn cached_class_lookup(class_path: &str) -> Option<ClassInfo> {
    // Fast path: cache hit (either Some(ci) or known-absent None).
    if let Ok(guard) = CLASSPATH_CACHE.lock() {
        if let Some(cache) = guard.as_ref() {
            if let Some(slot) = cache.get(class_path) {
                return slot.clone();
            }
        }
    }
    // Slow path: scan CLASSPATH once, populate cache. Uses the shared
    // archive cache so repeated lookups against the same JAR don't
    // re-parse its central directory.
    let cp = std::env::var("CLASSPATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    let entry_name = format!("{class_path}.class");
    let mut found: Option<ClassInfo> = None;
    for jar_path in cp.split(sep) {
        if jar_path.is_empty() {
            continue;
        }
        let path = std::path::Path::new(jar_path);
        if !path.exists() {
            continue;
        }
        if let Ok(bytes) = read_archive_entry(path, &entry_name) {
            if let Ok(ci) = parse_class(&bytes) {
                found = Some(ci);
                break;
            }
        }
    }
    if let Ok(mut guard) = CLASSPATH_CACHE.lock() {
        let cache = guard.get_or_insert_with(HashMap::new);
        cache.insert(class_path.to_string(), found.clone());
    }
    found
}

/// Information about a single Java class.
#[derive(Clone, Debug)]
pub struct ClassInfo {
    /// JVM internal name, e.g. "java/lang/System".
    pub name: String,
    /// JVM internal name of the superclass, e.g. "java/lang/Object".
    pub super_class: Option<String>,
    /// Access flags (ACC_PUBLIC, ACC_STATIC, etc.).
    pub access_flags: u16,
    /// Methods in this class.
    pub methods: Vec<MethodInfo>,
    /// Fields in this class.
    pub fields: Vec<FieldInfo>,
    /// Decoded `@kotlin.Metadata` annotation, if the class carries one
    /// (i.e. it was compiled by kotlinc). Present for Kotlin classes and
    /// file facades; `None` for plain Java classes. Lets the inferrer
    /// recover Kotlin-level type facts the JVM signature erases.
    pub metadata: Option<kotlin_metadata::RawMetadata>,
}

/// A method signature.
#[derive(Clone, Debug)]
pub struct MethodInfo {
    pub name: String,
    pub descriptor: String,
    pub access_flags: u16,
    /// The JVM `Signature` attribute (JVMS §4.7.9), present on
    /// generic methods. For `<T> List<T> listOf(T...)` this carries
    /// the unerased string `<T:Ljava/lang/Object;>([TT;)Ljava/util/List<TT;>;`
    /// — the descriptor would only show the erased
    /// `([Ljava/lang/Object;)Ljava/util/List;`. The type inferrer
    /// reads this to recover T from arg types and substitute it in
    /// the result, replacing what used to be a hard-coded match on
    /// known stdlib method names. `None` for non-generic methods,
    /// or methods compiled without the attribute (e.g. plain Java
    /// stdlib before any `<T>`).
    pub signature: Option<String>,
}

/// A field signature.
#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub descriptor: String,
    pub access_flags: u16,
    /// The JVM `Signature` attribute on the field. Present when the
    /// field's declared type carries generic args, e.g. a Kotlin
    /// property `val xs: List<Foo>` whose erased descriptor is
    /// `Ljava/util/List;` but whose signature is
    /// `Ljava/util/List<LFoo;>;`. Used so the inferrer can recover
    /// the element type of cross-file fields.
    pub signature: Option<String>,
}

const ACC_PUBLIC: u16 = 0x0001;
const ACC_STATIC: u16 = 0x0008;
const ACC_INTERFACE: u16 = 0x0200;
const ACC_ABSTRACT: u16 = 0x0400;

impl ClassInfo {
    pub fn is_interface(&self) -> bool {
        self.access_flags & ACC_INTERFACE != 0
    }
    pub fn is_abstract(&self) -> bool {
        self.access_flags & ACC_ABSTRACT != 0
    }
}

impl MethodInfo {
    pub fn is_static(&self) -> bool {
        self.access_flags & ACC_STATIC != 0
    }
    pub fn is_public(&self) -> bool {
        self.access_flags & ACC_PUBLIC != 0
    }
}

impl FieldInfo {
    pub fn is_static(&self) -> bool {
        self.access_flags & ACC_STATIC != 0
    }
}

/// Check if a JVM class is an interface by loading its classfile and
/// checking the ACC_INTERFACE flag. Returns `None` if the class can't
/// be loaded.
pub fn check_is_interface(class_path: &str) -> Option<bool> {
    load_jdk_class(class_path).ok().map(|ci| ci.is_interface())
}

/// Look up a method's JVM descriptor from the classpath (JDK + loaded JARs).
/// Returns the descriptor string like `"()Z"` or `"(Ljava/lang/String;)V"`.
pub fn lookup_method_descriptor(
    class_path: &str,
    method_name: &str,
    param_count: usize,
) -> Option<String> {
    // Try the CLASSPATH cache first (preloaded dep JARs + memoized misses).
    // Previously this re-opened every JAR in CLASSPATH on every call, which
    // made jetchat-scale builds stall (~129 JARs × thousands of method
    // lookups during MIR-lower = millions of ZIP opens).
    //
    // The match also accepts `<name>-XXXXXXX` (Kotlin inline-class name
    // mangling). E.g. `Measurable.measure(Constraints)Placeable` is
    // emitted by kotlinc as `measure-BRTryo0(J)Placeable` because
    // `Constraints` is a value class. Skotch's source-level lookup uses
    // the unmangled `measure`; without the prefix fallback the lookup
    // returns None and the call site falls back to a `()V` descriptor,
    // breaking everything that consumes the return value (like
    // `textPlaceable = measurable.measure(constraints)` in JetChat's
    // BaselineHeightModifier).
    if let Some(ci) = cached_class_lookup(class_path) {
        // Two-pass: prefer exact `m.name == method_name` matches. Only
        // fall back to mangled (`<stem>-<hash>` or `<stem>-<hash>$default`)
        // when no exact match exists. A single combined pass causes
        // false positives when BOTH unmangled `Font$default` AND mangled
        // `Font-<hash>$default` live on the same facade — iteration
        // order picks the wrong overload and the call emits a descriptor
        // whose arg types don't match the call site (observed as a
        // TypographyKt VerifyError in iter-22 when the suffix-mangled
        // match was first added).
        let mut best: Option<String> = None;
        for m in &ci.methods {
            if m.name == method_name
                && count_descriptor_params(&m.descriptor) == param_count
                && (best.is_none() || has_object_params(&m.descriptor))
            {
                best = Some(m.descriptor.clone());
            }
        }
        if best.is_some() {
            return best;
        }
        for m in &ci.methods {
            if matches_mangled(&m.name, method_name)
                && count_descriptor_params(&m.descriptor) == param_count
                && (best.is_none() || has_object_params(&m.descriptor))
            {
                best = Some(m.descriptor.clone());
            }
        }
        if best.is_some() {
            return best;
        }
    }
    // Try JDK classes.
    if let Ok(ci) = load_jdk_class(class_path) {
        let mut best: Option<&str> = None;
        for m in &ci.methods {
            if m.name == method_name
                && count_descriptor_params(&m.descriptor) == param_count
                && (best.is_none() || has_object_params(&m.descriptor))
            {
                best = Some(&m.descriptor);
            }
        }
        if let Some(d) = best {
            return Some(d.to_string());
        }
        for m in &ci.methods {
            if matches_mangled(&m.name, method_name)
                && count_descriptor_params(&m.descriptor) == param_count
                && (best.is_none() || has_object_params(&m.descriptor))
            {
                best = Some(&m.descriptor);
            }
        }
        if let Some(d) = best {
            return Some(d.to_string());
        }
    }
    None
}

/// Look up the parsed generic signature for a method, if present in
/// its classfile. Returns `None` for non-generic methods (no
/// `Signature` attribute) or when the class can't be loaded. Resolved
/// against the same lookup order as [`lookup_method_descriptor`] —
/// classpath cache first, then JDK jmods.
///
/// When `Some`, the result is suitable for feeding into
/// [`generic_signature::infer_return_ty`] alongside the call-site
/// arg types to recover the call's return Ty without enumerating
/// the target method's name.
/// Return `(name, descriptor)` for every method on the named class,
/// from the classpath cache first then the JDK registry. Used by
/// mir-lower's external-Kt-facade dispatch (#358) to find Kotlin
/// name-mangled `$default` synthetics whose JVM name carries an inline
/// value-class hash sitting BETWEEN the source-level stem and the
/// `$default` suffix (e.g. `Text-Nvy7gAk$default` vs. the source-level
/// `Text$default`). The fixed-name `lookup_method_descriptor` can't
/// match those — and a blanket suffix-aware `matches_mangled` is
/// unsafe because overloaded mangled forms differ in their param types
/// (iter 22/23 hit a TypographyKt VerifyError when an arity-only
/// fallback picked `Font-<resId_hash>$default` for a `Font(GoogleFont,
/// ...)` call). With this iterator the caller can apply arg-type-aware
/// overload resolution at the call site, where the source-level arg
/// types are available.
pub fn iter_class_methods(class_path: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(ci) = cached_class_lookup(class_path) {
        for m in &ci.methods {
            out.push((m.name.clone(), m.descriptor.clone()));
        }
        if !out.is_empty() {
            return out;
        }
    }
    if let Ok(ci) = load_jdk_class(class_path) {
        for m in &ci.methods {
            out.push((m.name.clone(), m.descriptor.clone()));
        }
        if !out.is_empty() {
            return out;
        }
    }
    // Direct CLASSPATH scan when the class isn't in the preloaded cache
    // or the JDK registry. The same Compose facades that
    // `find_wrapper_class_for_function` finds via direct JAR scan
    // (`TextKt`, `IconKt`, `SurfaceKt`, etc.) are NOT in the classinfo
    // cache — `cached_class_lookup` returns None and the iterator
    // previously returned an empty Vec, so the arg-type-aware
    // mangled-`$default` fallback in `lower_expr` couldn't enumerate the
    // candidates that actually live on the class. Mirror the JAR-scan
    // approach from `find_wrapper_class_for_function` here so the
    // iterator finds the same classes.
    let target = format!("{class_path}.class");
    let cp = std::env::var("CLASSPATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    for jar_path in cp.split(sep) {
        if jar_path.is_empty() {
            continue;
        }
        let path = std::path::Path::new(jar_path);
        if !path.exists() {
            continue;
        }
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let Ok(mut archive) = zip::ZipArchive::new(file) else {
            continue;
        };
        let Ok(mut entry) = archive.by_name(&target) else {
            continue;
        };
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(&mut entry, &mut bytes).is_err() {
            continue;
        }
        let Ok(ci) = parse_class(&bytes) else {
            continue;
        };
        for m in &ci.methods {
            out.push((m.name.clone(), m.descriptor.clone()));
        }
        if !out.is_empty() {
            return out;
        }
    }
    out
}

pub fn lookup_method_signature(
    class_path: &str,
    method_name: &str,
    param_count: usize,
) -> Option<generic_signature::MethodSignature> {
    let raw = lookup_method_signature_raw(class_path, method_name, param_count)?;
    generic_signature::parse_method_signature(&raw)
}

fn lookup_method_signature_raw(
    class_path: &str,
    method_name: &str,
    param_count: usize,
) -> Option<String> {
    if let Some(ci) = cached_class_lookup(class_path) {
        for m in &ci.methods {
            if (m.name == method_name || matches_mangled(&m.name, method_name))
                && count_descriptor_params(&m.descriptor) == param_count
            {
                if let Some(sig) = &m.signature {
                    return Some(sig.clone());
                }
            }
        }
    }
    if let Ok(ci) = load_jdk_class(class_path) {
        for m in &ci.methods {
            if (m.name == method_name || matches_mangled(&m.name, method_name))
                && count_descriptor_params(&m.descriptor) == param_count
            {
                if let Some(sig) = &m.signature {
                    return Some(sig.clone());
                }
            }
        }
    }
    None
}

/// Recover the `@kotlin.Metadata` description of a function on an
/// already-loaded class, by source-level name. Returns the first match
/// (overloads share a name — refine with arity at the call site if
/// needed). `None` when the class carries no `@Metadata` (e.g. plain
/// Java) or declares no such function.
pub fn class_function_metadata(
    ci: &ClassInfo,
    function_name: &str,
) -> Option<kotlin_metadata::FunctionInfo> {
    let cm = kotlin_metadata::parse_metadata(ci.metadata.as_ref()?)?;
    cm.functions.into_iter().find(|f| f.name == function_name)
}

/// Resolve a function's `@kotlin.Metadata` from the classpath
/// (cache → JDK/stdlib jars) — the Kotlin-level counterpart of
/// [`lookup_method_signature`]. Recovers facts the erased JVM signature
/// drops: receiver-ness (`T.() -> R` vs `(T) -> R`), nullability, and
/// parameter names. `None` for non-Kotlin classes or absent functions.
pub fn lookup_function_metadata(
    class_path: &str,
    function_name: &str,
) -> Option<kotlin_metadata::FunctionInfo> {
    if let Some(ci) = cached_class_lookup(class_path) {
        if let Some(f) = class_function_metadata(&ci, function_name) {
            return Some(f);
        }
    }
    if let Ok(ci) = load_jdk_class(class_path) {
        if let Some(f) = class_function_metadata(&ci, function_name) {
            return Some(f);
        }
    }
    None
}

/// Look up a method allowing mangled matches, returning both the actual
/// JVM method name (which may be mangled like `measure-BRTryo0`) and the
/// descriptor. Useful when the caller needs to emit an invokevirtual with
/// the real method name. Wraps `lookup_method_descriptor` semantics.
pub fn lookup_method_name_and_descriptor(
    class_path: &str,
    method_name: &str,
    param_count: usize,
) -> Option<(String, String)> {
    let scan = |ci: &ClassInfo| -> Option<(String, String)> {
        let mut best: Option<(String, String)> = None;
        for m in &ci.methods {
            if (m.name == method_name || matches_mangled(&m.name, method_name))
                && count_descriptor_params(&m.descriptor) == param_count
                && (best.is_none() || has_object_params(&m.descriptor))
            {
                best = Some((m.name.clone(), m.descriptor.clone()));
            }
        }
        best
    };
    if let Some(ci) = cached_class_lookup(class_path) {
        if let Some(r) = scan(&ci) {
            return Some(r);
        }
    }
    if let Ok(ci) = load_jdk_class(class_path) {
        if let Some(r) = scan(&ci) {
            return Some(r);
        }
    }
    None
}

/// Whether `mangled` looks like a kotlinc-mangled form of `unmangled`:
/// `unmangled-XXXXXXX` where `XXXXXXX` is a 7-char hash. Kotlin's hash
/// alphabet includes ASCII alphanumeric chars AND `_` — names like
/// `getWhite-0d7_KjU` (Compose's `Color.Companion.White` getter) MUST
/// match against the source-level `getWhite`. Used both for value-class
/// method calls (`measure-BRTryo0` ↔ `measure`) and for companion-
/// property getters returning a value-class type.
fn matches_mangled(mangled: &str, unmangled: &str) -> bool {
    if let Some(rest) = mangled
        .strip_prefix(unmangled)
        .and_then(|r| r.strip_prefix('-'))
    {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    } else {
        false
    }
}

/// Look up the descriptor of a static field on a class. Used by MIR
/// lowering to detect imports of enum constants (e.g.
/// `import androidx.compose.material3.DrawerValue.Closed` — `Closed` is
/// a static field of type `LDrawerValue;` on the enum class). Returns
/// `None` if the field doesn't exist or isn't static.
pub fn lookup_static_field_descriptor(class_path: &str, field_name: &str) -> Option<String> {
    if let Some(ci) = cached_class_lookup(class_path) {
        for f in &ci.fields {
            if f.name == field_name && f.is_static() {
                return Some(f.descriptor.clone());
            }
        }
    }
    if let Ok(ci) = load_jdk_class(class_path) {
        for f in &ci.fields {
            if f.name == field_name && f.is_static() {
                return Some(f.descriptor.clone());
            }
        }
    }
    None
}

/// Find which wrapper class (e.g. `ComposablesKt`) in a package contains
/// a given static method. Searches all `*Kt.class` files in the package
/// within the classpath JARs.
pub fn find_wrapper_class_for_function(package_path: &str, method_name: &str) -> Option<String> {
    let cp = std::env::var("CLASSPATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    let prefix = format!("{package_path}/");

    for jar_path in cp.split(sep) {
        if jar_path.is_empty() {
            continue;
        }
        let path = std::path::Path::new(jar_path);
        if !path.exists() {
            continue;
        }
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(mut archive) = zip::ZipArchive::new(file) {
                // Collect Kt class names in this package.
                let kt_classes: Vec<String> = (0..archive.len())
                    .filter_map(|i| {
                        archive.by_index(i).ok().and_then(|e| {
                            let name = e.name().to_string();
                            if name.starts_with(&prefix)
                                && name.ends_with("Kt.class")
                                && !name[prefix.len()..].contains('/')
                            {
                                Some(name)
                            } else {
                                None
                            }
                        })
                    })
                    .collect();

                for class_file in &kt_classes {
                    if let Ok(mut entry) = archive.by_name(class_file) {
                        let mut bytes = Vec::new();
                        if std::io::Read::read_to_end(&mut entry, &mut bytes).is_ok() {
                            if let Ok(ci) = parse_class(&bytes) {
                                // Also accept kotlinc-mangled names like
                                // `Font-wCLgNak` for the source-level `Font`
                                // — value-class returns produce mangled
                                // suffixes. Without this, `GoogleFontKt`
                                // (which only has `Font-wCLgNak` and
                                // `Font-wCLgNak$default`) is missed and the
                                // resolver falls back to a non-existent
                                // `Font.Font(...)` call that crashes at
                                // load-time with NoClassDefFoundError.
                                if ci.methods.iter().any(|m| {
                                    (m.name == method_name || matches_mangled(&m.name, method_name))
                                        && m.is_static()
                                        && m.is_public()
                                }) {
                                    let class_name =
                                        class_file.strip_suffix(".class").unwrap_or(class_file);
                                    return Some(class_name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Count the number of parameters in a JVM method descriptor.
/// E.g. `(Landroid/content/Context;)V` → 1, `(II)V` → 2.
pub fn count_descriptor_params_pub(desc: &str) -> usize {
    count_descriptor_params(desc)
}

/// Check if a descriptor has any object (L...;) parameters.
fn has_object_params(desc: &str) -> bool {
    let inner = desc
        .strip_prefix('(')
        .and_then(|s| s.split(')').next())
        .unwrap_or("");
    inner.contains('L')
}

fn count_descriptor_params(desc: &str) -> usize {
    let inner = desc
        .strip_prefix('(')
        .and_then(|s| s.split(')').next())
        .unwrap_or("");
    let mut count = 0;
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'L' => {
                count += 1;
                while i < bytes.len() && bytes[i] != b';' {
                    i += 1;
                }
                i += 1;
            }
            b'[' => {
                i += 1; // skip array prefix, next iteration counts the element
            }
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' => {
                count += 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    count
}

/// Parse a `.class` file from raw bytes.
pub fn parse_class(bytes: &[u8]) -> io::Result<ClassInfo> {
    let mut r = Reader { bytes, pos: 0 };

    let magic = r.u32()?;
    if magic != 0xCAFEBABE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a class file",
        ));
    }
    let _minor = r.u16()?;
    let _major = r.u16()?;

    // Constant pool.
    let cp_count = r.u16()? as usize;
    let mut cp = vec![CpEntry::Empty; cp_count];
    let mut i = 1;
    while i < cp_count {
        let tag = r.u8()?;
        match tag {
            1 => {
                // CONSTANT_Utf8 — keep raw bytes; decode lazily.
                let len = r.u16()? as usize;
                let raw = r.bytes[r.pos..r.pos + len].to_vec();
                r.pos += len;
                cp[i] = CpEntry::Utf8(raw);
            }
            3 => {
                // CONSTANT_Integer
                let v = r.u32()? as i32;
                cp[i] = CpEntry::Int(v);
            }
            4 => {
                r.pos += 4;
                cp[i] = CpEntry::Other;
            } // Float
            5 => {
                r.pos += 8;
                cp[i] = CpEntry::Other;
                i += 1;
            } // Long (takes 2 slots)
            6 => {
                r.pos += 8;
                cp[i] = CpEntry::Other;
                i += 1;
            } // Double (takes 2 slots)
            7 => {
                // CONSTANT_Class
                let name_idx = r.u16()? as usize;
                cp[i] = CpEntry::Class(name_idx);
            }
            8 => {
                r.pos += 2;
                cp[i] = CpEntry::Other;
            } // String
            9..=11 => {
                r.pos += 4;
                cp[i] = CpEntry::Other;
            } // Fieldref/Methodref/InterfaceMethodref
            12 => {
                // CONSTANT_NameAndType
                let name_idx = r.u16()? as usize;
                let desc_idx = r.u16()? as usize;
                cp[i] = CpEntry::NameAndType(name_idx, desc_idx);
            }
            15 => {
                r.pos += 3;
                cp[i] = CpEntry::Other;
            } // MethodHandle
            16 => {
                r.pos += 2;
                cp[i] = CpEntry::Other;
            } // MethodType
            17 => {
                r.pos += 4;
                cp[i] = CpEntry::Other;
            } // Dynamic
            18 => {
                r.pos += 4;
                cp[i] = CpEntry::Other;
            } // InvokeDynamic
            19 => {
                r.pos += 2;
                cp[i] = CpEntry::Other;
            } // Module
            20 => {
                r.pos += 2;
                cp[i] = CpEntry::Other;
            } // Package
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown cp tag {tag}"),
                ))
            }
        }
        i += 1;
    }

    let access_flags = r.u16()?;
    let this_class = r.u16()? as usize;
    let super_class_idx = r.u16()? as usize;

    // Class name.
    let class_name = resolve_class(&cp, this_class);

    // Superclass name (0 means java/lang/Object itself).
    let super_class = if super_class_idx == 0 {
        None
    } else {
        let name = resolve_class(&cp, super_class_idx);
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    };

    // Interfaces.
    let iface_count = r.u16()? as usize;
    r.pos += iface_count * 2;

    // Fields.
    let field_count = r.u16()? as usize;
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let f_access = r.u16()?;
        let f_name_idx = r.u16()? as usize;
        let f_desc_idx = r.u16()? as usize;
        let f_attrs = r.u16()? as usize;
        let mut f_signature: Option<String> = None;
        for _ in 0..f_attrs {
            let attr_name_idx = r.u16()? as usize;
            let attr_len = r.u32()? as usize;
            let attr_name = resolve_utf8(&cp, attr_name_idx);
            // `Signature` attribute body: u2 index into the constant
            // pool pointing at a Utf8 entry. JVMS §4.7.9.
            if attr_name == "Signature" && attr_len == 2 {
                let sig_idx = r.u16()? as usize;
                f_signature = Some(resolve_utf8(&cp, sig_idx));
            } else {
                r.pos += attr_len;
            }
        }
        fields.push(FieldInfo {
            name: resolve_utf8(&cp, f_name_idx),
            descriptor: resolve_utf8(&cp, f_desc_idx),
            access_flags: f_access,
            signature: f_signature,
        });
    }

    // Methods.
    let method_count = r.u16()? as usize;
    let mut methods = Vec::with_capacity(method_count);
    for _ in 0..method_count {
        let m_access = r.u16()?;
        let m_name_idx = r.u16()? as usize;
        let m_desc_idx = r.u16()? as usize;
        let m_attrs = r.u16()? as usize;
        let mut m_signature: Option<String> = None;
        for _ in 0..m_attrs {
            let attr_name_idx = r.u16()? as usize;
            let attr_len = r.u32()? as usize;
            let attr_name = resolve_utf8(&cp, attr_name_idx);
            if attr_name == "Signature" && attr_len == 2 {
                let sig_idx = r.u16()? as usize;
                m_signature = Some(resolve_utf8(&cp, sig_idx));
            } else {
                r.pos += attr_len;
            }
        }
        methods.push(MethodInfo {
            name: resolve_utf8(&cp, m_name_idx),
            descriptor: resolve_utf8(&cp, m_desc_idx),
            access_flags: m_access,
            signature: m_signature,
        });
    }

    // Class-level attributes — scan for `RuntimeVisibleAnnotations`
    // carrying `@kotlin/Metadata`.
    let mut metadata = None;
    if let Ok(class_attr_count) = r.u16() {
        for _ in 0..class_attr_count {
            let attr_name_idx = match r.u16() {
                Ok(n) => n as usize,
                Err(_) => break,
            };
            let attr_len = match r.u32() {
                Ok(l) => l as usize,
                Err(_) => break,
            };
            let attr_name = resolve_utf8(&cp, attr_name_idx);
            let end = (r.pos + attr_len).min(r.bytes.len());
            if attr_name == "RuntimeVisibleAnnotations" {
                metadata = extract_kotlin_metadata(&r.bytes[r.pos..end], &cp);
            }
            r.pos = end;
        }
    }

    Ok(ClassInfo {
        name: class_name,
        super_class,
        access_flags,
        methods,
        fields,
        metadata,
    })
}

/// A parsed `element_value` from a JVMS annotation (§4.7.16.1). Only
/// the shapes `@kotlin.Metadata` uses are kept distinctly; everything
/// else is consumed (so the cursor advances) and reported as `Other`.
enum ElementValue {
    Int(i32),
    Str(String),
    Array(Vec<ElementValue>),
    Other,
}

/// Parse one `element_value`, advancing `r` past it. Returns `None`
/// only on truncation.
fn parse_element_value(r: &mut Reader, cp: &[CpEntry]) -> Option<ElementValue> {
    let tag = r.u8().ok()?;
    match tag {
        b'I' => {
            let idx = r.u16().ok()? as usize;
            Some(ElementValue::Int(resolve_int(cp, idx).unwrap_or(0)))
        }
        b's' => {
            let idx = r.u16().ok()? as usize;
            Some(ElementValue::Str(resolve_utf8(cp, idx)))
        }
        // Other primitive constants (B/C/D/F/J/S/Z) and class refs (c):
        // a single u2 const index we don't need.
        b'B' | b'C' | b'D' | b'F' | b'J' | b'S' | b'Z' | b'c' => {
            r.u16().ok()?;
            Some(ElementValue::Other)
        }
        b'e' => {
            // enum: type_name_index + const_name_index.
            r.u16().ok()?;
            r.u16().ok()?;
            Some(ElementValue::Other)
        }
        b'@' => {
            // Nested annotation: type_index, then name/value pairs.
            r.u16().ok()?;
            let pairs = r.u16().ok()?;
            for _ in 0..pairs {
                r.u16().ok()?;
                parse_element_value(r, cp)?;
            }
            Some(ElementValue::Other)
        }
        b'[' => {
            let n = r.u16().ok()?;
            let mut items = Vec::with_capacity(n as usize);
            for _ in 0..n {
                items.push(parse_element_value(r, cp)?);
            }
            Some(ElementValue::Array(items))
        }
        _ => None,
    }
}

/// Find and decode `@kotlin/Metadata` within a `RuntimeVisibleAnnotations`
/// attribute body.
fn extract_kotlin_metadata(body: &[u8], cp: &[CpEntry]) -> Option<kotlin_metadata::RawMetadata> {
    let mut r = Reader {
        bytes: body,
        pos: 0,
    };
    let num_annotations = r.u16().ok()?;
    for _ in 0..num_annotations {
        let type_idx = r.u16().ok()? as usize;
        let num_pairs = r.u16().ok()?;
        if resolve_utf8(cp, type_idx) == "Lkotlin/Metadata;" {
            let mut kind = 1;
            let mut data1 = Vec::new();
            let mut data2 = Vec::new();
            for _ in 0..num_pairs {
                let name = resolve_utf8(cp, r.u16().ok()? as usize);
                let value = parse_element_value(&mut r, cp)?;
                match (name.as_str(), value) {
                    ("k", ElementValue::Int(v)) => kind = v,
                    ("d1", ElementValue::Array(items)) => data1 = collect_strings(items),
                    ("d2", ElementValue::Array(items)) => data2 = collect_strings(items),
                    _ => {}
                }
            }
            return Some(kotlin_metadata::RawMetadata { kind, data1, data2 });
        }
        // Not @Metadata: skip its pairs.
        for _ in 0..num_pairs {
            r.u16().ok()?;
            parse_element_value(&mut r, cp)?;
        }
    }
    None
}

/// Keep only the string values from an annotation array element.
fn collect_strings(items: Vec<ElementValue>) -> Vec<String> {
    items
        .into_iter()
        .filter_map(|v| match v {
            ElementValue::Str(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Load a class from the JDK's jmod files. Searches java.base first,
/// then all other jmod files in the jmods/ directory.
///
/// The result is held behind an `Arc` so cache hits are a cheap pointer
/// clone — `ClassInfo` carries a few hundred owned `String`s for the
/// constant-pool / methods / fields tables, and a deep clone per hit
/// added millions of allocations to a typical compile.
pub fn load_jdk_class(class_path: &str) -> io::Result<Arc<ClassInfo>> {
    // Memoize: the search below is expensive and MIR-lowering calls it
    // thousands of times for the same handful of classes (and repeats the
    // entire futile search for never-found Compose/AndroidX classes). The
    // classpath/jmods are fixed for a build, so caching hits *and* misses
    // is safe and bounds the cost to one search per distinct class_path.
    if let Ok(guard) = JDK_CLASS_CACHE.lock() {
        if let Some(cache) = guard.as_ref() {
            if let Some(slot) = cache.get(class_path) {
                return match slot {
                    Some(ci) => Ok(Arc::clone(ci)),
                    None => Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "class not found (cached miss)",
                    )),
                };
            }
        }
    }
    // Cheap reject for single-letter / lowercase-led last segments
    // that can't possibly be valid Java class names. The earlier
    // version of this check also rejected ANY unqualified PascalCase
    // name not on a hand-rolled allow-list (`Object`, `String`,
    // `StringBuilder`, …) — but that locked out unqualified user
    // class names (`Item`, `LruCache`) that some lookup paths still
    // pass raw. A user-class lookup of `Item` is rare and fast (the
    // miss is cached after the first walk); the false-rejection
    // of `Item.count` field lookups was a real bug. Keep only the
    // single-letter / non-PascalCase rejection — that covers the
    // common `java/lang/l` / bare `T` leaks without misfiring.
    if is_obviously_not_a_class(class_path) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "name looks like a type parameter or local variable",
        ));
    }
    let result = load_jdk_class_uncached(class_path).map(Arc::new);
    if let Ok(mut guard) = JDK_CLASS_CACHE.lock() {
        let cache = guard.get_or_insert_with(HashMap::new);
        cache.insert(class_path.to_string(), result.as_ref().ok().cloned());
    }
    result
}

fn is_obviously_not_a_class(class_path: &str) -> bool {
    let last_segment = class_path.rsplit('/').next().unwrap_or(class_path);
    // Empty or single-character last segment — `java/lang/l`, bare `T`, …
    if last_segment.len() <= 1 {
        return true;
    }
    // First char of the last segment must be an uppercase ASCII letter or
    // a digit (some JVM-internal names start with `$`/digits, but those
    // also won't be reached by our prepend-java/lang path). Lowercase or
    // symbolic — almost certainly a typo / local-var leak.
    let Some(first) = last_segment.chars().next() else {
        return true;
    };
    if !first.is_ascii_uppercase() && first != '$' {
        return true;
    }
    false
}

fn load_jdk_class_uncached(class_path: &str) -> io::Result<ClassInfo> {
    let entry_path = format!("classes/{class_path}.class");
    let jar_entry = format!("{class_path}.class");
    load_jdk_class_uncached_inner(class_path, &entry_path, &jar_entry)
}

fn load_jdk_class_uncached_inner(
    class_path: &str,
    entry_path: &str,
    jar_entry: &str,
) -> io::Result<ClassInfo> {
    // Path-prefix routing: `kotlin/*` and `kotlinx/*` only live in the
    // Kotlin stdlib JARs, never in the JDK. Walking every jmod first
    // (28 archives × by_name probe) added ~50 ms per first-time
    // kotlin/Pair / kotlin/collections/* lookup. Same shape in reverse:
    // `java/*` and `javax/*` only live in the JDK, not in Kotlin JARs.
    let probably_kotlin = class_path.starts_with("kotlin/") || class_path.starts_with("kotlinx/");
    let probably_jdk = class_path.starts_with("java/") || class_path.starts_with("javax/");

    if !probably_kotlin {
        let jdk_home = find_jdk_home()?;
        let jmods_dir = jdk_home.join("jmods");
        if jmods_dir.exists() {
            // Try java.base.jmod first (most common classes).
            let base_jmod = jmods_dir.join("java.base.jmod");
            if base_jmod.exists() {
                if let Ok(info) = load_class_from_jmod(&base_jmod, entry_path) {
                    return Ok(info);
                }
            }

            // Search all other jmod files.
            if let Ok(entries) = std::fs::read_dir(&jmods_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jmod") {
                        if let Ok(info) = load_class_from_jmod(&path, entry_path) {
                            return Ok(info);
                        }
                    }
                }
            }
        }

        // Also check CLASSPATH for directories and JARs.
        if let Ok(cp) = std::env::var("CLASSPATH") {
            let sep = if cfg!(windows) { ';' } else { ':' };
            for entry in cp.split(sep) {
                let p = Path::new(entry);
                if p.is_dir() {
                    let class_file = p.join(format!("{class_path}.class"));
                    if class_file.exists() {
                        let bytes = std::fs::read(&class_file)?;
                        return parse_class(&bytes);
                    }
                } else if p.extension().and_then(|e| e.to_str()) == Some("jar") && p.exists() {
                    if let Ok(info) = load_class_from_jar(p, jar_entry) {
                        return Ok(info);
                    }
                }
            }
        }
    }

    // Search Kotlin stdlib JARs (skipped for `java/*` paths).
    if !probably_jdk {
        if let Ok(kotlin_libs) = find_kotlin_lib_dir() {
            for jar_name in &[
                "kotlin-stdlib.jar",
                "kotlin-stdlib-jdk8.jar",
                "kotlin-stdlib-jdk7.jar",
            ] {
                let jar = kotlin_libs.join(jar_name);
                if jar.exists() {
                    if let Ok(info) = load_class_from_jar(&jar, jar_entry) {
                        return Ok(info);
                    }
                }
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("class {class_path} not found in JDK, CLASSPATH, or Kotlin stdlib"),
    ))
}

/// Find the directory containing Kotlin stdlib JARs.
///
/// Search order:
/// 1. CLASSPATH — any kotlin-stdlib*.jar on the classpath
/// 2. KOTLIN_HOME environment variable → $KOTLIN_HOME/lib/
/// 3. `kotlin.home` system property (via Java) — skipped (not accessible)
/// 4. Locate `kotlinc` on PATH, resolve symlinks, find lib/ relative to it
pub fn find_kotlin_lib_dir() -> io::Result<PathBuf> {
    // 1. Check CLASSPATH for kotlin-stdlib.jar
    if let Ok(cp) = std::env::var("CLASSPATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for entry in cp.split(sep) {
            let p = Path::new(entry);
            if p.file_name().and_then(|f| f.to_str()) == Some("kotlin-stdlib.jar") && p.exists() {
                if let Some(parent) = p.parent() {
                    return Ok(parent.to_path_buf());
                }
            }
        }
    }

    // 2. Check KOTLIN_HOME environment variable.
    if let Ok(home) = std::env::var("KOTLIN_HOME") {
        let lib = PathBuf::from(&home).join("lib");
        if lib.join("kotlin-stdlib.jar").exists() {
            return Ok(lib);
        }
        // Also check libexec/lib (Homebrew layout).
        let libexec = PathBuf::from(&home).join("libexec").join("lib");
        if libexec.join("kotlin-stdlib.jar").exists() {
            return Ok(libexec);
        }
    }

    // 3. Locate kotlinc on PATH and resolve symlinks.
    let compiler_name = if cfg!(windows) {
        "kotlinc.bat"
    } else {
        "kotlinc"
    };
    if let Ok(kotlinc_path) = which::which(compiler_name) {
        // Try both the original path and the canonicalized (symlink-resolved)
        // path. On Windows, canonicalize prepends \\?\ which can break
        // downstream path operations, so we try the original first.
        let candidates: Vec<PathBuf> = {
            let mut v = vec![kotlinc_path.clone()];
            if let Ok(resolved) = std::fs::canonicalize(&kotlinc_path) {
                if resolved != kotlinc_path {
                    v.push(resolved);
                }
            }
            v
        };
        for candidate in &candidates {
            // kotlinc is typically at $KOTLIN_HOME/bin/kotlinc
            if let Some(bin) = candidate.parent() {
                if let Some(home) = bin.parent() {
                    let lib = home.join("lib");
                    if lib.join("kotlin-stdlib.jar").exists() {
                        return Ok(lib);
                    }
                    let libexec = home.join("libexec").join("lib");
                    if libexec.join("kotlin-stdlib.jar").exists() {
                        return Ok(libexec);
                    }
                }
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Kotlin stdlib not found (set KOTLIN_HOME or add kotlin-stdlib.jar to CLASSPATH)",
    ))
}

fn load_class_from_jmod(jmod_path: &Path, entry_path: &str) -> io::Result<ClassInfo> {
    let bytes = read_archive_entry(jmod_path, entry_path)?;
    parse_class(&bytes)
}

fn load_class_from_jar(jar_path: &Path, entry_path: &str) -> io::Result<ClassInfo> {
    let bytes = read_archive_entry(jar_path, entry_path)?;
    parse_class(&bytes)
}

/// Scan all `.class` entries in a list of JARs and return a map of
/// JVM class name → ClassInfo.
pub fn scan_jars(jars: &[PathBuf]) -> HashMap<String, ClassInfo> {
    let mut map = HashMap::new();
    for jar_path in jars {
        if !jar_path.exists() {
            continue;
        }
        let file = match std::fs::File::open(jar_path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut archive = match zip::ZipArchive::new(file) {
            Ok(a) => a,
            Err(_) => continue,
        };
        for i in 0..archive.len() {
            let mut entry = match archive.by_index(i) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.name().to_string();
            if !name.ends_with(".class") || name.contains("module-info") {
                continue;
            }
            let mut bytes = Vec::new();
            if entry.read_to_end(&mut bytes).is_err() {
                continue;
            }
            if let Ok(info) = parse_class(&bytes) {
                map.insert(info.name.clone(), info);
            }
        }
    }
    map
}

/// Build a class registry from the JDK for commonly-used java.lang classes.
/// A discovered extension function from scanning a Kotlin facade class.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredExtension {
    /// JVM facade class: "kotlin/collections/CollectionsKt"
    pub facade_class: String,
    /// Method name: "map"
    pub method_name: String,
    /// JVM method descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;"
    pub descriptor: String,
    /// Receiver type (first parameter): "Ljava/lang/Iterable;"
    pub receiver_descriptor: String,
    /// Return type descriptor: "Ljava/util/List;"
    pub return_descriptor: String,
}

/// Discover stdlib extensions with disk caching.
/// The cache is stored in `~/.skotch/cache/stdlib-extensions-{hash}.json`
/// and invalidated when kotlin-stdlib.jar changes (by file size).
pub fn discover_stdlib_extensions() -> Vec<DiscoveredExtension> {
    // kotlin-stdlib extensions (disk-cached — the stdlib jar is stable).
    let mut extensions = match load_cached_extensions() {
        Some(cached) => cached,
        None => {
            let e = discover_stdlib_extensions_uncached();
            save_cached_extensions(&e);
            e
        }
    };
    // Plus the project's CLASSPATH dep-jar extensions (kotlinx-coroutines,
    // AndroidX, …). NOT disk-cached: CLASSPATH varies per project/build. The
    // caller (mir-lower's `DISCOVERED_EXTENSIONS`) memoizes the merged set so
    // this one-time scan happens once per build. Without it, calls like
    // `MutableStateFlow.asStateFlow()` can't be resolved to their `*Kt` facade
    // and get null-stubbed (e.g. JetChat's `MainViewModel.drawerShouldBeOpened`).
    extensions.extend(discover_classpath_extensions());
    extensions
}

/// Scan every jar on `CLASSPATH` for extension functions. The build adds the
/// resolved dependency jars to `CLASSPATH` before lowering, so this picks up
/// coroutines/AndroidX/etc. extensions. Empty when `CLASSPATH` is unset (e.g.
/// the stdlib-independent fixture tests), keeping those deterministic.
fn discover_classpath_extensions() -> Vec<DiscoveredExtension> {
    let mut extensions = Vec::new();
    let Ok(cp) = std::env::var("CLASSPATH") else {
        return extensions;
    };
    let sep = if cfg!(windows) { ';' } else { ':' };
    for entry in cp.split(sep) {
        let p = Path::new(entry);
        if p.extension().and_then(|e| e.to_str()) == Some("jar") {
            extensions.extend(scan_jar_for_extensions(p));
        }
    }
    extensions
}

/// Load cached extensions from disk if the cache is valid.
fn load_cached_extensions() -> Option<Vec<DiscoveredExtension>> {
    let kotlin_lib = find_kotlin_lib_dir().ok()?.join("kotlin-stdlib.jar");
    let meta = std::fs::metadata(&kotlin_lib).ok()?;
    let size = meta.len();
    let cache_key = format!("stdlib-extensions-{size}");
    let cache_dir = dirs_cache_dir()?;
    let cache_file = cache_dir.join(format!("{cache_key}.json"));
    let data = std::fs::read_to_string(&cache_file).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save extensions to the disk cache.
fn save_cached_extensions(extensions: &[DiscoveredExtension]) {
    let kotlin_lib = match find_kotlin_lib_dir() {
        Ok(dir) => dir.join("kotlin-stdlib.jar"),
        Err(_) => return,
    };
    let size = std::fs::metadata(&kotlin_lib).map(|m| m.len()).unwrap_or(0);
    let cache_key = format!("stdlib-extensions-{size}");
    if let Some(cache_dir) = dirs_cache_dir() {
        let _ = std::fs::create_dir_all(&cache_dir);
        let cache_file = cache_dir.join(format!("{cache_key}.json"));
        if let Ok(json) = serde_json::to_string(extensions) {
            let _ = std::fs::write(cache_file, json);
        }
    }
}

fn dirs_cache_dir() -> Option<std::path::PathBuf> {
    // ~/.skotch/cache/
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(std::path::PathBuf::from(home).join(".skotch/cache"))
}

/// Uncached scan of kotlin-stdlib.jar for extension functions.
fn discover_stdlib_extensions_uncached() -> Vec<DiscoveredExtension> {
    let kotlin_lib = match find_kotlin_lib_dir() {
        Ok(dir) => dir.join("kotlin-stdlib.jar"),
        Err(_) => return Vec::new(),
    };
    scan_jar_for_extensions(&kotlin_lib)
}

/// Scan one jar's `*Kt` facade classes for extension functions: public static
/// methods whose first parameter is the (extension) receiver. Shared by the
/// kotlin-stdlib discovery and the per-project CLASSPATH dep-jar scan
/// ([`discover_classpath_extensions`]) so e.g. `MutableStateFlow.asStateFlow()`
/// (kotlinx-coroutines `FlowKt`) resolves instead of null-stubbing.
fn scan_jar_for_extensions(jar: &Path) -> Vec<DiscoveredExtension> {
    let mut extensions = Vec::new();
    if !jar.exists() {
        return extensions;
    }
    let file = match std::fs::File::open(jar) {
        Ok(f) => f,
        Err(_) => return extensions,
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(_) => return extensions,
    };
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        // Scan *Kt.class facade classes (not inner classes, not $-suffixed).
        // Include implementation classes like CollectionsKt__CollectionsKt
        // and CollectionsKt___CollectionsKt which contain the actual methods.
        if !name.ends_with(".class") || name.contains('$') {
            continue;
        }
        // Must contain "Kt" in the simple class name.
        let simple = name.trim_end_matches(".class");
        let simple_name = simple.rsplit('/').next().unwrap_or(simple);
        if !simple_name.contains("Kt") {
            continue;
        }
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_err() {
            continue;
        }
        let class_info = match parse_class(&bytes) {
            Ok(ci) => ci,
            Err(_) => continue,
        };
        // Extract public static methods with at least one parameter
        // (the first parameter is the receiver).
        for method in &class_info.methods {
            if !method.is_public() || !method.is_static() {
                continue;
            }
            // Skip constructors, synthetic methods, and $default overloads.
            if method.name.starts_with('<')
                || method.name.contains('$')
                || method.name.starts_with("access$")
            {
                continue;
            }
            // Parse the descriptor to get the first parameter (receiver).
            let params = parse_descriptor_params(&method.descriptor);
            if params.is_empty() {
                continue; // No receiver → not an extension function
            }
            let return_desc = method
                .descriptor
                .rsplit(')')
                .next()
                .unwrap_or("V")
                .to_string();
            // Map internal impl class to public facade:
            // kotlin/collections/CollectionsKt__CollectionsKt →
            // kotlin/collections/CollectionsKt
            let facade = normalize_facade_class(&class_info.name);
            extensions.push(DiscoveredExtension {
                facade_class: facade,
                method_name: method.name.clone(),
                descriptor: method.descriptor.clone(),
                receiver_descriptor: params[0].clone(),
                return_descriptor: return_desc,
            });
        }
    }
    extensions
}

/// Map Kotlin internal implementation class names to their public facade.
/// E.g. `kotlin/collections/CollectionsKt__CollectionsKt` → `kotlin/collections/CollectionsKt`
fn normalize_facade_class(name: &str) -> String {
    // Internal implementation classes have `__` or `___` before the suffix.
    if let Some(idx) = name.find("__") {
        name[..idx].to_string()
    } else {
        name.to_string()
    }
}

/// Parse a JVM descriptor to extract parameter type strings.
fn parse_descriptor_params(desc: &str) -> Vec<String> {
    let inner = desc.split(')').next().unwrap_or("").trim_start_matches('(');
    let mut params = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z' => {
                params.push(c.to_string());
            }
            'L' => {
                let mut s = String::from("L");
                for sc in chars.by_ref() {
                    s.push(sc);
                    if sc == ';' {
                        break;
                    }
                }
                params.push(s);
            }
            '[' => {
                let mut s = String::from("[");
                if let Some(&next) = chars.peek() {
                    if next == 'L' {
                        chars.next();
                        s.push('L');
                        for sc in chars.by_ref() {
                            s.push(sc);
                            if sc == ';' {
                                break;
                            }
                        }
                    } else {
                        s.push(chars.next().unwrap_or('I'));
                    }
                }
                params.push(s);
            }
            _ => {}
        }
    }
    params
}

/// Returns an empty registry. Individual classes are loaded
/// on-demand via the `JDK_CLASS_CACHE` / `ARCHIVE_CACHE` pair on the
/// first `load_jdk_class(name)` call — no need to eagerly parse 26
/// classes (~70 ms cold) when a typical compile only references a
/// few. Kept as a function so callers that pass `build_jdk_registry`
/// to `get_or_insert_with` keep working unchanged.
pub fn build_jdk_registry() -> HashMap<String, ClassInfo> {
    HashMap::new()
}

/// Find the JDK home directory.
fn find_jdk_home() -> io::Result<PathBuf> {
    // Check JAVA_HOME first.
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let p = PathBuf::from(home);
        if p.join("jmods").exists() {
            return Ok(p);
        }
    }
    // macOS Homebrew path.
    let brew = Path::new("/opt/homebrew/opt/java/libexec/openjdk.jdk/Contents/Home");
    if brew.join("jmods").exists() {
        return Ok(brew.to_path_buf());
    }
    // Linux common paths.
    for base in &["/usr/lib/jvm", "/usr/local/lib/jvm"] {
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.join("jmods").exists() {
                    return Ok(p);
                }
            }
        }
    }
    // Try `which java` and resolve symlinks.
    // Try `which java` and resolve symlinks.
    if let Ok(java_path) = which::which("java") {
        if let Ok(resolved) = std::fs::canonicalize(&java_path) {
            // java is typically at $JDK/bin/java
            if let Some(bin) = resolved.parent() {
                if let Some(home) = bin.parent() {
                    if home.join("jmods").exists() {
                        return Ok(home.to_path_buf());
                    }
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "JDK home not found",
    ))
}

// ── Internal helpers ────────────────────────────────────────────────────

#[derive(Clone)]
enum CpEntry {
    Empty,
    /// `CONSTANT_Utf8` stored as raw bytes — decoded on demand via
    /// Modified UTF-8 (JVMS §4.4.7). Raw bytes are kept (rather than a
    /// pre-decoded `String`) so `@kotlin.Metadata` `d1`/`d2` payloads,
    /// which use the two-byte forms standard UTF-8 rejects, survive.
    Utf8(Vec<u8>),
    /// `CONSTANT_Integer` value — needed to read the `@Metadata` `k`
    /// (kind) element.
    Int(i32),
    Class(usize),
    NameAndType(#[allow(dead_code)] usize, #[allow(dead_code)] usize),
    Other,
}

fn resolve_utf8(cp: &[CpEntry], idx: usize) -> String {
    if idx < cp.len() {
        if let CpEntry::Utf8(bytes) = &cp[idx] {
            return kotlin_metadata::decode_modified_utf8(bytes);
        }
    }
    String::new()
}

fn resolve_int(cp: &[CpEntry], idx: usize) -> Option<i32> {
    match cp.get(idx) {
        Some(CpEntry::Int(v)) => Some(*v),
        _ => None,
    }
}

fn resolve_class(cp: &[CpEntry], idx: usize) -> String {
    if idx < cp.len() {
        if let CpEntry::Class(name_idx) = &cp[idx] {
            return resolve_utf8(cp, *name_idx);
        }
    }
    String::new()
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> io::Result<u8> {
        if self.pos >= self.bytes.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
        }
        let v = self.bytes[self.pos];
        self.pos += 1;
        Ok(v)
    }
    fn u16(&mut self) -> io::Result<u16> {
        if self.pos + 1 >= self.bytes.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
        }
        let v = u16::from_be_bytes([self.bytes[self.pos], self.bytes[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }
    fn u32(&mut self) -> io::Result<u32> {
        if self.pos + 3 >= self.bytes.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
        }
        let v = u32::from_be_bytes([
            self.bytes[self.pos],
            self.bytes[self.pos + 1],
            self.bytes[self.pos + 2],
            self.bytes[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }
}

/// Convert a JVM method descriptor return type to a simple type name.
pub fn return_type_from_descriptor(desc: &str) -> &str {
    let ret = desc.rsplit(')').next().unwrap_or("V");
    match ret {
        "V" => "Unit",
        "Z" => "Boolean",
        "B" | "S" | "I" => "Int", // byte, short, int → Int
        "C" => "Char",
        "J" => "Long",
        "D" | "F" => "Double", // float and double → Double
        _ if ret.starts_with("Ljava/lang/String;") => "String",
        _ if ret.starts_with('L') => "Object",
        _ if ret.starts_with('[') => "Array",
        _ => "Any",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_system_class() {
        if let Ok(info) = load_jdk_class("java/lang/System") {
            assert_eq!(info.name, "java/lang/System");
            let millis = info.methods.iter().find(|m| m.name == "currentTimeMillis");
            assert!(millis.is_some(), "currentTimeMillis not found");
            let m = millis.unwrap();
            assert_eq!(m.descriptor, "()J");
            assert!(m.is_static());
            assert!(m.is_public());
        }
        // Skip test if JDK not available.
    }

    #[test]
    fn parse_math_class() {
        if let Ok(info) = load_jdk_class("java/lang/Math") {
            let random = info.methods.iter().find(|m| m.name == "random");
            assert!(random.is_some(), "random not found");
            assert_eq!(random.unwrap().descriptor, "()D");
        }
    }

    #[test]
    fn return_type_parsing() {
        assert_eq!(return_type_from_descriptor("()V"), "Unit");
        assert_eq!(return_type_from_descriptor("()I"), "Int");
        assert_eq!(return_type_from_descriptor("()J"), "Long");
        assert_eq!(return_type_from_descriptor("(Ljava/lang/String;)I"), "Int");
        assert_eq!(
            return_type_from_descriptor("()Ljava/lang/String;"),
            "String"
        );
    }
}
