//! Centralized lists of Kotlin standard-library function/class names
//! that skotch handles specially.
//!
//! These cases CANNOT be replaced by reading classpath generic
//! signatures + unifying — they need bespoke handling for one of
//! three reasons:
//!
//!   1. **Compiler intrinsics with custom bytecode.** Coroutine
//!      builders (`runBlocking`, `withTimeout`) need continuation
//!      passing, scope construction, and `$default` mask handling
//!      that's not derivable from the method signature. Same for
//!      `error()` / `TODO()` which throw special exceptions.
//!
//!   2. **Compose plugin transformations.** `remember { … }` and
//!      friends would have their bodies turned into
//!      `currentComposer.cache(...)` calls by the Compose plugin
//!      that skotch doesn't run. The lambda's return type still
//!      drives the call's result type, but the emission path
//!      differs from a vanilla static call.
//!
//!   3. **Kotlin → Java type aliases defined by the Kotlin spec.**
//!      `kotlin.List` maps to `java.util.List`, `kotlin.String` to
//!      `java.lang.String`. These are language-level facts, not
//!      classpath data.
//!
//! Anything *not* in this module is now handled by the general
//! signature-driven unifier in [`skotch_classinfo::generic_signature`].
//!
//! This module is also the single home for the Kotlin-name → JVM-class
//! tables that the resolver and MIR-lowering both need (type aliases,
//! exception classes, `catch`-type resolution, and the boxed classes
//! used by `is`/`as` runtime checks). Each such name list lives here
//! exactly once; call sites consult the relevant accessor rather than
//! re-listing names inline.

use crate::Ty;

// ── 1. Coroutine builders ───────────────────────────────────────

/// Top-level coroutine builders that take a `suspend …` trailing
/// lambda and bridge to/from non-suspend code. Each one needs
/// dedicated bytecode (continuation passing, scope construction);
/// the unifier alone can't generate the right call shape.
pub const COROUTINE_BUILDERS: &[&str] = &[
    "runBlocking",
    "launch",
    "async",
    "withTimeout",
    "withTimeoutOrNull",
    "withContext",
    "coroutineScope",
    "supervisorScope",
];

/// Returns true when `name` is a coroutine builder that requires
/// the compose/suspend-state-machine transform to wrap the trailing
/// lambda. Used by mir-lower to decide whether to set
/// `force_suspend_lambda`.
pub fn is_coroutine_builder(name: &str) -> bool {
    COROUTINE_BUILDERS.contains(&name)
}

// ── 2. Compose intrinsics ───────────────────────────────────────

/// Compose-runtime functions whose result type is the trailing
/// lambda's return type. The signature in `compose-runtime.jar`
/// records this via `<T> remember(calc: () -> T): T`, so the
/// unifier in `skotch_classinfo::generic_signature` resolves T
/// correctly *when* the classpath signature is available. We keep
/// this list for the case where signature lookup fails (e.g. the
/// Compose runtime isn't on the classpath in a unit test).
pub const COMPOSE_T_FROM_LAMBDA: &[&str] =
    &["remember", "rememberSaveable", "lazy", "derivedStateOf"];

/// True when the method is a Compose intrinsic whose result type
/// equals its trailing lambda's return type.
pub fn is_compose_t_from_lambda(name: &str) -> bool {
    COMPOSE_T_FROM_LAMBDA.contains(&name)
}

/// Compose-runtime "unit property" extensions: `Int.dp`, `Int.sp`,
/// `Int.em`. These are extension properties on Int returning value
/// classes (`Dp`, `TextUnit`). Until `@Metadata` parsing lands, we
/// recognize them by suffix.
pub const COMPOSE_UNIT_PROPS: &[&str] = &["dp", "sp", "em"];

pub fn is_compose_unit_prop(name: &str) -> bool {
    COMPOSE_UNIT_PROPS.contains(&name)
}

// ── 3. Kotlin → Java type aliases ───────────────────────────────

/// Kotlin source-level type name → JVM internal class path. Sourced
/// from the Kotlin spec (Mapped Types section, §8.2.2). This is a
/// closed set defined by the language; classpath signatures can't
/// replace it.
pub fn kotlin_to_jvm_class(simple_name: &str) -> Option<&'static str> {
    let aliased = match simple_name {
        "Any" => "java/lang/Object",
        "String" => "java/lang/String",
        "CharSequence" => "java/lang/CharSequence",
        "Throwable" => "java/lang/Throwable",
        "Comparable" => "java/lang/Comparable",
        "Enum" => "java/lang/Enum",
        "Number" => "java/lang/Number",
        "List" | "MutableList" => "java/util/List",
        "Map" | "MutableMap" => "java/util/Map",
        "Set" | "MutableSet" => "java/util/Set",
        "Collection" | "MutableCollection" => "java/util/Collection",
        "Iterable" | "MutableIterable" => "java/lang/Iterable",
        "Iterator" | "MutableIterator" => "java/util/Iterator",
        "ListIterator" | "MutableListIterator" => "java/util/ListIterator",
        "Sequence" => "kotlin/sequences/Sequence",
        // Exception/error names are delegated to the single exception
        // table (§7) so they're enumerated in exactly one place.
        _ => return kotlin_exception_class(simple_name),
    };
    Some(aliased)
}

// ── 4. Stdlib collection builders (val-type inference) ──────────

/// Top-level Kotlin stdlib collection constructors. When skotch
/// can't look up the classpath signature (e.g. during val-type
/// inference at gather-declarations time, before classes are
/// loaded), this fallback table maps the well-known builder
/// function name to the resulting collection's class path. The
/// element type comes from the first argument's inferred type.
///
/// Once classpath preloading runs earlier in the pipeline, the
/// signature-based path covers all of these and this table can be
/// retired.
pub fn fallback_collection_builder_class(name: &str) -> Option<&'static str> {
    Some(match name {
        "listOf" | "mutableListOf" => "kotlin/collections/List",
        "setOf" | "mutableSetOf" | "hashSetOf" | "linkedSetOf" => "kotlin/collections/Set",
        "arrayOf" => "kotlin/Array",
        "mapOf" | "mutableMapOf" | "hashMapOf" | "linkedMapOf" => "kotlin/collections/Map",
        // Compose state holders that look like collection builders
        // from the inferrer's perspective (one arg, returns
        // `Wrapper<T>`).
        "mutableStateOf" | "stateOf" => "androidx/compose/runtime/MutableState",
        "mutableStateListOf" => "androidx/compose/runtime/snapshots/SnapshotStateList",
        "mutableStateSetOf" => "androidx/compose/runtime/snapshots/SnapshotStateSet",
        _ => return None,
    })
}

// ── 5. Scope functions ──────────────────────────────────────────

/// How a Kotlin scope function exposes its receiver to the trailing
/// lambda.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeReceiver {
    /// Receiver is bound as `this` inside the lambda (`apply`, `run`).
    This,
    /// Receiver is passed as the `it` parameter (`let`, `also`,
    /// `takeIf`, `takeUnless`).
    It,
}

/// What value a scope-function *call* evaluates to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeResult {
    /// The call returns the lambda's result (`run`, `let`).
    LambdaResult,
    /// The call returns the original receiver (`apply`, `also`).
    Receiver,
    /// The call returns the receiver or null, gated on a predicate
    /// lambda (`takeIf`, `takeUnless`).
    ReceiverIfMatch,
}

/// A Kotlin receiver-extension scope function and how it binds its
/// receiver / computes its result. These are `inline` in stdlib, so
/// once inlining lands the receiver-binding becomes structural rather
/// than name-based and this table can retire.
///
/// `with` is intentionally excluded — it's a two-arg top-level
/// function (`with(receiver) { … }`), not a `receiver.fn { … }`
/// extension, and is registered via [`STDLIB_TOP_LEVEL_NAMES`].
pub struct ScopeFn {
    pub name: &'static str,
    pub receiver: ScopeReceiver,
    pub result: ScopeResult,
}

impl ScopeFn {
    /// Receiver is bound as `this` inside the lambda.
    pub fn binds_this(&self) -> bool {
        self.receiver == ScopeReceiver::This
    }
    /// Receiver is passed as the `it` parameter.
    pub fn binds_it(&self) -> bool {
        self.receiver == ScopeReceiver::It
    }
    /// The lambda is a `(T) -> Boolean` predicate; the call returns the
    /// receiver (or null) rather than consuming the receiver value.
    pub fn is_predicate(&self) -> bool {
        self.result == ScopeResult::ReceiverIfMatch
    }
    /// The call evaluates to the original receiver (`apply`, `also`).
    pub fn returns_receiver(&self) -> bool {
        self.result == ScopeResult::Receiver
    }
}

/// The canonical list of receiver-extension scope functions. This is
/// the single place these names are enumerated.
pub const SCOPE_FNS: &[ScopeFn] = &[
    ScopeFn {
        name: "apply",
        receiver: ScopeReceiver::This,
        result: ScopeResult::Receiver,
    },
    ScopeFn {
        name: "run",
        receiver: ScopeReceiver::This,
        result: ScopeResult::LambdaResult,
    },
    ScopeFn {
        name: "let",
        receiver: ScopeReceiver::It,
        result: ScopeResult::LambdaResult,
    },
    ScopeFn {
        name: "also",
        receiver: ScopeReceiver::It,
        result: ScopeResult::Receiver,
    },
    ScopeFn {
        name: "takeIf",
        receiver: ScopeReceiver::It,
        result: ScopeResult::ReceiverIfMatch,
    },
    ScopeFn {
        name: "takeUnless",
        receiver: ScopeReceiver::It,
        result: ScopeResult::ReceiverIfMatch,
    },
];

/// Look up a receiver-extension scope function by name.
pub fn scope_fn(name: &str) -> Option<&'static ScopeFn> {
    SCOPE_FNS.iter().find(|s| s.name == name)
}

/// True if calling `receiver.<name> { … }` passes receiver as `it`
/// to the lambda (`let`, `also`, `takeIf`, `takeUnless`).
pub fn scope_fn_binds_it(name: &str) -> bool {
    scope_fn(name).is_some_and(ScopeFn::binds_it)
}

/// True for the scope functions whose lambda directly consumes the
/// receiver value and that return either the receiver or the lambda
/// result (`apply`, `run`, `let`, `also`) — every scope function
/// except the `takeIf`/`takeUnless` predicate filters. mir-lower
/// lowers these inline.
pub fn is_value_scope_fn(name: &str) -> bool {
    scope_fn(name).is_some_and(|s| !s.is_predicate())
}

// ── 6. Collection method lambda-receiver propagation ────────────

/// Methods on `Iterable<T>` / `Sequence<T>` whose lambda parameter
/// is `T` — the receiver's element type. Used as a fallback when
/// the classpath signature isn't available; the canonical answer
/// comes from `skotch_classinfo::generic_signature::infer_return_ty`.
pub const ITERABLE_T_LAMBDA: &[&str] = &[
    // Receiver-element-preserving collection methods (result is
    // List<T> or related, lambda is (T) -> R).
    "filter",
    "filterNot",
    "filterIsInstance",
    "filterNotNull",
    "map",
    "mapNotNull",
    "mapIndexed",
    "mapIndexedNotNull",
    "flatMap",
    "forEach",
    "forEachIndexed",
    "onEach",
    "any",
    "all",
    "none",
    "count",
    "first",
    "firstOrNull",
    "last",
    "lastOrNull",
    "find",
    "findLast",
    "single",
    "singleOrNull",
    "partition",
    "groupBy",
    "associateBy",
    "associateWith",
    "sortedBy",
    "sortedByDescending",
    "maxBy",
    "maxByOrNull",
    "minBy",
    "minByOrNull",
    "sumOf",
    "sumBy",
    "takeWhile",
    "dropWhile",
];

pub fn is_iterable_t_lambda_method(name: &str) -> bool {
    ITERABLE_T_LAMBDA.contains(&name)
}

/// Methods that preserve the receiver's element type into the
/// result (`filter` → List<T>, `take` → List<T>, etc.).
pub const ITERABLE_ELEMENT_PRESERVING: &[&str] = &[
    "filter",
    "filterNot",
    "filterNotNull",
    "sortedBy",
    "sortedByDescending",
    "take",
    "drop",
    "takeWhile",
    "dropWhile",
    "reversed",
    "distinct",
    "distinctBy",
];

pub fn is_element_preserving(name: &str) -> bool {
    ITERABLE_ELEMENT_PRESERVING.contains(&name)
}

/// Methods that produce `List<R>` from a lambda `(T) -> R`. The
/// result element type comes from the lambda's body, not the
/// receiver.
pub const ITERABLE_MAP_FAMILY: &[&str] = &["map", "mapNotNull", "mapIndexed", "mapIndexedNotNull"];

pub fn is_map_family(name: &str) -> bool {
    ITERABLE_MAP_FAMILY.contains(&name)
}

// ── 7. Common Kotlin exception types ────────────────────────────

/// Kotlin source-level exception/error simple name → JVM internal
/// class name. This is the single source for exception-constructor
/// codegen (`throw IllegalStateException(...)`, `error()`, `check()`,
/// `TODO()`). Most entries are `typealias`es onto `java.lang.*` /
/// `java.util.*` declared in the Kotlin stdlib (TypeAliases.kt);
/// `NotImplementedError` is a genuine `kotlin.*` class.
///
/// [`kotlin_to_jvm_class`] delegates here for the exception subset, so
/// these names are listed in exactly one place.
pub fn kotlin_exception_class(simple_name: &str) -> Option<&'static str> {
    Some(match simple_name {
        // Kotlin's `Error` typealiases `java.lang.Error`; `AssertionError`
        // is the distinct `java.lang.AssertionError`.
        "Error" => "java/lang/Error",
        "AssertionError" => "java/lang/AssertionError",
        "Exception" => "java/lang/Exception",
        "RuntimeException" => "java/lang/RuntimeException",
        "IllegalArgumentException" => "java/lang/IllegalArgumentException",
        "IllegalStateException" => "java/lang/IllegalStateException",
        "IndexOutOfBoundsException" => "java/lang/IndexOutOfBoundsException",
        "ClassCastException" => "java/lang/ClassCastException",
        "ArithmeticException" => "java/lang/ArithmeticException",
        "NumberFormatException" => "java/lang/NumberFormatException",
        "NullPointerException" => "java/lang/NullPointerException",
        "UnsupportedOperationException" => "java/lang/UnsupportedOperationException",
        "NoSuchElementException" => "java/util/NoSuchElementException",
        "NotImplementedError" => "kotlin/NotImplementedError",
        _ => return None,
    })
}

/// Resolve a `catch (e: T)` declared type to the JVM internal class
/// name written into the exception-handler table. Known stdlib types
/// resolve via [`kotlin_to_jvm_class`]; an unqualified unknown name is
/// assumed to live in `java/lang/`, and an already-qualified name (one
/// containing `/`) passes through unchanged.
pub fn catch_type_to_jvm(name: &str) -> String {
    if let Some(jvm) = kotlin_to_jvm_class(name) {
        return jvm.to_string();
    }
    if name.contains('/') {
        name.to_string()
    } else {
        format!("java/lang/{name}")
    }
}

// ── 7b. Runtime type checks (instanceof / checkcast) ────────────

/// JVM internal class name used for a runtime type check (`x is T`,
/// `x as T`) against Kotlin type `T`. Primitive Kotlin types map to
/// their *boxed* JVM classes — a runtime `is Int` compiles to
/// `instanceof java/lang/Integer` — and `Any` maps to `Object`.
/// Returns `None` for types that aren't built in (user classes,
/// imported types), so the caller can fall back to its own resolution
/// (import map / identity). This is the single source for the
/// boxed-class table used by `is`/`as` lowering.
pub fn runtime_check_jvm_class(name: &str) -> Option<&'static str> {
    Some(match name {
        "String" => "java/lang/String",
        "Any" => "java/lang/Object",
        "Int" => "java/lang/Integer",
        "Long" => "java/lang/Long",
        "Short" => "java/lang/Short",
        "Byte" => "java/lang/Byte",
        "Double" => "java/lang/Double",
        "Float" => "java/lang/Float",
        "Boolean" => "java/lang/Boolean",
        "Char" => "java/lang/Character",
        _ => return None,
    })
}

// ── 8. Top-level stdlib function names ─────────────────────────

/// Top-level Kotlin-stdlib function names that the resolver
/// pre-registers so they bind to a known intrinsic def-id.
/// Used by `skotch_resolve::register_stdlib_top_level_fns`.
///
/// This is the canonical list — anything else that wants to know
/// "is X a stdlib top-level fn?" should consult this. Includes:
///   - Collection builders (also covered by
///     `fallback_collection_builder_class`, but listed here for
///     resolution purposes).
///   - Coroutine builders (also in `COROUTINE_BUILDERS`).
///   - `kotlin.math` functions, IO, exceptions, etc.
pub const STDLIB_TOP_LEVEL_NAMES: &[&str] = &[
    // Functional / scope
    "maxOf",
    "minOf",
    "with",
    "repeat",
    // Collection builders
    "listOf",
    "mutableListOf",
    "mapOf",
    "mutableMapOf",
    "setOf",
    "mutableSetOf",
    "arrayOf",
    "intArrayOf",
    "longArrayOf",
    "doubleArrayOf",
    "booleanArrayOf",
    "byteArrayOf",
    "LongArray",
    "DoubleArray",
    "BooleanArray",
    "ByteArray",
    "buildString",
    "sequence",
    "buildList",
    "buildMap",
    "buildSet",
    "emptyList",
    "emptyMap",
    "emptySet",
    "hashMapOf",
    "hashSetOf",
    "linkedMapOf",
    "sortedMapOf",
    "sortedSetOf",
    // Pair / Triple
    "Pair",
    "Triple",
    // Coroutines
    "runBlocking",
    "delay",
    "launch",
    "async",
    "withContext",
    "coroutineScope",
    "supervisorScope",
    "withTimeout",
    "withTimeoutOrNull",
    "yield",
    // Misc
    "StringBuilder",
    "require",
    "check",
    "error",
    "TODO",
    "lazy",
    "Regex",
    // kotlin.math
    "abs",
    "sqrt",
    "ceil",
    "floor",
    "round",
    "pow",
    "sin",
    "cos",
    "tan",
    "log",
    "log10",
    "exp",
    // IO
    "readLine",
    "readln",
    // Exception constructors
    "IllegalStateException",
    "IllegalArgumentException",
    "RuntimeException",
    "NullPointerException",
    "UnsupportedOperationException",
    "IndexOutOfBoundsException",
    "NoSuchElementException",
    "Exception",
    "AssertionError",
    "NotImplementedError",
];

/// Strip a `Nullable` wrapper from a `Ty` to get the underlying
/// type. Useful when the unifier or call-site narrowing needs to
/// work against the non-null projection (`x?.let { it.foo }` —
/// inside the body, `it` is the non-null form).
pub fn strip_nullable(ty: &Ty) -> Ty {
    match ty {
        Ty::Nullable(inner) => (**inner).clone(),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_coroutine_builders() {
        assert!(is_coroutine_builder("runBlocking"));
        assert!(is_coroutine_builder("withContext"));
        assert!(!is_coroutine_builder("listOf"));
    }

    #[test]
    fn kotlin_alias_resolution() {
        assert_eq!(kotlin_to_jvm_class("String"), Some("java/lang/String"));
        assert_eq!(kotlin_to_jvm_class("MutableList"), Some("java/util/List"));
        assert_eq!(
            kotlin_to_jvm_class("Sequence"),
            Some("kotlin/sequences/Sequence")
        );
        assert_eq!(kotlin_to_jvm_class("UserDefined"), None);
    }

    #[test]
    fn scope_fns_partitioned() {
        assert!(scope_fn("apply").unwrap().binds_this());
        assert!(scope_fn_binds_it("let"));
        assert!(!scope_fn("let").unwrap().binds_this());
        assert!(!scope_fn_binds_it("apply"));
        // `apply`/`also` return the receiver; `run`/`let` return the
        // lambda result; `takeIf`/`takeUnless` are predicates.
        assert!(scope_fn("apply").unwrap().returns_receiver());
        assert!(scope_fn("also").unwrap().returns_receiver());
        assert!(!scope_fn("run").unwrap().returns_receiver());
        assert!(scope_fn("takeIf").unwrap().is_predicate());
        assert!(is_value_scope_fn("let"));
        assert!(!is_value_scope_fn("takeIf"));
        // `with` is not a receiver-extension scope fn.
        assert!(scope_fn("with").is_none());
    }

    #[test]
    fn iterable_method_categorization() {
        assert!(is_iterable_t_lambda_method("filter"));
        assert!(is_element_preserving("filter"));
        assert!(!is_element_preserving("map"));
        assert!(is_map_family("map"));
        assert!(!is_map_family("filter"));
    }

    #[test]
    fn strip_nullable_works() {
        assert_eq!(strip_nullable(&Ty::String), Ty::String);
        assert_eq!(strip_nullable(&Ty::Nullable(Box::new(Ty::Int))), Ty::Int);
    }

    #[test]
    fn exception_table_is_canonical() {
        // Kotlin's `Error` and `AssertionError` are distinct JVM classes.
        assert_eq!(kotlin_exception_class("Error"), Some("java/lang/Error"));
        assert_eq!(
            kotlin_exception_class("AssertionError"),
            Some("java/lang/AssertionError")
        );
        // Non-java.lang exceptions resolve to their real package.
        assert_eq!(
            kotlin_exception_class("NoSuchElementException"),
            Some("java/util/NoSuchElementException")
        );
        assert_eq!(
            kotlin_exception_class("NotImplementedError"),
            Some("kotlin/NotImplementedError")
        );
        assert_eq!(kotlin_exception_class("UserError"), None);
        // `kotlin_to_jvm_class` delegates to the exception table, so it
        // resolves exceptions too (single source of truth).
        assert_eq!(
            kotlin_to_jvm_class("IllegalStateException"),
            Some("java/lang/IllegalStateException")
        );
        assert_eq!(kotlin_to_jvm_class("Error"), Some("java/lang/Error"));
    }

    #[test]
    fn catch_type_resolution() {
        // Known stdlib exceptions resolve via the central table.
        assert_eq!(catch_type_to_jvm("Exception"), "java/lang/Exception");
        assert_eq!(
            catch_type_to_jvm("NoSuchElementException"),
            "java/util/NoSuchElementException"
        );
        // Unqualified user types default to java/lang/.
        assert_eq!(catch_type_to_jvm("MyException"), "java/lang/MyException");
        // Already-qualified names pass through unchanged.
        assert_eq!(catch_type_to_jvm("com/example/Boom"), "com/example/Boom");
    }

    #[test]
    fn runtime_check_boxes_primitives() {
        // `is Int` is `instanceof Integer` at runtime.
        assert_eq!(runtime_check_jvm_class("Int"), Some("java/lang/Integer"));
        assert_eq!(runtime_check_jvm_class("Char"), Some("java/lang/Character"));
        assert_eq!(runtime_check_jvm_class("Any"), Some("java/lang/Object"));
        assert_eq!(runtime_check_jvm_class("String"), Some("java/lang/String"));
        // User types are not built in; caller falls back.
        assert_eq!(runtime_check_jvm_class("Foo"), None);
    }
}
