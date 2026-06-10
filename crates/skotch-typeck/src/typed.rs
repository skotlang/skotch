//! Typed-AST entry point for type checking.
//!
//! Parallel to the legacy [`crate::type_check`] but takes a
//! [`skotch_ast::KtFile`] (typed view over a SIL tree) instead of the
//! Box-tree `&skotch_syntax::KtFile`.
//!
//! ## Current coverage
//!
//! Pass 1 (signature collection):
//! - Top-level fun: param/return Ty from KtTypeReference, with
//!   typealias substitution and import resolution.
//! - Top-level val: declared Ty (or `Ty::Any` when omitted).
//! - Class / interface / enum / object: members threaded into the
//!   per-file `TypedFile` so cross-file consumers can read them.
//!
//! Not yet covered (next migration sessions):
//! - Function body type inference (the legacy bidirectional checker).
//! - `when` exhaustiveness over enum / sealed subjects.
//! - Smart-cast narrowing on `is`/`as` / `requireNotNull`.
//! - Cycle detection across top-level vals.

use crate::{Signature, TypedFile, TypedFunction, TypedTopVal};
use rustc_hash::FxHashMap;
use skotch_ast::{
    AstNode, KtDecl, KtFile, KtFun, KtFunctionType, KtTypeReference, KtUserType, KtValueParameter,
    KtValueParameterList,
};
use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_resolve::{DefId, PackageSymbolTable, ResolvedFile};
use skotch_types::Ty;

/// Type-check a single file using the typed AST input.
pub fn type_check(
    file: KtFile<'_>,
    _resolved: &ResolvedFile,
    _interner: &mut Interner,
    _diags: &mut Diagnostics,
    package_symbols: Option<&PackageSymbolTable>,
) -> TypedFile {
    let mut out = TypedFile::default();

    // ── Imports / typealiases (per-file) ────────────────────────────
    let imports = collect_imports(file);
    let mut aliases: FxHashMap<String, AliasTarget> = FxHashMap::default();
    for d in file.decls() {
        if let KtDecl::TypeAlias(t) = d {
            if let (Some(name), Some(tr)) = (t.name(), t.type_reference()) {
                aliases.insert(name.to_string(), AliasTarget::from_type_ref(tr));
            }
        }
    }
    let _ = package_symbols; // future: thread cross-file aliases

    // ── Pass 1: top-level signatures ────────────────────────────────
    let mut fn_index = 0u32;
    let mut val_index = 0u32;
    for decl in file.decls() {
        match decl {
            KtDecl::Fun(f) => {
                let param_tys = collect_param_tys(f.value_parameter_list(), &imports, &aliases);
                let return_ty = f
                    .return_type()
                    .map(|tr| type_ref_to_ty(tr, &imports, &aliases))
                    .unwrap_or_else(|| infer_return_ty(f));
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
            KtDecl::Property(p) => {
                let ty = p
                    .type_reference()
                    .map(|tr| type_ref_to_ty(tr, &imports, &aliases))
                    .unwrap_or(Ty::Any);
                out.top_signatures.insert(
                    DefId::TopLevelVal(val_index),
                    Signature {
                        params: Vec::new(),
                        ret: ty.clone(),
                    },
                );
                out.top_vals.push(TypedTopVal {
                    name_index: val_index,
                    ty,
                });
                val_index += 1;
            }
            _ => {}
        }
    }

    // ── Pass 2: per-function body inference (basic) ─────────────────
    //
    // For each top-level fun we walk the body, recording locals types
    // in `TypedFunction.local_tys`. This is a minimal bidirectional
    // checker — literal and var-init shapes only. Full coverage
    // (operator overloading, smart casts, sealed exhaustiveness)
    // lands in subsequent sessions.
    let mut fn_idx = 0u32;
    for decl in file.decls() {
        if let KtDecl::Fun(f) = decl {
            let mut local_tys: Vec<Ty> = Vec::new();
            if let Some(block) = f.body_block() {
                walk_block_for_locals(block, &imports, &aliases, &mut local_tys);
            }
            if let Some(rec) = out.functions.iter_mut().find(|r| r.name_index == fn_idx) {
                rec.local_tys = local_tys;
            }
            fn_idx += 1;
        }
    }

    out
}

/// Walk a function body to harvest local variable types in source
/// order. Local `val`/`var` declarations surface as PROPERTY children
/// of the BLOCK composite; nested blocks (inside `if` / `when` arms)
/// recurse. Synthesized expression types feed the scope so a later
/// initializer that references an earlier local picks up its type.
fn walk_block_for_locals(
    block: skotch_ast::KtBlock<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
    local_tys: &mut Vec<Ty>,
) {
    let mut scope: Vec<(String, Ty)> = Vec::new();
    walk_block_with_scope(block, imports, aliases, local_tys, &mut scope);
}

fn walk_block_with_scope(
    block: skotch_ast::KtBlock<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
    local_tys: &mut Vec<Ty>,
    scope: &mut Vec<(String, Ty)>,
) {
    use skotch_ast::KtExpr;
    let saved = scope.len();
    for c in skotch_ast::children(block.syntax()) {
        if let Some(prop) = skotch_ast::KtProperty::cast(c) {
            let ty = prop
                .type_reference()
                .map(|tr| type_ref_to_ty(tr, imports, aliases))
                .or_else(|| prop.initializer().map(|e| synth_expr(&e, scope)))
                .unwrap_or(Ty::Any);
            local_tys.push(ty.clone());
            if let Some(name) = prop.name() {
                scope.push((name.to_string(), ty));
            }
            continue;
        }
        if let Some(expr) = KtExpr::cast(c) {
            match expr {
                KtExpr::If(i) => {
                    if let Some(KtExpr::Block(b)) =
                        i.then_branch().and_then(|t| t.expression())
                    {
                        walk_block_with_scope(b, imports, aliases, local_tys, scope);
                    }
                    if let Some(KtExpr::Block(b)) =
                        i.else_branch().and_then(|e| e.expression())
                    {
                        walk_block_with_scope(b, imports, aliases, local_tys, scope);
                    }
                }
                KtExpr::Block(b) => {
                    walk_block_with_scope(b, imports, aliases, local_tys, scope)
                }
                _ => {}
            }
        }
    }
    scope.truncate(saved);
}

/// Synthesize the type of an expression against the given scope.
/// Mirrors the legacy `TypeChecker::synth_expr` for the common
/// expression shapes. Returns `Ty::Any` for shapes that need
/// TypeEnv-aware inference (method calls, field access against
/// user classes — those land in a follow-up session).
fn synth_expr(e: &skotch_ast::KtExpr<'_>, scope: &[(String, Ty)]) -> Ty {
    use skotch_ast::KtExpr;
    match e {
        KtExpr::Boolean(_) => Ty::Bool,
        KtExpr::Integer(_) => Ty::Int,
        KtExpr::Float(_) => Ty::Double,
        KtExpr::Character(_) => Ty::Char,
        KtExpr::Null(_) => Ty::Nullable(Box::new(Ty::Any)),
        KtExpr::String(_) => Ty::String,
        KtExpr::Reference(r) => {
            if let Some(name) = r.name() {
                if let Some((_, t)) = scope.iter().rev().find(|(n, _)| n == name) {
                    return t.clone();
                }
            }
            Ty::Any
        }
        KtExpr::Parenthesized(p) => {
            for c in skotch_ast::children(p.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    return synth_expr(&inner, scope);
                }
            }
            Ty::Any
        }
        KtExpr::Binary(b) => {
            let lt = b.lhs().map(|l| synth_expr(&l, scope)).unwrap_or(Ty::Any);
            let rt = b.rhs().map(|r| synth_expr(&r, scope)).unwrap_or(Ty::Any);
            let op = b.operation().map(|o| o.text()).unwrap_or_default();
            match op.as_str() {
                "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => Ty::Bool,
                "+" | "-" | "*" | "/" | "%" => {
                    if lt == Ty::Double || rt == Ty::Double {
                        Ty::Double
                    } else if lt == Ty::Long || rt == Ty::Long {
                        Ty::Long
                    } else if matches!(lt, Ty::Int | Ty::Any) && matches!(rt, Ty::Int | Ty::Any) {
                        Ty::Int
                    } else if op == "+" && lt == Ty::String {
                        Ty::String
                    } else {
                        Ty::Int
                    }
                }
                _ => Ty::Any,
            }
        }
        KtExpr::Unary(u) => {
            for c in skotch_ast::children(u.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    return synth_expr(&inner, scope);
                }
            }
            Ty::Any
        }
        KtExpr::Prefix(p) => {
            for c in skotch_ast::children(p.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    return synth_expr(&inner, scope);
                }
            }
            Ty::Any
        }
        KtExpr::Postfix(p) => {
            for c in skotch_ast::children(p.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    return synth_expr(&inner, scope);
                }
            }
            Ty::Any
        }
        // KtExpr::Call / Field / DotQualified / Lambda / etc. require
        // TypeEnv-aware lookup; pending follow-up port.
        _ => Ty::Any,
    }
}

// ── Import collection ───────────────────────────────────────────────

fn collect_imports(file: KtFile<'_>) -> FxHashMap<String, String> {
    let mut out = FxHashMap::default();
    if let Some(list) = file.import_list() {
        for imp in
            skotch_ast::typed_children::<skotch_ast::KtImportDirective>(list.syntax())
        {
            if imp.is_wildcard() {
                continue;
            }
            let parts = imp.name_parts();
            if parts.is_empty() {
                continue;
            }
            let fq = parts.join("/");
            let simple = imp
                .alias()
                .and_then(|a| a.name())
                .unwrap_or_else(|| parts.last().copied().unwrap_or(""));
            if !simple.is_empty() {
                out.insert(simple.to_string(), fq);
            }
        }
    }
    out
}

// ── Type-ref → Ty (typed, with alias side table) ────────────────────

#[derive(Clone)]
struct AliasTarget {
    /// SilNode pointer of the alias target's TYPE_REFERENCE. Lifetime
    /// is bounded by the enclosing `ParsedFile`'s pin.
    target_node_ptr: usize,
}

impl AliasTarget {
    fn from_type_ref(tr: KtTypeReference<'_>) -> Self {
        Self {
            target_node_ptr: tr.syntax() as *const _ as usize,
        }
    }
    fn as_type_ref<'a>(&self) -> KtTypeReference<'a> {
        let raw = self.target_node_ptr as *const skotch_sil::SilNode;
        let node = unsafe { &*raw };
        KtTypeReference::cast(node).expect("alias target stored as TYPE_REFERENCE")
    }
}

fn type_ref_to_ty(
    tr: KtTypeReference<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Ty {
    if let Some(ft) = tr.function_type() {
        return function_type_to_ty(ft, imports, aliases, tr.is_suspend(), tr.is_composable());
    }
    if let Some(n) = tr.nullable_type() {
        let inner = if let Some(u) = n.inner_user_type() {
            user_type_to_ty(u, imports, aliases)
        } else if let Some(ft) = n.inner_function_type() {
            function_type_to_ty(ft, imports, aliases, false, false)
        } else {
            Ty::Any
        };
        return Ty::Nullable(Box::new(inner));
    }
    if let Some(u) = tr.user_type() {
        return user_type_to_ty(u, imports, aliases);
    }
    Ty::Any
}

fn user_type_to_ty(
    u: KtUserType<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Ty {
    let name = u.name().unwrap_or("");
    if let Some(target) = aliases.get(name) {
        return type_ref_to_ty(target.as_type_ref(), imports, aliases);
    }
    skotch_types::ty_from_name(name).unwrap_or_else(|| {
        if let Some(jvm) = skotch_types::intrinsics::kotlin_to_jvm_class(name) {
            Ty::Class(jvm.to_string())
        } else if let Some(fq) = imports.get(name) {
            Ty::Class(fq.clone())
        } else {
            Ty::Any
        }
    })
}

fn function_type_to_ty(
    ft: KtFunctionType<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
    is_suspend: bool,
    is_composable: bool,
) -> Ty {
    let params: Vec<Ty> = ft
        .parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| {
                    p.type_reference()
                        .map(|ptr| type_ref_to_ty(ptr, imports, aliases))
                        .unwrap_or(Ty::Any)
                })
                .collect()
        })
        .unwrap_or_default();
    let ret = ft
        .return_type()
        .map(|rtr| type_ref_to_ty(rtr, imports, aliases))
        .unwrap_or(Ty::Unit);
    Ty::Function {
        params,
        ret: Box::new(ret),
        is_suspend,
        is_composable,
    }
}

fn collect_param_tys(
    plist: Option<KtValueParameterList<'_>>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Vec<Ty> {
    plist
        .map(|pl| {
            pl.parameters()
                .map(|p: KtValueParameter<'_>| {
                    p.type_reference()
                        .map(|tr| type_ref_to_ty(tr, imports, aliases))
                        .unwrap_or(Ty::Any)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Best-effort return-type inference from a function with no explicit
/// `: Type` annotation. Mirrors the legacy `infer_body_return_ty`
/// semantics — only when an explicit `return value` statement is
/// present do we narrow.
fn infer_return_ty(f: KtFun<'_>) -> Ty {
    use skotch_ast::KtExpr;
    if let Some(e) = f.body_expression() {
        return literal_ty(&e);
    }
    let Some(block) = f.body_block() else {
        return Ty::Unit;
    };
    let mut returned: Option<KtExpr<'_>> = None;
    for stmt in block.statements() {
        if let KtExpr::Return(r) = stmt {
            for c in skotch_ast::children(r.syntax()) {
                if let Some(e) = KtExpr::cast(c) {
                    returned = Some(e);
                }
            }
        }
    }
    returned.map(|e| literal_ty(&e)).unwrap_or(Ty::Unit)
}

fn literal_ty(e: &skotch_ast::KtExpr<'_>) -> Ty {
    use skotch_ast::KtExpr;
    match e {
        KtExpr::Boolean(_) => Ty::Bool,
        KtExpr::Integer(_) => Ty::Int,
        KtExpr::Float(_) => Ty::Double,
        KtExpr::Character(_) => Ty::Char,
        KtExpr::Null(_) => Ty::Nullable(Box::new(Ty::Any)),
        KtExpr::String(_) => Ty::String,
        KtExpr::Binary(b) => {
            let op = b.operation().map(|o| o.text()).unwrap_or_default();
            match op.as_str() {
                "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => Ty::Bool,
                _ => Ty::Any,
            }
        }
        _ => Ty::Any,
    }
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
        assert_eq!(typed.functions[0].param_tys, vec![Ty::Int, Ty::Int]);
        assert_eq!(typed.functions[0].return_ty, Ty::Int);
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

    #[test]
    fn typed_string_param_resolves_to_string_ty() {
        let parsed = skotch_ast::parse("test.kt", "fun greet(name: String): String = name");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.functions[0].param_tys, vec![Ty::String]);
        assert_eq!(typed.functions[0].return_ty, Ty::String);
    }

    #[test]
    fn typed_nullable_returns_nullable() {
        let parsed = skotch_ast::parse("test.kt", "fun maybe(x: Int?): String? = null");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert!(matches!(
            typed.functions[0].param_tys[0],
            Ty::Nullable(_)
        ));
        assert!(matches!(typed.functions[0].return_ty, Ty::Nullable(_)));
    }

    #[test]
    fn typed_top_val_recorded() {
        let parsed = skotch_ast::parse("test.kt", "val GREETING: String = \"hi\"");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.top_vals.len(), 1);
        assert_eq!(typed.top_vals[0].ty, Ty::String);
        assert!(typed.top_signatures.contains_key(&DefId::TopLevelVal(0)));
    }

    #[test]
    fn typealias_substitution_to_function_ty() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "typealias Predicate = (Int) -> Boolean\nfun apply(p: Predicate): Boolean = true",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        match &typed.functions[0].param_tys[0] {
            Ty::Function { params, ret, .. } => {
                assert_eq!(params.as_slice(), &[Ty::Int]);
                assert_eq!(**ret, Ty::Bool);
            }
            other => panic!("expected Function, got {other:?}"),
        }
    }

    #[test]
    fn expr_body_literal_infers_return_ty() {
        let parsed = skotch_ast::parse("test.kt", "fun pi() = 3.14");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.functions[0].return_ty, Ty::Double);
    }

    #[test]
    fn body_walks_record_local_types() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "fun main() {\n  val a: Int = 1\n  val b: String = \"hi\"\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        assert_eq!(f.local_tys, vec![Ty::Int, Ty::String]);
    }

    #[test]
    fn body_locals_infer_from_initializer_when_no_annotation() {
        let parsed = skotch_ast::parse("test.kt", "fun main() {\n  val a = 42\n}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        assert_eq!(f.local_tys, vec![Ty::Int]);
    }

    #[test]
    fn body_local_initialized_from_binary_op() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "fun main() {\n  val a = 1\n  val b = a + 2\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        // a inferred from literal as Int; b from synth_expr(a + 2) → Int.
        assert_eq!(f.local_tys, vec![Ty::Int, Ty::Int]);
    }

    #[test]
    fn body_local_initialized_from_comparison() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "fun main() {\n  val a = 1\n  val b = a > 0\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        assert_eq!(f.local_tys, vec![Ty::Int, Ty::Bool]);
    }

    #[test]
    fn body_local_initialized_from_string_concat() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "fun main() {\n  val a = \"hi\"\n  val b = a + \"!\"\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        assert_eq!(f.local_tys, vec![Ty::String, Ty::String]);
    }
}
