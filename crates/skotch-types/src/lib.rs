//! Type lattice for skotch.
//!
//! # Soundness Invariant
//!
//! **The type system MUST be sound.** This means:
//!
//! - [`Ty::assignable_to_in`] is the primary assignability check and
//!   must never allow a value of type A to be used where type B is
//!   expected unless A is genuinely a subtype of B.
//! - `Class("X")` is NOT assignable to `Class("Y")` unless X is the
//!   same class as, or a subclass/implementor of, Y (verified by the
//!   `is_subclass` callback from the class hierarchy).
//! - `Nullable(T)` is only assignable to `Nullable(U)` if T → U.
//! - `null` has type `Nullable(Nothing)`, NOT `Nullable(Any)`.
//! - Unknown/unresolvable types use `Ty::Error` (not `Ty::Any`) to
//!   suppress cascading diagnostics without silently accepting bad code.
//!
//! The `#[cfg(test)]` soundness invariant tests at the bottom of this
//! file **must never be weakened, loosened, or removed**. Any change
//! that makes one of them fail is a soundness regression.
//!
//! # Architecture
//!
//! This crate is intentionally tiny: it just defines the [`Ty`] enum and
//! a couple of helpers. There is no inference engine, no unification, no
//! variance — those live in `skotch-typeck`. Keeping the lattice in its
//! own crate lets backends import the types directly without depending
//! on the type-checker.

/// Surface type after the typeck pass.
///
/// PR #1 supports a tiny set; the remaining variants are placeholders
/// the parser/typeck can produce when they encounter unsupported syntax,
/// so error recovery doesn't blow up.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Ty {
    /// Kotlin's `Unit` (≡ Java `void`).
    Unit,
    /// `Boolean`.
    Bool,
    /// `Byte` — 8-bit signed. JVM type `B`.
    Byte,
    /// `Short` — 16-bit signed. JVM type `S`.
    Short,
    /// `Char` — 16-bit unsigned (UTF-16 code unit). JVM type `C`.
    Char,
    /// `Int` — 32-bit signed.
    Int,
    /// `Float` — 32-bit float. JVM type `F`.
    Float,
    /// `Long` — 64-bit signed.
    Long,
    /// `Double` — 64-bit float.
    Double,
    /// `String`.
    String,
    /// `Any` — top type.
    Any,
    /// `Nothing` — bottom type. No value has this type.
    /// `throw` expressions synthesize Nothing, and functions that always
    /// throw (like `error()`, `TODO()`) return Nothing.
    Nothing,
    /// `IntArray` — primitive int array (`int[]` on JVM).
    IntArray,
    /// `LongArray` — primitive long array (`long[]` on JVM).
    LongArray,
    /// `DoubleArray` — primitive double array (`double[]` on JVM).
    DoubleArray,
    /// `BooleanArray` — primitive boolean array (`boolean[]` on JVM).
    BooleanArray,
    /// `ByteArray` — primitive byte array (`byte[]` on JVM).
    ByteArray,
    /// Nullable wrapper. `String?` ≡ `Nullable(String)`.
    Nullable(Box<Ty>),
    /// A user-defined class type. Carries the fully-qualified class name.
    Class(std::string::String),
    /// Function type: `(Int, String) -> Boolean`. Used for lambda parameters.
    /// When `is_suspend` is true, this represents a `suspend` function type
    /// (e.g. `suspend () -> String`). On the JVM the arity is bumped by +1
    /// for the implicit `Continuation` parameter.
    Function {
        params: Vec<Ty>,
        ret: Box<Ty>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_suspend: bool,
    },
    /// Sentinel emitted when type-checking fails for an expression. The
    /// downstream pass should propagate it without complaining.
    Error,
}

impl Ty {
    /// Is `self` assignable to `other`? PR #1 only needs the trivial
    /// reflexive case plus `Int → Any` and `String → Any`.
    pub fn assignable_to(&self, other: &Ty) -> bool {
        if self == other {
            return true;
        }
        // Nothing is the bottom type — assignable to everything.
        if matches!(self, Ty::Nothing) {
            return true;
        }
        if matches!(other, Ty::Any) {
            return true;
        }
        // Any is assignable to Function (lambda can satisfy function type)
        if matches!(self, Ty::Any) && matches!(other, Ty::Function { .. }) {
            return true;
        }
        // Function is assignable to Any (erased)
        if matches!(self, Ty::Function { .. }) && matches!(other, Ty::Any) {
            return true;
        }
        // Class subtyping: defer to the external hierarchy checker when
        // provided (see `assignable_to_in`). Without a hierarchy, we
        // only allow identity (caught above by self == other).
        // The `assignable_to_in` method is the sound entry point;
        // this method is a conservative fallback for contexts where
        // no hierarchy is available.

        // Non-nullable T is assignable to nullable T?
        if let Ty::Nullable(inner) = other {
            if self.assignable_to(inner.as_ref()) {
                return true;
            }
        }
        // Nullable(T) is assignable to Nullable(U) iff T is assignable to U.
        if let (Ty::Nullable(self_inner), Ty::Nullable(other_inner)) = (self, other) {
            return self_inner.assignable_to(other_inner);
        }
        false
    }

    /// Sound assignability check with class-hierarchy awareness.
    ///
    /// `is_subclass(child, parent) -> bool` should return true when
    /// `child` is the same class as or a subclass/implementor of
    /// `parent`. The callback receives JVM-internal class names
    /// (e.g. `"java/lang/String"`).
    ///
    /// This is the **primary entry point** for type checking.
    /// The plain `assignable_to` is a conservative fallback for
    /// contexts where no hierarchy is available.
    pub fn assignable_to_in(&self, other: &Ty, is_subclass: &dyn Fn(&str, &str) -> bool) -> bool {
        if self == other {
            return true;
        }
        if matches!(self, Ty::Nothing) {
            return true;
        }
        if matches!(other, Ty::Any) {
            return true;
        }
        // Error propagation — never produce cascading diagnostics.
        if matches!(self, Ty::Error) || matches!(other, Ty::Error) {
            return true;
        }
        if matches!(self, Ty::Any) && matches!(other, Ty::Function { .. }) {
            return true;
        }
        if matches!(self, Ty::Function { .. }) && matches!(other, Ty::Any) {
            return true;
        }
        // Sound class subtyping via the hierarchy callback.
        if let (Ty::Class(child), Ty::Class(parent)) = (self, other) {
            return is_subclass(child, parent);
        }
        // Class is assignable to Any (already handled above).
        // Non-nullable T is assignable to nullable T?
        if let Ty::Nullable(inner) = other {
            if self.assignable_to_in(inner.as_ref(), is_subclass) {
                return true;
            }
        }
        // Nullable(T) → Nullable(U) iff T → U.
        if let (Ty::Nullable(s), Ty::Nullable(o)) = (self, other) {
            return s.assignable_to_in(o, is_subclass);
        }
        false
    }

    /// Helper for diagnostics.
    pub fn display_name(&self) -> &'static str {
        match self {
            Ty::Unit => "Unit",
            Ty::Bool => "Boolean",
            Ty::Byte => "Byte",
            Ty::Short => "Short",
            Ty::Char => "Char",
            Ty::Int => "Int",
            Ty::Float => "Float",
            Ty::Long => "Long",
            Ty::Double => "Double",
            Ty::String => "String",
            Ty::Any => "Any",
            Ty::Nothing => "Nothing",
            Ty::IntArray => "IntArray",
            Ty::LongArray => "LongArray",
            Ty::DoubleArray => "DoubleArray",
            Ty::BooleanArray => "BooleanArray",
            Ty::ByteArray => "ByteArray",
            Ty::Class(_) => "<class>",
            Ty::Function { .. } => "<function>",
            Ty::Nullable(_) => "<nullable>",
            Ty::Error => "<error>",
        }
    }
}

/// Convenience: build a `Ty` from a Kotlin source-level type name. Used
/// by both the typeck and the (later) build-script DSL walker.
pub fn ty_from_name(name: &str) -> Option<Ty> {
    Some(match name {
        "Unit" => Ty::Unit,
        "Boolean" => Ty::Bool,
        "Byte" => Ty::Byte,
        "Short" => Ty::Short,
        "Char" => Ty::Char,
        "Int" => Ty::Int,
        "Float" => Ty::Float,
        "Long" => Ty::Long,
        "Double" => Ty::Double,
        "String" => Ty::String,
        "Any" => Ty::Any,
        "Nothing" => Ty::Nothing,
        "IntArray" => Ty::IntArray,
        "LongArray" => Ty::LongArray,
        "DoubleArray" => Ty::DoubleArray,
        "BooleanArray" => Ty::BooleanArray,
        "ByteArray" => Ty::ByteArray,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignable_reflexive() {
        assert!(Ty::Int.assignable_to(&Ty::Int));
        assert!(Ty::String.assignable_to(&Ty::String));
    }

    #[test]
    fn assignable_to_any() {
        assert!(Ty::Int.assignable_to(&Ty::Any));
        assert!(Ty::String.assignable_to(&Ty::Any));
    }

    #[test]
    fn nullable_wrapping() {
        assert!(Ty::String.assignable_to(&Ty::Nullable(Box::new(Ty::String))));
    }

    #[test]
    fn nothing_is_bottom_type() {
        // Nothing is assignable to everything.
        assert!(Ty::Nothing.assignable_to(&Ty::Int));
        assert!(Ty::Nothing.assignable_to(&Ty::String));
        assert!(Ty::Nothing.assignable_to(&Ty::Any));
        assert!(Ty::Nothing.assignable_to(&Ty::Unit));
        assert!(Ty::Nothing.assignable_to(&Ty::Nothing));
        assert!(Ty::Nothing.assignable_to(&Ty::Nullable(Box::new(Ty::String))));
    }

    #[test]
    fn nothing_from_name() {
        assert_eq!(ty_from_name("Nothing"), Some(Ty::Nothing));
    }

    // ═══════════════════════════════════════════════════════════════════
    // SOUNDNESS INVARIANT TESTS
    //
    // These tests establish the type-system soundness baseline.
    // They MUST NEVER be weakened, loosened, or removed. Any change
    // that makes one of these tests fail is a soundness regression
    // and must be treated as a P0 bug.
    // ═══════════════════════════════════════════════════════════════════

    /// Sound subtype check for tests: walk a list of (child, parent) pairs.
    fn check_subclass(pairs: &[(&str, &str)], child: &str, parent: &str) -> bool {
        if child == parent {
            return true;
        }
        let mut current = child;
        for _ in 0..20 {
            // depth guard
            if let Some((_, sup)) = pairs.iter().find(|(c, _)| *c == current) {
                if *sup == parent {
                    return true;
                }
                current = sup;
            } else {
                return false;
            }
        }
        false
    }

    // ── INVARIANT: Class assignability requires subtype evidence ────

    #[test]
    fn soundness_class_not_assignable_to_unrelated_class() {
        // Without hierarchy info, Class("A") is NOT assignable to Class("B").
        let a = Ty::Class("A".to_string());
        let b = Ty::Class("B".to_string());
        assert!(
            !a.assignable_to(&b),
            "unrelated classes must not be assignable"
        );
        assert!(
            !b.assignable_to(&a),
            "unrelated classes must not be assignable"
        );
    }

    #[test]
    fn soundness_class_assignable_with_hierarchy() {
        // With hierarchy evidence, subclass IS assignable to superclass.
        let child = Ty::Class("Dog".to_string());
        let parent = Ty::Class("Animal".to_string());
        let pairs = [("Dog", "Animal")];
        let hierarchy = |c: &str, p: &str| check_subclass(&pairs, c, p);
        assert!(
            child.assignable_to_in(&parent, &hierarchy),
            "subclass must be assignable to superclass"
        );
        assert!(
            !parent.assignable_to_in(&child, &hierarchy),
            "superclass must NOT be assignable to subclass"
        );
    }

    #[test]
    fn soundness_class_transitive_hierarchy() {
        let a = Ty::Class("Poodle".to_string());
        let _b = Ty::Class("Dog".to_string());
        let c = Ty::Class("Animal".to_string());
        let pairs = [("Poodle", "Dog"), ("Dog", "Animal")];
        let h = |c: &str, p: &str| check_subclass(&pairs, c, p);
        assert!(a.assignable_to_in(&c, &h), "transitive subtyping");
        assert!(!c.assignable_to_in(&a, &h), "no reverse transitive");
    }

    // ── INVARIANT: Nullable soundness ──────────────────────────────

    #[test]
    fn soundness_nullable_inner_must_match() {
        // Nullable(Int) is NOT assignable to Nullable(String).
        let ni = Ty::Nullable(Box::new(Ty::Int));
        let ns = Ty::Nullable(Box::new(Ty::String));
        assert!(
            !ni.assignable_to(&ns),
            "Nullable(Int) must not be assignable to Nullable(String)"
        );
        assert!(
            !ns.assignable_to(&ni),
            "Nullable(String) must not be assignable to Nullable(Int)"
        );
    }

    #[test]
    fn soundness_nullable_nothing_is_universal_null() {
        // Nullable(Nothing) — the type of `null` — is assignable to any Nullable.
        let null_ty = Ty::Nullable(Box::new(Ty::Nothing));
        assert!(null_ty.assignable_to(&Ty::Nullable(Box::new(Ty::String))));
        assert!(null_ty.assignable_to(&Ty::Nullable(Box::new(Ty::Int))));
        assert!(null_ty.assignable_to(&Ty::Nullable(Box::new(Ty::Any))));
    }

    #[test]
    fn soundness_non_nullable_not_assignable_to_wrong_nullable() {
        // Int is assignable to Int? but NOT to String?.
        assert!(Ty::Int.assignable_to(&Ty::Nullable(Box::new(Ty::Int))));
        assert!(!Ty::Int.assignable_to(&Ty::Nullable(Box::new(Ty::String))));
    }

    // ── INVARIANT: Primitive types are distinct ────────────────────

    #[test]
    fn soundness_primitives_not_interchangeable() {
        assert!(!Ty::Int.assignable_to(&Ty::Long));
        assert!(!Ty::Long.assignable_to(&Ty::Int));
        assert!(!Ty::Int.assignable_to(&Ty::String));
        assert!(!Ty::String.assignable_to(&Ty::Int));
        assert!(!Ty::Bool.assignable_to(&Ty::Int));
        assert!(!Ty::Int.assignable_to(&Ty::Double));
        assert!(!Ty::Float.assignable_to(&Ty::Double));
    }

    // ── INVARIANT: Error type suppresses cascading diagnostics ────

    #[test]
    fn soundness_error_is_compatible_with_everything_in_hierarchy() {
        let pairs: [(&str, &str); 0] = [];
        let h = |c: &str, p: &str| check_subclass(&pairs, c, p);
        assert!(Ty::Error.assignable_to_in(&Ty::Int, &h));
        assert!(Ty::Int.assignable_to_in(&Ty::Error, &h));
        assert!(Ty::Error.assignable_to_in(&Ty::Error, &h));
    }

    // ── INVARIANT: Any is top, Nothing is bottom ──────────────────

    #[test]
    fn soundness_any_is_not_assignable_to_specific() {
        // Any is NOT assignable to Int (would be unsound widening).
        assert!(!Ty::Any.assignable_to(&Ty::Int));
        assert!(!Ty::Any.assignable_to(&Ty::String));
    }

    #[test]
    fn soundness_nothing_assignable_to_all() {
        assert!(Ty::Nothing.assignable_to(&Ty::Int));
        assert!(Ty::Nothing.assignable_to(&Ty::String));
        assert!(Ty::Nothing.assignable_to(&Ty::Any));
        assert!(Ty::Nothing.assignable_to(&Ty::Class("Foo".to_string())));
    }
}
