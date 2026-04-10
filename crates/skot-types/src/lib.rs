//! Type lattice for skot.
//!
//! This crate is intentionally tiny: it just defines the [`Ty`] enum and
//! a couple of helpers. There is no inference engine, no unification, no
//! variance — those live in `skot-typeck`. Keeping the lattice in its
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
    /// `Int` — 32-bit signed.
    Int,
    /// `Long` — 64-bit signed. Not produced in PR #1; reserved.
    Long,
    /// `Double` — 64-bit float. Not produced in PR #1; reserved.
    Double,
    /// `String`.
    String,
    /// `Any` — top type.
    Any,
    /// Nullable wrapper. `String?` ≡ `Nullable(String)`.
    Nullable(Box<Ty>),
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
        if matches!(other, Ty::Any) {
            return true;
        }
        if let Ty::Nullable(inner) = other {
            if self == inner.as_ref() {
                return true;
            }
        }
        false
    }

    /// Helper for diagnostics.
    pub fn display_name(&self) -> &'static str {
        match self {
            Ty::Unit => "Unit",
            Ty::Bool => "Boolean",
            Ty::Int => "Int",
            Ty::Long => "Long",
            Ty::Double => "Double",
            Ty::String => "String",
            Ty::Any => "Any",
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
        "Int" => Ty::Int,
        "Long" => Ty::Long,
        "Double" => Ty::Double,
        "String" => Ty::String,
        "Any" => Ty::Any,
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
}
