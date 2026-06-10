//! Typed-AST entry points for name resolution.
//!
//! This module exposes the same shapes as the legacy [`crate`] root —
//! [`PackageSymbolTable`] and [`ResolvedFile`] — but the *input*
//! shifts from `&skotch_syntax::KtFile` (legacy Box-tree AST) to
//! [`skotch_ast::KtFile`] (typed wrapper over the SIL tree).
//!
//! ## Migration path
//!
//! Each public function here is the kotlinc-PSI-derived counterpart
//! of an existing legacy entry point in [`crate`]. Consumers migrate
//! call sites from the legacy version to the typed version, one at a
//! time. When every consumer is on the typed API, the legacy
//! functions and their pattern-match-on-enum bodies come out, along
//! with the `skotch-syntax/ast.rs` types they consumed.
//!
//! ## Current coverage
//!
//! Initial implementation covers the highest-traffic shapes (top-level
//! `fun`/`val`, file-level `package`/`import` walks). Coverage expands
//! as each consumer migration uncovers new requirements.

use crate::{
    DefId, ExternalFunDecl, ExternalValDecl, PackageSymbolTable, ResolvedFile,
};
use skotch_ast::{AstToken, KtDecl, KtFile, KtIdentifier};
use skotch_intern::Interner;
use skotch_syntax::Visibility;
use skotch_types::Ty;

/// Gather top-level declarations across files into a
/// [`PackageSymbolTable`] for cross-file resolution. Typed-AST input
/// counterpart of [`crate::gather_declarations`].
///
/// `files` is a slice of `(file_typed_ast, wrapper_class_name)`. The
/// wrapper class is the synthetic JVM class that file-level
/// `fun`/`val` declarations become static members of.
pub fn gather_declarations<'a>(
    files: &[(KtFile<'a>, &str)],
    interner: &Interner,
) -> PackageSymbolTable {
    let mut table = PackageSymbolTable::default();

    for (file, wrapper_class) in files {
        let pkg_prefix = file
            .package_directive()
            .map(|p| {
                let name = p.name();
                if name.is_empty() {
                    String::new()
                } else {
                    format!("{}/", name.replace('.', "/"))
                }
            })
            .unwrap_or_default();
        let fq_wrapper = format!("{pkg_prefix}{wrapper_class}");

        for decl in file.decls() {
            match decl {
                KtDecl::Fun(f) => {
                    // Public visibility default; `private` modifier excludes.
                    if has_private_modifier(f.modifier_list()) {
                        continue;
                    }
                    let Some(name) = f.name() else { continue };
                    // Minimal descriptor placeholder — the full
                    // descriptor build is a separate migration step.
                    // See `build_descriptor_with_aliases` in the
                    // legacy module for the complete logic.
                    let descriptor = String::from("()V");
                    let ext = ExternalFunDecl {
                        owner_class: fq_wrapper.clone(),
                        descriptor,
                        return_ty: Ty::Unit,
                        param_count: 0,
                        param_tys: Vec::new(),
                        is_suspend: f
                            .modifier_list()
                            .map(|m| m.has_kind(skotch_syntax::SyntaxKind::KW_SUSPEND))
                            .unwrap_or(false),
                        is_inline: f
                            .modifier_list()
                            .map(|m| m.has_kind(skotch_syntax::SyntaxKind::KW_INLINE))
                            .unwrap_or(false),
                        is_extension: false,
                        receiver_ty: None,
                        has_default: Vec::new(),
                        is_vararg: Vec::new(),
                        annotations: Vec::new(),
                    };
                    table
                        .functions
                        .entry(name.to_string())
                        .or_default()
                        .push(ext);
                }
                KtDecl::Property(p) => {
                    if has_private_modifier(p.modifier_list()) {
                        continue;
                    }
                    let Some(name) = p.name() else { continue };
                    table.vals.insert(
                        name.to_string(),
                        ExternalValDecl {
                            owner_class: fq_wrapper.clone(),
                            ty: Ty::Any,
                            annotations: Vec::new(),
                        },
                    );
                }
                // Class / Interface / Object / EnumClass / TypeAlias —
                // future migration steps. The legacy
                // `crate::gather_declarations` handles these; we keep
                // them out of this skeleton until the full descriptor
                // / class-shape machinery is ported over.
                _ => {}
            }
        }
    }

    let _ = interner; // not used yet — the legacy gather uses it for resolving
                     // import aliases and same-package decl simple names.
    table
}

/// Resolve identifier references in a single file. Typed-AST input
/// counterpart of [`crate::resolve_file`].
///
/// Minimal implementation: builds a [`ResolvedFile`] with each
/// top-level `fun`/`val` registered as the corresponding [`DefId`].
/// Body-level resolution (parameter references, local vals, inner
/// scopes, when-arm scopes) is a follow-up migration step.
pub fn resolve_file(
    file: KtFile<'_>,
    interner: &mut Interner,
    _package_symbols: Option<&PackageSymbolTable>,
) -> ResolvedFile {
    let mut out = ResolvedFile::default();

    let println_sym = interner.intern("println");
    out.top_level.insert(println_sym, DefId::PrintlnIntrinsic);
    let print_sym = interner.intern("print");
    out.top_level.insert(print_sym, DefId::PrintlnIntrinsic);

    for (i, decl) in file.decls().enumerate() {
        match decl {
            KtDecl::Fun(f) => {
                if let Some(name) = f.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::Function(i as u32));
                }
            }
            KtDecl::Property(p) => {
                if let Some(name) = p.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::TopLevelVal(i as u32));
                }
            }
            _ => {}
        }
    }

    out
}

fn has_private_modifier(modlist: Option<skotch_ast::KtModifierList<'_>>) -> bool {
    modlist
        .map(|m| m.has_kind(skotch_syntax::SyntaxKind::KW_PRIVATE))
        .unwrap_or(false)
}

// Allow exporting for downstream typed-API consumers that want to
// stay on a single Visibility surface.
#[allow(dead_code)]
fn visibility_from_modifier_list(modlist: Option<skotch_ast::KtModifierList<'_>>) -> Visibility {
    let Some(m) = modlist else {
        return Visibility::Public;
    };
    use skotch_syntax::SyntaxKind as S;
    if m.has_kind(S::KW_PRIVATE) {
        Visibility::Private
    } else if m.has_kind(S::KW_PROTECTED) {
        Visibility::Protected
    } else if m.has_kind(S::KW_INTERNAL) {
        Visibility::Internal
    } else {
        Visibility::Public
    }
}

// Token helper kept here to avoid leaking a private skotch-ast import
// into the legacy resolver: lets us extract IDENTIFIER text from the
// children of a typed wrapper without forcing the caller to know the
// underlying SilNode layout.
#[allow(dead_code)]
fn ident_text<'a>(node: &'a skotch_sil::SilNode) -> Option<&'a str> {
    use skotch_syntax::SyntaxKind;
    for c in skotch_ast::children(node) {
        if c.kind == SyntaxKind::IDENTIFIER {
            if let Some(tok) = KtIdentifier::cast(c) {
                return Some(tok.text());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_gather_finds_top_level_fun() {
        let parsed = skotch_ast::parse("test.kt", "fun greet(): String = \"hi\"\nfun farewell() {}");
        let interner = Interner::new();
        let table = gather_declarations(&[(parsed.file(), "TestKt")], &interner);
        assert!(table.functions.contains_key("greet"));
        assert!(table.functions.contains_key("farewell"));
    }

    #[test]
    fn typed_gather_skips_private_fun() {
        let parsed =
            skotch_ast::parse("test.kt", "private fun hidden() {}\nfun visible() {}");
        let interner = Interner::new();
        let table = gather_declarations(&[(parsed.file(), "TestKt")], &interner);
        assert!(!table.functions.contains_key("hidden"));
        assert!(table.functions.contains_key("visible"));
    }

    #[test]
    fn typed_resolve_assigns_def_ids() {
        let parsed = skotch_ast::parse("test.kt", "fun a() {}\nfun b() {}");
        let mut interner = Interner::new();
        let r = resolve_file(parsed.file(), &mut interner, None);
        let a = interner.intern("a");
        let b = interner.intern("b");
        assert_eq!(r.top_level.get(&a), Some(&DefId::Function(0)));
        assert_eq!(r.top_level.get(&b), Some(&DefId::Function(1)));
    }
}
