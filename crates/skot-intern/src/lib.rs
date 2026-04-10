//! String interner producing cheap [`Symbol`] handles.
//!
//! This is a thin wrapper over `lasso::Rodeo` that hides the lasso API
//! behind a stable type. The rest of the workspace imports `Symbol` and
//! `Interner`; nothing imports `lasso` directly.

use lasso::{Rodeo, Spur};

/// A cheap, copy-able handle to an interned string.
///
/// `Symbol` deliberately does **not** know which interner it came from. The
/// caller is responsible for using a consistent interner per compilation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(Spur);

/// String interner. Insertion-order stable, cheap to clone-by-reference.
pub struct Interner {
    rodeo: Rodeo,
}

impl Interner {
    pub fn new() -> Self {
        Interner {
            rodeo: Rodeo::default(),
        }
    }

    /// Intern `s`, returning a stable [`Symbol`].
    pub fn intern(&mut self, s: &str) -> Symbol {
        Symbol(self.rodeo.get_or_intern(s))
    }

    /// Look up the string for an existing symbol.
    ///
    /// Panics if the symbol does not belong to this interner — symbols are
    /// not portable across interners.
    pub fn resolve(&self, sym: Symbol) -> &str {
        self.rodeo.resolve(&sym.0)
    }

    /// Number of distinct strings interned so far.
    pub fn len(&self) -> usize {
        self.rodeo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rodeo.is_empty()
    }
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_dedupes() {
        let mut i = Interner::new();
        let a = i.intern("hello");
        let b = i.intern("hello");
        let c = i.intern("world");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn resolve_round_trip() {
        let mut i = Interner::new();
        let sym = i.intern("println");
        assert_eq!(i.resolve(sym), "println");
    }
}
