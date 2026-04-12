//! Minimal `.class` file parser for extracting method and field signatures.
//!
//! Reads just enough of the class file format to build a registry of
//! available methods for Java interop — constant pool, field_info,
//! method_info, and access flags. Does NOT parse bytecode, attributes,
//! or annotations.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Information about a single Java class.
#[derive(Clone, Debug)]
pub struct ClassInfo {
    /// JVM internal name, e.g. "java/lang/System".
    pub name: String,
    /// Access flags (ACC_PUBLIC, ACC_STATIC, etc.).
    pub access_flags: u16,
    /// Methods in this class.
    pub methods: Vec<MethodInfo>,
    /// Fields in this class.
    pub fields: Vec<FieldInfo>,
}

/// A method signature.
#[derive(Clone, Debug)]
pub struct MethodInfo {
    pub name: String,
    pub descriptor: String,
    pub access_flags: u16,
}

/// A field signature.
#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub descriptor: String,
    pub access_flags: u16,
}

const ACC_PUBLIC: u16 = 0x0001;
const ACC_STATIC: u16 = 0x0008;

impl MethodInfo {
    pub fn is_static(&self) -> bool {
        self.access_flags & ACC_STATIC != 0
    }
    pub fn is_public(&self) -> bool {
        self.access_flags & ACC_PUBLIC != 0
    }
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
                // CONSTANT_Utf8
                let len = r.u16()? as usize;
                let s = std::str::from_utf8(&r.bytes[r.pos..r.pos + len])
                    .unwrap_or("")
                    .to_string();
                r.pos += len;
                cp[i] = CpEntry::Utf8(s);
            }
            3 => {
                r.pos += 4;
                cp[i] = CpEntry::Other;
            } // Integer
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
    let _super_class = r.u16()?;

    // Class name.
    let class_name = resolve_class(&cp, this_class);

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
        for _ in 0..f_attrs {
            let _attr_name = r.u16()?;
            let attr_len = r.u32()? as usize;
            r.pos += attr_len;
        }
        fields.push(FieldInfo {
            name: resolve_utf8(&cp, f_name_idx),
            descriptor: resolve_utf8(&cp, f_desc_idx),
            access_flags: f_access,
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
        for _ in 0..m_attrs {
            let _attr_name = r.u16()?;
            let attr_len = r.u32()? as usize;
            r.pos += attr_len;
        }
        methods.push(MethodInfo {
            name: resolve_utf8(&cp, m_name_idx),
            descriptor: resolve_utf8(&cp, m_desc_idx),
            access_flags: m_access,
        });
    }

    Ok(ClassInfo {
        name: class_name,
        access_flags,
        methods,
        fields,
    })
}

/// Load a class from the JDK's jmod files. Searches java.base first,
/// then all other jmod files in the jmods/ directory.
pub fn load_jdk_class(class_path: &str) -> io::Result<ClassInfo> {
    let jdk_home = find_jdk_home()?;
    let jmods_dir = jdk_home.join("jmods");
    if !jmods_dir.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "jmods directory not found",
        ));
    }
    let entry_path = format!("classes/{class_path}.class");

    // Try java.base.jmod first (most common classes).
    let base_jmod = jmods_dir.join("java.base.jmod");
    if base_jmod.exists() {
        if let Ok(info) = load_class_from_jmod(&base_jmod, &entry_path) {
            return Ok(info);
        }
    }

    // Search all other jmod files.
    if let Ok(entries) = std::fs::read_dir(&jmods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jmod") {
                if let Ok(info) = load_class_from_jmod(&path, &entry_path) {
                    return Ok(info);
                }
            }
        }
    }

    // Also check CLASSPATH for directories and JARs.
    if let Ok(cp) = std::env::var("CLASSPATH") {
        for entry in cp.split(':') {
            let p = Path::new(entry);
            if p.is_dir() {
                let class_file = p.join(format!("{class_path}.class"));
                if class_file.exists() {
                    let bytes = std::fs::read(&class_file)?;
                    return parse_class(&bytes);
                }
            } else if p.extension().and_then(|e| e.to_str()) == Some("jar") && p.exists() {
                if let Ok(info) = load_class_from_jar(p, &format!("{class_path}.class")) {
                    return Ok(info);
                }
            }
        }
    }

    // Search Kotlin stdlib JARs.
    if let Ok(kotlin_libs) = find_kotlin_lib_dir() {
        for jar_name in &[
            "kotlin-stdlib.jar",
            "kotlin-stdlib-jdk8.jar",
            "kotlin-stdlib-jdk7.jar",
        ] {
            let jar = kotlin_libs.join(jar_name);
            if jar.exists() {
                if let Ok(info) = load_class_from_jar(&jar, &format!("{class_path}.class")) {
                    return Ok(info);
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
        // Resolve the ultimate symlink destination.
        if let Ok(resolved) = std::fs::canonicalize(&kotlinc_path) {
            // kotlinc is typically at $KOTLIN_HOME/bin/kotlinc
            if let Some(bin) = resolved.parent() {
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
    let file = std::fs::File::open(jmod_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut entry = archive.by_name(entry_path).map_err(|_| {
        io::Error::new(io::ErrorKind::NotFound, format!("{entry_path} not in jmod"))
    })?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    parse_class(&bytes)
}

fn load_class_from_jar(jar_path: &Path, entry_path: &str) -> io::Result<ClassInfo> {
    let file = std::fs::File::open(jar_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut entry = archive
        .by_name(entry_path)
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, format!("{entry_path} not in jar")))?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    parse_class(&bytes)
}

/// Build a class registry from the JDK for commonly-used java.lang classes.
pub fn build_jdk_registry() -> HashMap<String, ClassInfo> {
    let mut reg = HashMap::new();
    let classes = [
        "java/lang/System",
        "java/lang/Math",
        "java/lang/Integer",
        "java/lang/Long",
        "java/lang/Double",
        "java/lang/Boolean",
        "java/lang/String",
        "java/lang/Thread",
        "java/lang/Runtime",
        "java/lang/Object",
        "java/lang/StringBuilder",
        "java/util/Arrays",
    ];
    for class_path in &classes {
        if let Ok(info) = load_jdk_class(class_path) {
            let simple = class_path
                .rsplit('/')
                .next()
                .unwrap_or(class_path)
                .to_string();
            reg.insert(simple, info.clone());
            reg.insert(class_path.to_string(), info);
        }
    }

    // Pre-load common Kotlin stdlib classes.
    let kotlin_classes = [
        "kotlin/text/StringsKt",
        "kotlin/text/StringsKt__StringsKt",
        "kotlin/text/StringsKt__StringsJVMKt",
        "kotlin/text/StringsKt__StringNumberConversionsJVMKt",
        "kotlin/text/StringsKt__StringNumberConversionsKt",
        "kotlin/collections/CollectionsKt",
        "kotlin/collections/CollectionsKt__CollectionsKt",
        "kotlin/collections/CollectionsKt__CollectionsJVMKt",
        "kotlin/collections/ArraysKt",
        "kotlin/ranges/RangesKt",
        "kotlin/comparisons/ComparisonsKt",
        "kotlin/io/ConsoleKt",
        "kotlin/math/MathKt",
        "kotlin/math/MathKt__MathJVMKt",
    ];
    for class_path in &kotlin_classes {
        if let Ok(info) = load_jdk_class(class_path) {
            let simple = class_path
                .rsplit('/')
                .next()
                .unwrap_or(class_path)
                .to_string();
            reg.insert(simple, info.clone());
            reg.insert(class_path.to_string(), info);
        }
    }

    reg
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
    Utf8(String),
    Class(usize),
    NameAndType(#[allow(dead_code)] usize, #[allow(dead_code)] usize),
    Other,
}

fn resolve_utf8(cp: &[CpEntry], idx: usize) -> String {
    if idx < cp.len() {
        if let CpEntry::Utf8(s) = &cp[idx] {
            return s.clone();
        }
    }
    String::new()
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
        "B" | "S" | "C" | "I" => "Int", // byte, short, char, int → Int
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
