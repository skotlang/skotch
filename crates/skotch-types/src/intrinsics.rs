//! Centralized Kotlin standard-library special-cases that skotch
//! handles by name, in one place so call sites consult an accessor
//! rather than re-listing names inline.
//!
//! Each list below is annotated with whether the **reference compiler
//! (`kotlin/`) hardcodes the same thing**, so it's clear which entries
//! are irreducible language facts versus skotch-only fallbacks that a
//! more complete front-end would infer. Two categories:
//!
//! ## Category A — kotlinc hardcodes these too (keep; mirror kotlinc)
//!
//! The Kotlin → JVM class mappings — §3 type aliases, §7 exception
//! classes, and §7b `is`/`as` boxing — mirror kotlinc's
//! `JavaToKotlinClassMap` (`kotlin/core/compiler.common.jvm/src/org/
//! jetbrains/kotlin/builtins/jvm/JavaToKotlinClassMap.kt`) and
//! `JvmPrimitiveType`. kotlinc maintains the very same ~40-entry table
//! because these are language-level facts, not classpath data. The
//! goal for these is to *match* kotlinc's table, not remove it.
//!
//! ## Category B — kotlinc infers these; skotch falls back by name
//!
//! Scope functions (§5), Compose intrinsics (§2), collection builders
//! (§4) and iterable methods (§6) are NOT special-cased in kotlinc's
//! front-end. kotlinc resolves them by ordinary overload resolution +
//! generic inference against the stdlib it always compiles against,
//! using information skotch cannot yet recover from `.class` files:
//!
//!   * **Scope-fn `this` vs `it` is purely structural in kotlinc.**
//!     `run`/`apply`/`with` take a *receiver* lambda `T.() -> R`;
//!     `let`/`also`/`takeIf` take `(T) -> R`
//!     (`kotlin/libraries/stdlib/src/kotlin/util/Standard.kt`). But
//!     `T.() -> R` and `(T) -> R` erase to the **same** JVM type
//!     (`Function1`); the receiver-ness survives only in Kotlin
//!     `@Metadata` (or a `@kotlin.ExtensionFunctionType` type
//!     annotation), neither of which skotch parses yet (task #297).
//!   * **`remember`/`lazy`/`derivedStateOf`** are ordinary
//!     `inline fun <T> …(): T`; the Compose plugin only rewrites their
//!     bodies at the IR level — the *result type* is plain generic
//!     inference. skotch resolves this through the unifier
//!     ([`skotch_classinfo::generic_signature`]) when the classpath
//!     signature is available and falls back to the name list otherwise.
//!   * **`listOf`/`setOf`/`mapOf`** are ordinary
//!     `fun <T> listOf(vararg e: T): List<T>` (stdlib `Collections.kt`).
//!
//! **Why skotch keeps these at all (the determinism constraint).**
//! kotlinc can avoid name lists because it *always* compiles against
//! `kotlin-stdlib.jar` and reads these facts from it. skotch's golden
//! tests instead compare byte-for-byte against committed `skotch.class`
//! files (`crates/skotch-driver/tests/fixture_compare.rs`), so type
//! inference must be **deterministic and stdlib-independent** — pulling
//! signatures / `@Metadata` from a stdlib jar that may or may not be
//! present in a given environment would make output environment-
//! dependent and drift the goldens. These tables provide that
//! determinism.
//!
//! The deterministic, kotlinc-faithful way to *shrink* Category B is
//! therefore NOT to load the stdlib, but to **embed canonical
//! signatures as data and run the same generic unifier**
//! ([`skotch_classinfo::generic_signature`]) over them — replacing the
//! hand-categorised name lists + bespoke branching with one inference
//! algorithm. (Scope-fn receiver-ness additionally needs `@Metadata`
//! / `Ty::TypeVar` threading, task #297.) That is a deliberate refactor
//! with golden regeneration, not a drop-in change; until it lands these
//! stay as documented fallbacks.
//!
//! Coroutine builders (§1) are a third case: not a *type-inference*
//! special case at all — they drive the suspend bytecode transform,
//! which kotlinc keys off the `suspend` modifier (again `@Metadata`).

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

/// Compose-runtime functions whose result type is their trailing
/// lambda's return type (`<T> remember(calculation: () -> T): T`).
///
/// Returns the canonical JVM generic signature rather than a boolean so
/// the **same generic unifier** (`skotch_classinfo::generic_signature`)
/// that resolves classpath-derived signatures recovers `T` — there is no
/// bespoke "result = lambda return type" branch any more. The caller runs
/// it against the trailing lambda argument (`remember` has variadic
/// leading `key` args, so the lambda is always last).
///
/// kotlinc reads this signature from the `compose-runtime.jar` it always
/// compiles against; skotch prefers the real classpath signature when
/// present and falls back to this embedded copy for determinism (the
/// Compose runtime isn't on the classpath in unit tests). The receiver-
/// less `() -> T` shape and the result-equals-lambda-return fact are
/// library facts, so the name set stays — only the *mechanism* is now the
/// shared unifier rather than a hand-written `match`.
pub fn compose_lambda_result_signature(name: &str) -> Option<&'static str> {
    matches!(
        name,
        "remember" | "rememberSaveable" | "lazy" | "derivedStateOf"
    )
    .then_some("<T:Ljava/lang/Object;>(Lkotlin/jvm/functions/Function0<+TT;>;)TT;")
}

/// Compose-runtime "unit property" extensions: `Int.dp`, `Int.sp`,
/// `Int.em`. These are extension properties on Int returning value
/// classes (`Dp`, `TextUnit`). Until `@Metadata` parsing lands, we
/// recognize them by suffix.
pub const COMPOSE_UNIT_PROPS: &[&str] = &["dp", "sp", "em"];

pub fn is_compose_unit_prop(name: &str) -> bool {
    COMPOSE_UNIT_PROPS.contains(&name)
}

/// Compose state-holder builders whose result wraps the *first*
/// argument's type: `mutableStateOf(v)` → `MutableState<typeof v>`,
/// `mutableStateListOf(a, b)` → `SnapshotStateList<typeof a>`. The
/// element type flows from the first arg onto the result local's
/// `local_generic_args` side channel so `state.value` resolves to the
/// right type downstream. (The nominal class for each is listed in
/// [`fallback_collection_builder_class`].)
pub const COMPOSE_STATE_HOLDERS: &[&str] = &[
    "mutableStateOf",
    "stateOf",
    "mutableStateListOf",
    "mutableStateSetOf",
];

pub fn is_compose_state_holder(name: &str) -> bool {
    COMPOSE_STATE_HOLDERS.contains(&name)
}

// ── 3. Kotlin → Java type aliases ───────────────────────────────

/// Kotlin source-level type name → JVM internal class path.
///
/// **kotlinc hardcodes the same table** in `JavaToKotlinClassMap`
/// (`kotlin/core/compiler.common.jvm/src/org/jetbrains/kotlin/builtins/
/// jvm/JavaToKotlinClassMap.kt`, the `init {}` block): `Any`, `String`,
/// `CharSequence`, `Throwable`, `Cloneable`, `Number`, `Comparable`,
/// `Enum`, `Annotation`, plus the read-only/mutable collection
/// "mutability mappings" (List, Set, Map, …). This is a closed set of
/// language-level facts, not classpath data — so this is the *correct*
/// category to keep hardcoded; the aim is to mirror kotlinc's table.
pub fn kotlin_to_jvm_class(simple_name: &str) -> Option<&'static str> {
    let aliased = match simple_name {
        "Any" => "java/lang/Object",
        "String" => "java/lang/String",
        "CharSequence" => "java/lang/CharSequence",
        "Throwable" => "java/lang/Throwable",
        "Cloneable" => "java/lang/Cloneable",
        "Annotation" => "java/lang/annotation/Annotation",
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

/// Inferred type of a collection-builder call given its element type:
/// `Ty::Generic { <builder class>, [elem_ty] }`, or `None` when `name`
/// isn't a builder. This is the single place the wrapping is built — the
/// val-type inferrers in `skotch-mir-lower` (`infer_top_level_val_ty`)
/// and `skotch-resolve` (`infer_val_type_from_init`) both call it with
/// their own first-argument element type, so the mir-lower side and the
/// cross-file side stay in agreement without duplicating the
/// `fallback_collection_builder_class` → `Ty::Generic` construction.
pub fn collection_builder_result_ty(name: &str, elem_ty: Ty) -> Option<Ty> {
    let class = fallback_collection_builder_class(name)?;
    Some(Ty::Generic {
        base: Box::new(Ty::Class(class.to_string())),
        args: vec![elem_ty],
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
/// receiver / computes its result.
///
/// In kotlinc the `this`-vs-`it` choice is *not* name-based: it falls
/// out of the block parameter's type — `run`/`apply` declare
/// `block: T.() -> R` (receiver lambda → `this`), `let`/`also` declare
/// `block: (T) -> R` (→ `it`) (stdlib `Standard.kt`). skotch keeps the
/// table because `T.() -> R` and `(T) -> R` erase to the same JVM
/// `Function1`, so the receiver-ness is unrecoverable from `.class`
/// files without `@Metadata` parsing (task #297). Once that lands this
/// table can retire in favour of structural detection.
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

/// Generic signature of a §6 collection method whose *result element
/// type* the shared unifier (`skotch_classinfo::generic_signature`)
/// recovers. This replaces the former `is_element_preserving` /
/// `is_map_family` boolean lists plus the bespoke "result elem = receiver
/// elem vs lambda return" branch in mir-lower with one signature-driven
/// path. Two shapes:
///
/// * **element-preserving** (`filter`, `take`, `reversed`, …): result is
///   `List<T>` where `T` is the receiver's element —
///   `(Iterable<T>) -> List<T>`.
/// * **map-family** (`map`, `mapNotNull`, …): result is `List<R>` where
///   `R` is the transform lambda's return — `(Iterable<T>, (T)->R) -> List<R>`.
///
/// kotlinc reads these from `kotlin-stdlib` and resolves the element by
/// ordinary generic inference; skotch uses the real classpath signature
/// when present and this embedded copy as the deterministic fallback. Only
/// the receiver-`T` / lambda-`R` binding matters here, so surplus value
/// args (`take`'s `Int`, `mapIndexed`'s index) are omitted — the unifier
/// ignores extra actuals.
pub fn iterable_result_signature(name: &str) -> Option<&'static str> {
    const ELEMENT_PRESERVING: &str =
        "<T:Ljava/lang/Object;>(Ljava/lang/Iterable<TT;>;)Ljava/util/List<TT;>;";
    const MAP_FAMILY: &str = "<T:Ljava/lang/Object;R:Ljava/lang/Object;>\
        (Ljava/lang/Iterable<TT;>;Lkotlin/jvm/functions/Function1<-TT;+TR;>;)\
        Ljava/util/List<TR;>;";
    match name {
        "filter" | "filterNot" | "filterNotNull" | "sortedBy" | "sortedByDescending" | "take"
        | "drop" | "takeWhile" | "dropWhile" | "reversed" | "distinct" | "distinctBy" => {
            Some(ELEMENT_PRESERVING)
        }
        "map" | "mapNotNull" | "mapIndexed" | "mapIndexedNotNull" => Some(MAP_FAMILY),
        _ => None,
    }
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
        // Mappings mirrored from kotlinc's JavaToKotlinClassMap.init{}.
        assert_eq!(
            kotlin_to_jvm_class("Cloneable"),
            Some("java/lang/Cloneable")
        );
        assert_eq!(
            kotlin_to_jvm_class("Annotation"),
            Some("java/lang/annotation/Annotation")
        );
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
        // Result-element signatures: element-preserving methods keep the
        // receiver's `T`; the map family returns `List<R>` from the lambda.
        assert_eq!(
            iterable_result_signature("filter"),
            iterable_result_signature("take"),
            "all element-preserving methods share one signature"
        );
        assert_ne!(
            iterable_result_signature("filter"),
            iterable_result_signature("map"),
            "element-preserving and map-family signatures differ"
        );
        assert!(iterable_result_signature("map").unwrap().contains("TR;"));
        assert!(iterable_result_signature("not_a_method").is_none());
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

    #[test]
    fn compose_state_holders_recognized() {
        assert!(is_compose_state_holder("mutableStateOf"));
        assert!(is_compose_state_holder("mutableStateListOf"));
        assert!(!is_compose_state_holder("listOf"));
        // Every state holder also has a nominal class in the builder table.
        for name in COMPOSE_STATE_HOLDERS {
            assert!(fallback_collection_builder_class(name).is_some());
        }
    }

    #[test]
    fn collection_builder_result_ty_wraps_element() {
        // `listOf(1, 2)` infers `List<Int>` — the shared helper both
        // mir-lower and resolve route through.
        assert_eq!(
            collection_builder_result_ty("listOf", Ty::Int),
            Some(Ty::Generic {
                base: Box::new(Ty::Class("kotlin/collections/List".to_string())),
                args: vec![Ty::Int],
            })
        );
        assert_eq!(
            collection_builder_result_ty("mutableSetOf", Ty::String),
            Some(Ty::Generic {
                base: Box::new(Ty::Class("kotlin/collections/Set".to_string())),
                args: vec![Ty::String],
            })
        );
        // Non-builders get nothing.
        assert_eq!(collection_builder_result_ty("println", Ty::Int), None);
    }
}
