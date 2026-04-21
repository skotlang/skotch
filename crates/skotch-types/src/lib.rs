//! Type lattice for skotch.
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
        // Any Class is assignable to any other Class (subtyping resolved at runtime).
        // This is permissive — real Kotlin checks the inheritance chain.
        if matches!(self, Ty::Class(_)) && matches!(other, Ty::Class(_)) {
            return true;
        }
        // Non-nullable T is assignable to nullable T?
        if let Ty::Nullable(inner) = other {
            if self == inner.as_ref() {
                return true;
            }
        }
        // Any nullable is assignable to any other nullable
        // (e.g., null literal typed as Nullable(Any) → Nullable(String))
        if matches!(self, Ty::Nullable(_)) && matches!(other, Ty::Nullable(_)) {
            return true;
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
}
