//! Classpath registry — shared global state of `ClassInfo` records
//! loaded from JDK jmods, the Kotlin stdlib, and dependency JARs on
//! the user's CLASSPATH. Used by static/virtual method dispatch and
//! by the `<init>` descriptor lookup.
//!
//! Lives at the `skotch-mir-lower` crate root so the CLI's
//! `preload_registry_jars(&classpath)` entry point keeps the same
//! import path that consumers used before the legacy mir-lower removal.

use std::collections::HashMap;
use std::sync::Mutex;

static CLASS_REGISTRY: Mutex<Option<HashMap<String, std::sync::Arc<skotch_classinfo::ClassInfo>>>> =
    Mutex::new(None);

fn with_registry<R>(
    f: impl FnOnce(&mut HashMap<String, std::sync::Arc<skotch_classinfo::ClassInfo>>) -> R,
) -> Option<R> {
    let mut guard = CLASS_REGISTRY.lock().ok()?;
    let reg = guard.get_or_insert_with(HashMap::new);
    Some(f(reg))
}

/// Pre-load classes from dependency JARs into the shared registry.
/// Call this before compilation so external method dispatch is
/// resolvable. Also seeds the per-classpath ZIP cache so each call
/// site doesn't re-scan the JARs.
pub fn preload_registry_jars(jars: &[std::path::PathBuf]) {
    let scanned = skotch_classinfo::scan_jars(jars);
    if let Ok(mut guard) = CLASS_REGISTRY.lock() {
        let reg = guard.get_or_insert_with(HashMap::new);
        for (k, v) in scanned {
            reg.entry(k).or_insert_with(|| std::sync::Arc::new(v));
        }
    }
    skotch_classinfo::preload_classpath_cache(jars);
}

/// Ensure a class is loaded into the registry (by JVM internal path).
fn ensure_class_loaded(class_name: &str) {
    let needs_load = with_registry(|reg| !reg.contains_key(class_name)).unwrap_or(true);
    if needs_load {
        let jvm_path = if class_name.contains('/') {
            class_name.to_string()
        } else {
            format!("java/lang/{class_name}")
        };
        if let Ok(info) = skotch_classinfo::load_jdk_class(&jvm_path) {
            if let Ok(mut guard) = CLASS_REGISTRY.lock() {
                if let Some(reg) = guard.as_mut() {
                    reg.insert(class_name.to_string(), info);
                }
            }
        }
    }
}

/// Check if `method_name` is a static, public method on the class
/// identified by `class_jvm_path`. Used to recognize static method
/// imports like `import Assertions.assertTrue`.
pub fn is_static_method_on_class(class_jvm_path: &str, method_name: &str) -> bool {
    ensure_class_loaded(class_jvm_path);
    with_registry(|reg| {
        reg.get(class_jvm_path)
            .map(|ci| {
                ci.methods
                    .iter()
                    .any(|m| m.name == method_name && m.is_static() && m.is_public())
            })
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// Look up the source-level parameter names of a constructor on
/// `class_jvm_path` whose arity matches `arity` (excluding the
/// implicit `this`). Reads `@kotlin.Metadata` from the loaded
/// classfile and walks the protobuf-encoded constructor list. Used by
/// the cross-file super-call named-arg reorder in mir-lower so
/// `class Sub: Base { constructor(...) : super(algorithm = ..., ...) }`
/// can map `super(name = expr, ...)` back to the positional order
/// expected by `Base.<init>`. Returns `None` if the class isn't
/// loaded, has no metadata, or has no constructor of that arity.
pub fn lookup_external_ctor_param_names(class_jvm_path: &str, arity: usize) -> Option<Vec<String>> {
    ensure_class_loaded(class_jvm_path);
    let raw = with_registry(|reg| reg.get(class_jvm_path).cloned()).flatten()?;
    let raw_md = raw.metadata.as_ref()?;
    let md = skotch_classinfo::kotlin_metadata::parse_metadata(raw_md)?;
    let ctor = md
        .constructors
        .iter()
        .find(|c| c.value_params.len() == arity)?;
    Some(ctor.value_params.iter().map(|p| p.name.clone()).collect())
}

/// Check if `class_name` is a JVM interface. Reads the ACC_INTERFACE
/// flag from the classfile when the class is available in the
/// registry; falls back to the static stdlib registry for known
/// stdlib interfaces when no classfile is available (e.g. no JDK
/// installed).
pub fn is_jvm_interface(class_name: &str) -> bool {
    ensure_class_loaded(class_name);
    let from_classfile =
        with_registry(|reg| reg.get(class_name).map(|ci| ci.is_interface())).flatten();
    if let Some(is_iface) = from_classfile {
        return is_iface;
    }
    skotch_stdlib_registry::is_jvm_interface(class_name)
}
