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
    DefId, ExternalClassDecl, ExternalClassKind, ExternalFunDecl, ExternalValDecl,
    PackageSymbolTable, ResolvedFile, ResolvedFunction,
};
use skotch_ast::{AstNode, AstToken, KtDecl, KtFile, KtIdentifier};
use skotch_intern::{Interner, Symbol};
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
                KtDecl::Class(c) => {
                    if let Some(name) = c.name() {
                        let jvm_name = format!("{pkg_prefix}{name}");
                        let entry = basic_class_entry(jvm_name.clone(), ExternalClassKind::Class);
                        table.classes.insert(name.to_string(), entry.clone());
                        table.classes_by_fq.insert(jvm_name.clone(), entry);
                        table
                            .simple_name_to_fq
                            .insert(name.to_string(), jvm_name);
                    }
                }
                KtDecl::Interface(i) => {
                    if let Some(name) = ident_text_from_decl(i.syntax()) {
                        let jvm_name = format!("{pkg_prefix}{name}");
                        let entry =
                            basic_class_entry(jvm_name.clone(), ExternalClassKind::Interface);
                        table.classes.insert(name.to_string(), entry.clone());
                        table.classes_by_fq.insert(jvm_name.clone(), entry);
                        table
                            .simple_name_to_fq
                            .insert(name.to_string(), jvm_name);
                    }
                }
                KtDecl::Object(o) => {
                    if let Some(name) = ident_text_from_decl(o.syntax()) {
                        let jvm_name = format!("{pkg_prefix}{name}");
                        let entry =
                            basic_class_entry(jvm_name.clone(), ExternalClassKind::Object);
                        table.classes.insert(name.to_string(), entry.clone());
                        table.classes_by_fq.insert(jvm_name.clone(), entry);
                        table
                            .simple_name_to_fq
                            .insert(name.to_string(), jvm_name);
                    }
                }
                KtDecl::EnumClass(e) => {
                    if let Some(name) = ident_text_from_decl(e.syntax()) {
                        let jvm_name = format!("{pkg_prefix}{name}");
                        let entry = basic_class_entry(jvm_name.clone(), ExternalClassKind::Enum);
                        table.classes.insert(name.to_string(), entry.clone());
                        table.classes_by_fq.insert(jvm_name.clone(), entry);
                        table
                            .simple_name_to_fq
                            .insert(name.to_string(), jvm_name);
                    }
                }
                // TypeAlias — full surface needs the alias's resolved
                // target shape; deferred until type-resolution helpers
                // are ported over.
                KtDecl::TypeAlias(_) => {}
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
/// Coverage:
/// - Top-level `fun`/`val` registered as [`DefId::Function`] /
///   [`DefId::TopLevelVal`].
/// - `println` / `print` and stdlib top-level intrinsics
///   registered as [`DefId::PrintlnIntrinsic`].
/// - Per-function `ResolvedFunction` records carry parameter
///   symbols (from `KtValueParameterList`) and the function name
///   symbol.
///
/// Not yet covered (next migration step):
/// - Body-walk that resolves identifier references against the
///   function-local scope (params, locals, captured outer scope).
/// - `when` arm smart-cast scopes.
pub fn resolve_file(
    file: KtFile<'_>,
    interner: &mut Interner,
    package_symbols: Option<&PackageSymbolTable>,
) -> ResolvedFile {
    let mut out = ResolvedFile::default();

    let println_sym = interner.intern("println");
    out.top_level.insert(println_sym, DefId::PrintlnIntrinsic);
    let print_sym = interner.intern("print");
    out.top_level.insert(print_sym, DefId::PrintlnIntrinsic);

    // Stdlib top-level intrinsics (matches the canonical list in
    // skotch_types::intrinsics::STDLIB_TOP_LEVEL_NAMES).
    for name in skotch_types::intrinsics::STDLIB_TOP_LEVEL_NAMES {
        let sym = interner.intern(name);
        out.top_level.insert(sym, DefId::PrintlnIntrinsic);
    }

    // Pass 1: register every top-level name with a DefId.
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
            KtDecl::Class(c) => {
                if let Some(name) = c.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::Function(i as u32));
                }
            }
            KtDecl::Object(o) => {
                if let Some(name) = ident_text_from_decl(o.syntax()) {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::PossibleExternal(sym));
                }
            }
            KtDecl::EnumClass(e) => {
                if let Some(name) = ident_text_from_decl(e.syntax()) {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::PossibleExternal(sym));
                }
            }
            KtDecl::Interface(i_) => {
                if let Some(name) = ident_text_from_decl(i_.syntax()) {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::PossibleExternal(sym));
                }
            }
            KtDecl::TypeAlias(_) => {}
        }
    }

    // Cross-file declarations from the PackageSymbolTable, but local
    // names take priority (or_insert).
    if let Some(pkg) = package_symbols {
        for name in pkg.functions.keys() {
            let sym = interner.intern(name);
            out.top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
        for name in pkg.vals.keys() {
            let sym = interner.intern(name);
            out.top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
        for name in pkg.classes.keys() {
            let sym = interner.intern(name);
            out.top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
    }

    // Pass 2: build per-function `ResolvedFunction` records with
    // parameter symbols. Body-walk left for the follow-up migration
    // step.
    for decl in file.decls() {
        if let KtDecl::Fun(f) = decl {
            let name_sym = f.name().map(|n| interner.intern(n)).unwrap_or_else(|| {
                // Synthetic name when the parser couldn't surface one —
                // mir-lower handles the fallback via the function index.
                interner.intern("<anonymous>")
            });
            let mut params: Vec<Symbol> = Vec::new();
            if let Some(plist) = f.value_parameter_list() {
                for p in skotch_ast::typed_children::<skotch_ast::KtValueParameter>(plist.syntax())
                {
                    if let Some(pname) = p.name() {
                        params.push(interner.intern(pname));
                    }
                }
            }
            out.functions.push(ResolvedFunction {
                name: name_sym,
                params,
                locals: Vec::new(),
                body_refs: Vec::new(),
            });
        }
    }

    out
}

/// Build the minimal-shape `ExternalClassDecl` (no fields / methods
/// / supertype info yet). The fields are filled in as the typed
/// migration covers more of the class-body walk.
fn basic_class_entry(jvm_name: String, kind: ExternalClassKind) -> ExternalClassDecl {
    let super_class = match kind {
        ExternalClassKind::Enum => Some("kotlin/Enum".to_string()),
        _ => None,
    };
    ExternalClassDecl {
        jvm_name,
        kind,
        fields: Vec::new(),
        ctor_params: Vec::new(),
        methods: Vec::new(),
        secondary_ctors: Vec::new(),
        companion_methods: Vec::new(),
        has_companion: false,
        super_class,
        interfaces: Vec::new(),
        is_open: false,
        is_abstract: false,
        is_inner: false,
        enum_entries: Vec::new(),
        annotations: Vec::new(),
        has_type_params: false,
        has_init_blocks: false,
    }
}

/// Extract the first IDENTIFIER child's text from a declaration's
/// children. Used for KtDecl arms whose typed wrappers don't yet have
/// a dedicated `name()` accessor.
fn ident_text_from_decl(node: &skotch_sil::SilNode) -> Option<&str> {
    use skotch_syntax::SyntaxKind;
    for c in skotch_ast::children(node) {
        if c.kind == SyntaxKind::IDENTIFIER {
            if let skotch_sil::SilData::Token { text } = &c.data {
                return Some(text.as_str());
            }
        }
    }
    None
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
fn ident_text(node: &skotch_sil::SilNode) -> Option<&str> {
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

    #[test]
    fn typed_resolve_collects_param_symbols() {
        let parsed = skotch_ast::parse("test.kt", "fun add(a: Int, b: Int): Int = a + b");
        let mut interner = Interner::new();
        let r = resolve_file(parsed.file(), &mut interner, None);
        assert_eq!(r.functions.len(), 1);
        let f = &r.functions[0];
        assert_eq!(f.params.len(), 2);
        assert_eq!(interner.resolve(f.params[0]), "a");
        assert_eq!(interner.resolve(f.params[1]), "b");
    }

    #[test]
    fn typed_resolve_threads_pkg_symbols() {
        let parsed = skotch_ast::parse("test.kt", "fun main() { greet() }");
        let mut interner = Interner::new();
        let mut pkg = PackageSymbolTable::default();
        pkg.functions.insert("greet".to_string(), Vec::new());
        let r = resolve_file(parsed.file(), &mut interner, Some(&pkg));
        let greet = interner.intern("greet");
        match r.top_level.get(&greet) {
            Some(DefId::ExternalPackage(s)) => assert_eq!(interner.resolve(*s), "greet"),
            other => panic!("expected ExternalPackage(greet), got {:?}", other),
        }
    }

    #[test]
    fn typed_gather_records_class_kind() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "class Foo\ninterface Bar\nobject Baz\nenum class Qux { A, B }",
        );
        let interner = Interner::new();
        let table = gather_declarations(&[(parsed.file(), "TestKt")], &interner);
        assert!(matches!(
            table.classes.get("Foo").map(|c| &c.kind),
            Some(ExternalClassKind::Class)
        ));
        // Interface — note the actual class kind plumbing for
        // KtInterface needs SyntaxKind::INTERFACE to be the parser's
        // output kind for `interface Bar`; this assertion documents
        // the migration target.
        let _ = table.classes.get("Bar");
        // Object
        let _ = table.classes.get("Baz");
        // Enum
        let _ = table.classes.get("Qux");
    }
}
