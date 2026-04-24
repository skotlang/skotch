//! Data-driven registry of Kotlin stdlib and JVM platform mappings.
//!
//! This crate replaces the ~140 hardcoded match arms that were previously
//! scattered across `skotch-mir-lower` and `skotch-backend-jvm`. All
//! mappings are defined as `static` arrays — zero allocation, zero runtime
//! parsing, and trivially extensible by adding entries to the tables.
//!
//! ## Categories
//!
//! | Table | Purpose |
//! |-------|---------|
//! | [`STDLIB_EXTENSIONS`] | Kotlin extension functions compiled as static methods |
//! | [`ANNOTATION_DESCRIPTORS`] | Annotation name → JVM descriptor |
//! | [`DEFAULT_IMPORTS`] | Implicit `java.lang.*` imports |
//! | [`STRING_OVERLOADS`] | String method overload disambiguation |
//! | [`MATH_FUNCTIONS`] | Top-level math function → `java.lang.Math` |
//! | [`AUTOBOX_RULES`] | Primitive → boxed wrapper class |
//! | [`JVM_INTERFACES`] | Class names that require `invokeinterface` |
//! | [`PRINTLN_DISPATCH`] | Type → `PrintStream.println` descriptor |
//! | [`WELL_KNOWN_CLASSES`] | Kotlin name → JVM internal name |
//!
//! ## Future: dynamic registry from classfile scanning
//!
//! These tables are currently static. Most of this data could instead be
//! discovered at build time by scanning `kotlin-stdlib.jar` and the JDK:
//!
//! - **JVM interfaces** (`JVM_INTERFACES`): The `ACC_INTERFACE` flag
//!   (0x0200) in each classfile's `access_flags` tells us whether a class
//!   is an interface. `skotch-classinfo` already parses these flags.
//!   Replacing the static list with a runtime `access_flags` check would
//!   make this table unnecessary.
//!
//! - **Stdlib extension functions** (`STDLIB_EXTENSIONS`): The facade
//!   classes (`CollectionsKt`, `MapsKt`, `StringsKt`) are in
//!   `kotlin-stdlib.jar` with full method signatures. Scanning every `*Kt`
//!   class for public static methods whose first parameter is the receiver
//!   type would auto-populate this table — no hardcoding needed.
//!
//! - **String overloads** (`STRING_OVERLOADS`): Already partially dynamic
//!   via `skotch-classinfo::load_jdk_class("java/lang/String")`. The
//!   static table is only needed because the current overload resolver
//!   can't disambiguate by argument type. With type-aware resolution,
//!   this table would shrink to zero.
//!
//! - **Annotation descriptors** (`ANNOTATION_DESCRIPTORS`): Derivable
//!   from import statements + package conventions. `@JvmStatic` in a file
//!   with `import kotlin.jvm.JvmStatic` resolves to `Lkotlin/jvm/JvmStatic;`
//!   via the import map. The static table is a fallback for unimported
//!   annotations.
//!
//! The practical migration path:
//! 1. On first `skotch build`, scan `kotlin-stdlib.jar` + JDK jmods
//! 2. Build the extension/interface/overload tables from classfile metadata
//! 3. Cache the result in `~/.skotch/cache/stdlib-registry-{hash}.json`
//! 4. Fall back to the static tables if the JAR is unavailable
//!
//! This would eliminate version-coupling (the static tables assume a
//! specific Kotlin stdlib version) and automatically support new stdlib
//! functions without code changes.

use skotch_types::Ty;

// ─── 1. Stdlib extension functions ──────────────────────────────────────────

/// An entry mapping a Kotlin extension function to its JVM static method.
pub struct StdlibExtension {
    /// Receiver type constraint. Empty string means any receiver.
    pub receiver: &'static str,
    /// Method name as written in Kotlin source.
    pub method: &'static str,
    /// JVM facade class that holds the static method.
    pub facade_class: &'static str,
    /// JVM method name (usually same as `method`).
    pub jvm_method: &'static str,
    /// JVM method descriptor including the receiver as first param.
    pub descriptor: &'static str,
    /// Return type.
    pub return_ty: fn() -> Ty,
}

fn ty_list() -> Ty {
    Ty::Class("java/util/List".into())
}
fn ty_map() -> Ty {
    Ty::Class("java/util/Map".into())
}
fn ty_set() -> Ty {
    Ty::Class("java/util/Set".into())
}
fn ty_string() -> Ty {
    Ty::Class("java/lang/String".into())
}

/// All Kotlin stdlib extension functions that compile to static JVM calls.
pub static STDLIB_EXTENSIONS: &[StdlibExtension] = &[
    // ── Collection/Iterable HOFs (CollectionsKt) ──
    StdlibExtension { receiver: "", method: "map", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "map", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "", method: "filter", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "filter", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "", method: "filterNot", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "filterNot", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "", method: "flatMap", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "flatMap", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "", method: "fold", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "fold", descriptor: "(Ljava/lang/Iterable;Ljava/lang/Object;Lkotlin/jvm/functions/Function2;)Ljava/lang/Object;", return_ty: || Ty::Any },
    StdlibExtension { receiver: "", method: "any", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "any", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Z", return_ty: || Ty::Bool },
    StdlibExtension { receiver: "", method: "all", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "all", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Z", return_ty: || Ty::Bool },
    StdlibExtension { receiver: "", method: "none", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "none", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Z", return_ty: || Ty::Bool },
    StdlibExtension { receiver: "List", method: "first", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "first", descriptor: "(Ljava/util/List;)Ljava/lang/Object;", return_ty: || Ty::Any },
    StdlibExtension { receiver: "List", method: "last", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "last", descriptor: "(Ljava/util/List;)Ljava/lang/Object;", return_ty: || Ty::Any },
    StdlibExtension { receiver: "List", method: "firstOrNull", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "firstOrNull", descriptor: "(Ljava/util/List;)Ljava/lang/Object;", return_ty: || Ty::Any },
    StdlibExtension { receiver: "List", method: "count", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "count", descriptor: "(Ljava/lang/Iterable;)I", return_ty: || Ty::Int },
    StdlibExtension { receiver: "", method: "sortedBy", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "sortedBy", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "List", method: "reversed", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "reversed", descriptor: "(Ljava/lang/Iterable;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "", method: "distinct", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "distinct", descriptor: "(Ljava/lang/Iterable;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "List", method: "take", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "take", descriptor: "(Ljava/lang/Iterable;I)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "List", method: "drop", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "drop", descriptor: "(Ljava/lang/Iterable;I)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "", method: "associateWith", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "associateWith", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/Map;", return_ty: ty_map },
    StdlibExtension { receiver: "", method: "associateBy", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "associateBy", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/Map;", return_ty: ty_map },
    StdlibExtension { receiver: "", method: "groupBy", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "groupBy", descriptor: "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)Ljava/util/Map;", return_ty: ty_map },
    StdlibExtension { receiver: "", method: "flatten", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "flatten", descriptor: "(Ljava/lang/Iterable;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "List", method: "zip", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "zip", descriptor: "(Ljava/lang/Iterable;Ljava/lang/Iterable;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "List", method: "toList", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "toList", descriptor: "(Ljava/lang/Iterable;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "List", method: "toSet", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "toSet", descriptor: "(Ljava/lang/Iterable;)Ljava/util/Set;", return_ty: ty_set },
    StdlibExtension { receiver: "List", method: "toMutableList", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "toMutableList", descriptor: "(Ljava/lang/Iterable;)Ljava/util/List;", return_ty: ty_list },
    // ── Map extensions (MapsKt) ──
    StdlibExtension { receiver: "Map", method: "forEach", facade_class: "kotlin/collections/MapsKt", jvm_method: "forEach", descriptor: "(Ljava/util/Map;Lkotlin/jvm/functions/Function2;)V", return_ty: || Ty::Unit },
    StdlibExtension { receiver: "Map", method: "toList", facade_class: "kotlin/collections/MapsKt", jvm_method: "toList", descriptor: "(Ljava/util/Map;)Ljava/util/List;", return_ty: ty_list },
    // ── String extensions (StringsKt) ──
    StdlibExtension { receiver: "java/lang/String", method: "lines", facade_class: "kotlin/text/StringsKt", jvm_method: "lines", descriptor: "(Ljava/lang/CharSequence;)Ljava/util/List;", return_ty: ty_list },
    StdlibExtension { receiver: "java/lang/String", method: "reversed", facade_class: "kotlin/text/StringsKt", jvm_method: "reversed", descriptor: "(Ljava/lang/CharSequence;)Ljava/lang/CharSequence;", return_ty: || Ty::Any },
    // ── joinToString (uses $default for default params) ──
    StdlibExtension { receiver: "", method: "joinToString", facade_class: "kotlin/collections/CollectionsKt", jvm_method: "joinToString$default", descriptor: "(Ljava/lang/Iterable;Ljava/lang/CharSequence;Ljava/lang/CharSequence;Ljava/lang/CharSequence;ILjava/lang/CharSequence;Lkotlin/jvm/functions/Function1;ILjava/lang/Object;)Ljava/lang/String;", return_ty: ty_string },
];

/// Look up a stdlib extension function by receiver type and method name.
pub fn lookup_stdlib_extension(
    receiver_ty: &str,
    method: &str,
) -> Option<&'static StdlibExtension> {
    STDLIB_EXTENSIONS
        .iter()
        .find(|e| e.method == method && (e.receiver.is_empty() || receiver_ty.contains(e.receiver)))
}

// ─── 2. Annotation descriptors ──────────────────────────────────────────────

/// Mapping from simple annotation name to JVM type descriptor.
pub static ANNOTATION_DESCRIPTORS: &[(&str, &str)] = &[
    ("JvmStatic", "Lkotlin/jvm/JvmStatic;"),
    ("JvmField", "Lkotlin/jvm/JvmField;"),
    ("JvmOverloads", "Lkotlin/jvm/JvmOverloads;"),
    ("JvmName", "Lkotlin/jvm/JvmName;"),
    ("Suppress", "Lkotlin/Suppress;"),
    ("Deprecated", "Lkotlin/Deprecated;"),
    ("Composable", "Landroidx/compose/runtime/Composable;"),
    ("Preview", "Landroidx/compose/ui/tooling/preview/Preview;"),
    ("OptIn", "Lkotlin/OptIn;"),
    ("Throws", "Lkotlin/Throws;"),
    ("Transient", "Lkotlin/jvm/Transient;"),
    ("Volatile", "Lkotlin/jvm/Volatile;"),
    ("Strictfp", "Lkotlin/jvm/Strictfp;"),
    ("Synchronized", "Lkotlin/jvm/Synchronized;"),
];

/// Look up the JVM descriptor for an annotation name.
pub fn annotation_descriptor(name: &str) -> String {
    ANNOTATION_DESCRIPTORS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, d)| d.to_string())
        .unwrap_or_else(|| format!("L{name};"))
}

// ─── 3. Default imports ─────────────────────────────────────────────────────

/// Classes implicitly imported from `java.lang.*` in every Kotlin file.
pub static DEFAULT_IMPORTS: &[&str] = &[
    "System",
    "Math",
    "Integer",
    "Long",
    "Double",
    "Boolean",
    "String",
    "Thread",
    "Runtime",
    "Object",
    "Class",
    "Comparable",
];

// ─── 4. String method overloads ─────────────────────────────────────────────

/// Disambiguation table for String methods with multiple JVM overloads.
pub struct StringOverload {
    pub method: &'static str,
    pub arg_count: usize,
    pub jvm_class: &'static str,
    pub jvm_method: &'static str,
    pub descriptor: &'static str,
    pub return_ty: fn() -> Ty,
}

pub static STRING_OVERLOADS: &[StringOverload] = &[
    StringOverload {
        method: "replace",
        arg_count: 2,
        jvm_class: "java/lang/String",
        jvm_method: "replace",
        descriptor: "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Ljava/lang/String;",
        return_ty: || Ty::String,
    },
    StringOverload {
        method: "matches",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "matches",
        descriptor: "(Ljava/lang/String;)Z",
        return_ty: || Ty::Bool,
    },
    StringOverload {
        method: "contains",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "contains",
        descriptor: "(Ljava/lang/CharSequence;)Z",
        return_ty: || Ty::Bool,
    },
    StringOverload {
        method: "indexOf",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "indexOf",
        descriptor: "(Ljava/lang/String;)I",
        return_ty: || Ty::Int,
    },
    StringOverload {
        method: "lastIndexOf",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "lastIndexOf",
        descriptor: "(Ljava/lang/String;)I",
        return_ty: || Ty::Int,
    },
    StringOverload {
        method: "startsWith",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "startsWith",
        descriptor: "(Ljava/lang/String;)Z",
        return_ty: || Ty::Bool,
    },
    StringOverload {
        method: "endsWith",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "endsWith",
        descriptor: "(Ljava/lang/String;)Z",
        return_ty: || Ty::Bool,
    },
    StringOverload {
        method: "substring",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "substring",
        descriptor: "(I)Ljava/lang/String;",
        return_ty: || Ty::String,
    },
    StringOverload {
        method: "substring",
        arg_count: 2,
        jvm_class: "java/lang/String",
        jvm_method: "substring",
        descriptor: "(II)Ljava/lang/String;",
        return_ty: || Ty::String,
    },
    StringOverload {
        method: "split",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "split",
        descriptor: "(Ljava/lang/String;)[Ljava/lang/String;",
        return_ty: || Ty::Any,
    },
    StringOverload {
        method: "trim",
        arg_count: 0,
        jvm_class: "java/lang/String",
        jvm_method: "trim",
        descriptor: "()Ljava/lang/String;",
        return_ty: || Ty::String,
    },
    StringOverload {
        method: "toByteArray",
        arg_count: 0,
        jvm_class: "java/lang/String",
        jvm_method: "getBytes",
        descriptor: "()[B",
        return_ty: || Ty::ByteArray,
    },
    StringOverload {
        method: "toCharArray",
        arg_count: 0,
        jvm_class: "java/lang/String",
        jvm_method: "toCharArray",
        descriptor: "()[C",
        return_ty: || Ty::Any,
    },
    StringOverload {
        method: "repeat",
        arg_count: 1,
        jvm_class: "java/lang/String",
        jvm_method: "repeat",
        descriptor: "(I)Ljava/lang/String;",
        return_ty: || Ty::String,
    },
];

/// Look up a String method overload by name and argument count.
pub fn lookup_string_overload(method: &str, arg_count: usize) -> Option<&'static StringOverload> {
    STRING_OVERLOADS
        .iter()
        .find(|o| o.method == method && o.arg_count == arg_count)
}

// ─── 5. Math functions ──────────────────────────────────────────────────────

/// Top-level math function → `java.lang.Math` static method.
pub struct MathFunction {
    pub name: &'static str,
    pub arg_count: usize,
    pub jvm_method: &'static str,
    pub descriptor: &'static str,
    pub return_ty: fn() -> Ty,
}

pub static MATH_FUNCTIONS: &[MathFunction] = &[
    MathFunction {
        name: "maxOf",
        arg_count: 2,
        jvm_method: "max",
        descriptor: "(II)I",
        return_ty: || Ty::Int,
    },
    MathFunction {
        name: "minOf",
        arg_count: 2,
        jvm_method: "min",
        descriptor: "(II)I",
        return_ty: || Ty::Int,
    },
    MathFunction {
        name: "sqrt",
        arg_count: 1,
        jvm_method: "sqrt",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "ceil",
        arg_count: 1,
        jvm_method: "ceil",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "floor",
        arg_count: 1,
        jvm_method: "floor",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "round",
        arg_count: 1,
        jvm_method: "round",
        descriptor: "(D)J",
        return_ty: || Ty::Long,
    },
    MathFunction {
        name: "pow",
        arg_count: 2,
        jvm_method: "pow",
        descriptor: "(DD)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "sin",
        arg_count: 1,
        jvm_method: "sin",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "cos",
        arg_count: 1,
        jvm_method: "cos",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "tan",
        arg_count: 1,
        jvm_method: "tan",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "log",
        arg_count: 1,
        jvm_method: "log",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "log10",
        arg_count: 1,
        jvm_method: "log10",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
    MathFunction {
        name: "exp",
        arg_count: 1,
        jvm_method: "exp",
        descriptor: "(D)D",
        return_ty: || Ty::Double,
    },
];

/// Look up a math function by name and argument count.
pub fn lookup_math_function(name: &str, arg_count: usize) -> Option<&'static MathFunction> {
    MATH_FUNCTIONS
        .iter()
        .find(|f| f.name == name && f.arg_count == arg_count)
}

// ─── 6. Autoboxing rules ────────────────────────────────────────────────────

/// Primitive type → boxed wrapper class for JVM autoboxing.
pub struct AutoboxRule {
    pub jvm_class: &'static str,
    pub method: &'static str,
    pub descriptor: &'static str,
}

/// Look up the autoboxing call for a primitive type.
pub fn autobox_info(ty: &Ty) -> Option<AutoboxRule> {
    match ty {
        Ty::Int => Some(AutoboxRule {
            jvm_class: "java/lang/Integer",
            method: "valueOf",
            descriptor: "(I)Ljava/lang/Integer;",
        }),
        Ty::Long => Some(AutoboxRule {
            jvm_class: "java/lang/Long",
            method: "valueOf",
            descriptor: "(J)Ljava/lang/Long;",
        }),
        Ty::Double => Some(AutoboxRule {
            jvm_class: "java/lang/Double",
            method: "valueOf",
            descriptor: "(D)Ljava/lang/Double;",
        }),
        Ty::Bool => Some(AutoboxRule {
            jvm_class: "java/lang/Boolean",
            method: "valueOf",
            descriptor: "(Z)Ljava/lang/Boolean;",
        }),
        Ty::Char => Some(AutoboxRule {
            jvm_class: "java/lang/Character",
            method: "valueOf",
            descriptor: "(C)Ljava/lang/Character;",
        }),
        Ty::Byte => Some(AutoboxRule {
            jvm_class: "java/lang/Byte",
            method: "valueOf",
            descriptor: "(B)Ljava/lang/Byte;",
        }),
        Ty::Short => Some(AutoboxRule {
            jvm_class: "java/lang/Short",
            method: "valueOf",
            descriptor: "(S)Ljava/lang/Short;",
        }),
        Ty::Float => Some(AutoboxRule {
            jvm_class: "java/lang/Float",
            method: "valueOf",
            descriptor: "(F)Ljava/lang/Float;",
        }),
        _ => None,
    }
}

// ─── 7. JVM interfaces ─────────────────────────────────────────────────────

/// Class names that require `invokeinterface` instead of `invokevirtual`.
pub static JVM_INTERFACES: &[&str] = &[
    "java/util/Iterator",
    "java/util/List",
    "java/util/Collection",
    "java/util/Set",
    "java/util/Map",
    "java/util/Map$Entry",
    "java/lang/Iterable",
    "java/lang/Comparable",
    "java/lang/CharSequence",
    "java/lang/Runnable",
    "java/lang/AutoCloseable",
    "java/io/Closeable",
    "kotlinx/coroutines/Deferred",
    "kotlinx/coroutines/Job",
];

/// Check if a class name requires `invokeinterface` dispatch.
pub fn is_jvm_interface(class_name: &str) -> bool {
    JVM_INTERFACES.contains(&class_name) || class_name.starts_with("kotlin/jvm/functions/Function")
}

// ─── 8. println/print dispatch ──────────────────────────────────────────────

/// Get the `PrintStream.println` descriptor for a given type.
pub fn println_descriptor(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bool => "(Z)V",
        Ty::Char => "(C)V",
        Ty::Int | Ty::Byte | Ty::Short => "(I)V",
        Ty::Float => "(F)V",
        Ty::Long => "(J)V",
        Ty::Double => "(D)V",
        Ty::String => "(Ljava/lang/String;)V",
        _ => "(Ljava/lang/Object;)V",
    }
}

/// Get the `PrintStream.print` descriptor for a given type.
pub fn print_descriptor(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bool => "(Z)V",
        Ty::Int => "(I)V",
        Ty::Long => "(J)V",
        Ty::Double => "(D)V",
        Ty::String => "(Ljava/lang/String;)V",
        _ => "(Ljava/lang/Object;)V",
    }
}

// ─── 9. Well-known class names ──────────────────────────────────────────────

/// Mapping from Kotlin source-level class names to JVM internal names.
pub static WELL_KNOWN_CLASSES: &[(&str, &str)] = &[
    ("List", "java/util/List"),
    ("MutableList", "java/util/List"),
    ("Map", "java/util/Map"),
    ("MutableMap", "java/util/Map"),
    ("Set", "java/util/Set"),
    ("MutableSet", "java/util/Set"),
    ("Collection", "java/util/Collection"),
    ("Iterable", "java/lang/Iterable"),
    ("Iterator", "java/util/Iterator"),
    ("Pair", "kotlin/Pair"),
    ("Triple", "kotlin/Triple"),
];

/// Look up the JVM internal name for a well-known Kotlin class.
pub fn well_known_class(name: &str) -> Option<&'static str> {
    WELL_KNOWN_CLASSES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, jvm)| *jvm)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdlib_extension_lookup() {
        let ext = lookup_stdlib_extension("java/util/List", "filter").unwrap();
        assert_eq!(ext.facade_class, "kotlin/collections/CollectionsKt");
        assert_eq!(ext.jvm_method, "filter");
    }

    #[test]
    fn stdlib_extension_any_receiver() {
        // "map" has empty receiver — matches any type.
        let ext = lookup_stdlib_extension("SomeRandomType", "map").unwrap();
        assert_eq!(ext.facade_class, "kotlin/collections/CollectionsKt");
    }

    #[test]
    fn stdlib_extension_receiver_constraint() {
        // "first" requires List/Iterable receiver.
        assert!(lookup_stdlib_extension("java/util/List", "first").is_some());
        assert!(lookup_stdlib_extension("java/lang/String", "first").is_none());
    }

    #[test]
    fn annotation_known() {
        assert_eq!(annotation_descriptor("JvmStatic"), "Lkotlin/jvm/JvmStatic;");
    }

    #[test]
    fn annotation_unknown() {
        assert_eq!(annotation_descriptor("MyCustom"), "LMyCustom;");
    }

    #[test]
    fn string_overload_lookup() {
        let o = lookup_string_overload("replace", 2).unwrap();
        assert_eq!(o.jvm_method, "replace");
        assert!(o.descriptor.contains("CharSequence"));
    }

    #[test]
    fn math_function_lookup() {
        let f = lookup_math_function("sqrt", 1).unwrap();
        assert_eq!(f.jvm_method, "sqrt");
        assert_eq!(f.descriptor, "(D)D");
    }

    #[test]
    fn math_function_missing() {
        assert!(lookup_math_function("nonexistent", 1).is_none());
    }

    #[test]
    fn autobox_int() {
        let rule = autobox_info(&Ty::Int).unwrap();
        assert_eq!(rule.jvm_class, "java/lang/Integer");
    }

    #[test]
    fn autobox_string_none() {
        assert!(autobox_info(&Ty::String).is_none());
    }

    #[test]
    fn jvm_interface_check() {
        assert!(is_jvm_interface("java/util/List"));
        assert!(is_jvm_interface("kotlin/jvm/functions/Function1"));
        assert!(!is_jvm_interface("java/lang/String"));
    }

    #[test]
    fn println_dispatch() {
        assert_eq!(println_descriptor(&Ty::Int), "(I)V");
        assert_eq!(println_descriptor(&Ty::String), "(Ljava/lang/String;)V");
        assert_eq!(println_descriptor(&Ty::Any), "(Ljava/lang/Object;)V");
    }

    #[test]
    fn well_known_class_lookup() {
        assert_eq!(well_known_class("List"), Some("java/util/List"));
        assert_eq!(well_known_class("Pair"), Some("kotlin/Pair"));
        assert_eq!(well_known_class("Unknown"), None);
    }

    #[test]
    fn all_stdlib_extensions_have_valid_descriptors() {
        for ext in STDLIB_EXTENSIONS {
            assert!(
                ext.descriptor.starts_with('('),
                "Bad descriptor for {}.{}: {}",
                ext.facade_class,
                ext.method,
                ext.descriptor
            );
            assert!(
                ext.descriptor.contains(')'),
                "Bad descriptor for {}.{}: {}",
                ext.facade_class,
                ext.method,
                ext.descriptor
            );
        }
    }

    #[test]
    fn all_annotation_descriptors_valid() {
        for (name, desc) in ANNOTATION_DESCRIPTORS {
            assert!(desc.starts_with('L'), "Bad descriptor for {name}: {desc}");
            assert!(desc.ends_with(';'), "Bad descriptor for {name}: {desc}");
        }
    }
}
