//! Typed-AST entry point for type checking.
//!
//! Parallel to the legacy [`crate::type_check`] but takes a
//! [`skotch_ast::KtFile`] (typed view over a SIL tree) instead of the
//! Box-tree `&skotch_syntax::KtFile`.
//!
//! ## Current coverage
//!
//! Initial scaffold returns an empty [`crate::TypedFile`]. Each
//! consumer migration step expands the coverage. The same migration
//! pattern as [`skotch_resolve::typed`] applies — fill in the
//! pattern-match arms as upstream callers move to the typed API.

use crate::{Signature, TypedFile, TypedFunction};
use skotch_ast::{AstNode, KtDecl, KtFile};
use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_resolve::{DefId, PackageSymbolTable, ResolvedFile};
use skotch_types::Ty;

/// Type-check a single file using the typed AST input.
///
/// Counterpart of [`crate::type_check`].
///
/// Coverage:
/// - Pass 1: collect [`Signature`] for each top-level function,
///   keyed by [`DefId::Function`]. Parameters use a placeholder
///   [`Ty::Any`] until [`type_ref_to_ty`] is ported to walk typed
///   `KtTypeReference` children.
/// - Build [`TypedFunction`] records mirroring the function order
///   so MIR lowering can index into them.
///
/// Not yet covered:
/// - Function body type inference (the bidirectional check that
///   walks each `KtExpr` and infers/checks against the expected
///   type).
/// - Class / interface / object / enum signatures.
pub fn type_check(
    file: KtFile<'_>,
    _resolved: &ResolvedFile,
    _interner: &mut Interner,
    _diags: &mut Diagnostics,
    _package_symbols: Option<&PackageSymbolTable>,
) -> TypedFile {
    let mut out = TypedFile::default();

    let mut fn_index = 0u32;
    for decl in file.decls() {
        if let KtDecl::Fun(f) = decl {
            // Parameter Ty list (placeholder Ty::Any per param).
            let param_count = f
                .value_parameter_list()
                .map(|pl| {
                    skotch_ast::typed_children::<skotch_ast::KtValueParameter>(pl.syntax()).count()
                })
                .unwrap_or(0);
            let param_tys: Vec<Ty> = (0..param_count).map(|_| Ty::Any).collect();

            // Return type — Ty::Unit when the function has no `: Type`
            // annotation; placeholder Ty::Any when it does (the typed
            // KtTypeReference → Ty conversion is the next migration
            // step).
            let return_ty = if f.return_type().is_some() {
                Ty::Any
            } else {
                Ty::Unit
            };

            let sig = Signature {
                params: param_tys.clone(),
                ret: return_ty.clone(),
            };
            out.top_signatures.insert(DefId::Function(fn_index), sig);
            out.functions.push(TypedFunction {
                name_index: fn_index,
                return_ty,
                param_tys,
                local_tys: Vec::new(),
            });
            fn_index += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_type_check_finds_top_level_fun() {
        let parsed = skotch_ast::parse("test.kt", "fun main() {}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.functions.len(), 1);
        assert_eq!(typed.functions[0].name_index, 0);
        // No `: Type` annotation → Ty::Unit.
        assert!(matches!(typed.functions[0].return_ty, Ty::Unit));
    }

    #[test]
    fn typed_type_check_collects_param_count() {
        let parsed = skotch_ast::parse("test.kt", "fun add(a: Int, b: Int): Int = a + b");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.functions.len(), 1);
        assert_eq!(typed.functions[0].param_tys.len(), 2);
        // `: Int` return annotation → Ty::Any placeholder until type
        // resolution is ported.
        assert!(matches!(typed.functions[0].return_ty, Ty::Any));
    }

    #[test]
    fn typed_type_check_registers_signatures_by_def_id() {
        let parsed = skotch_ast::parse("test.kt", "fun a() {}\nfun b() {}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert!(typed.top_signatures.contains_key(&DefId::Function(0)));
        assert!(typed.top_signatures.contains_key(&DefId::Function(1)));
    }
}
