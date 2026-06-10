//! Typed-AST entry point for MIR lowering.
//!
//! Parallel to the legacy [`crate::lower_file`] but takes a
//! [`skotch_ast::KtFile`] (typed view over the SIL tree) instead of
//! the Box-tree `&skotch_syntax::KtFile`.
//!
//! ## Current coverage
//!
//! Initial scaffold returns an empty [`skotch_mir::MirModule`] with
//! the wrapper class name populated. Each consumer migration step
//! expands the coverage one decl/expression form at a time. Same
//! migration pattern as [`skotch_resolve::typed`] and
//! [`skotch_typeck::typed`].

use skotch_ast::{AstNode, KtDecl, KtFile};
use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_mir::{BasicBlock, FuncId, MirFunction, MirModule, Terminator};
use skotch_resolve::{PackageSymbolTable, ResolvedFile};
use skotch_typeck::TypedFile;
use skotch_types::Ty;

/// Lower a single typed file to MIR.
///
/// Counterpart of [`crate::lower_file`]. Initial coverage handles
/// the simplest top-level functions; further decl/expression shapes
/// land in subsequent porting sessions.
///
/// ## Current coverage
///
/// - Top-level fun with empty body → MirFunction with a single
///   BasicBlock terminating in `Return`. Parameters / declared
///   types pulled from the typed AST.
///
/// ## Not yet covered
///
/// Every expression form / statement form / class lowering —
/// these are the legacy `lower_*` and `emit_*` functions in
/// `crate::lib.rs` (27k LOC), to be ported one at a time.
pub fn lower_file(
    file: KtFile<'_>,
    _resolved: &ResolvedFile,
    typed: &TypedFile,
    _interner: &mut Interner,
    _diags: &mut Diagnostics,
    wrapper_class: &str,
    _package_symbols: Option<&PackageSymbolTable>,
) -> MirModule {
    let mut module = MirModule {
        wrapper_class: wrapper_class.to_string(),
        ..MirModule::default()
    };

    // Top-level vals — emit as top_level_consts (if `const val`) or
    // top_level_props (otherwise). The actual <clinit> synthesis and
    // get<Name>() accessor emission is deferred to a follow-up port.
    for decl in file.decls() {
        if let KtDecl::Property(p) = decl {
            let Some(name) = p.name() else { continue };
            let ty = p
                .type_reference()
                .and_then(|tr| {
                    // Walk the typed TypeRef to a Ty. We don't yet have
                    // shared TypeRef->Ty here; use the typeck output
                    // when available, or fall back to Ty::Any.
                    let _ = tr;
                    None
                })
                .or_else(|| {
                    // Pull from TypedFile.top_vals if pass-1 typeck
                    // collected the val.
                    typed.top_vals.iter().find_map(|tv| {
                        // We don't track the val name in TypedTopVal;
                        // best-effort: assume source-order matches.
                        // (TypedTopVal.name_index is the index, not a
                        // symbol.) Pull by position below instead.
                        let _ = tv;
                        None
                    })
                })
                .unwrap_or(skotch_types::Ty::Any);
            let init_const = p.initializer().and_then(lower_const_init_typed);
            let entry = (
                name.to_string(),
                ty,
                init_const.unwrap_or(skotch_mir::MirConst::Null),
            );
            if p.is_const() {
                module.top_level_consts.push(entry);
            } else {
                module.top_level_prop_names.insert(name.to_string());
                module.top_level_props.push(entry);
            }
        }
    }

    // Top-level functions — one MirFunction per KtFun decl.
    let mut fn_id = 0u32;
    for decl in file.decls() {
        if let KtDecl::Fun(f) = decl {
            let name = f.name().unwrap_or("<anon>").to_string();
            // Pull param/return Ty from the TypedFile pass-1 output if
            // the indices line up.
            let typed_fn = typed.functions.iter().find(|tf| tf.name_index == fn_id);
            let return_ty = typed_fn.map(|tf| tf.return_ty.clone()).unwrap_or(Ty::Unit);
            let param_count = f
                .value_parameter_list()
                .map(|pl| pl.parameters().count())
                .unwrap_or(0);
            let params: Vec<skotch_mir::LocalId> = (0..param_count)
                .map(|i| skotch_mir::LocalId(i as u32))
                .collect();
            let param_tys: Vec<Ty> = typed_fn
                .map(|tf| tf.param_tys.clone())
                .unwrap_or_else(|| (0..param_count).map(|_| Ty::Any).collect());
            let param_names: Vec<String> = f
                .value_parameter_list()
                .map(|pl| {
                    pl.parameters()
                        .map(|p| p.name().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            // Single empty basic block terminating in Return.
            // This is the minimum viable body — further statement
            // lowering lands in follow-up porting steps. Even a
            // non-Unit-returning fn body produces a Return terminator
            // here as the placeholder; a future port emits
            // ReturnValue with the lowered last expression.
            let blocks = vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }];

            module.functions.push(MirFunction {
                id: FuncId(fn_id),
                name,
                params,
                locals: param_tys,
                blocks,
                return_ty,
                required_params: param_count,
                param_names,
                param_receiver_types: Vec::new(),
                param_defaults: Vec::new(),
                is_abstract: false,
                vararg_index: None,
                exception_handlers: Vec::new(),
                is_suspend: f.is_suspend(),
                is_inline: f.is_inline(),
                has_type_params: f
                    .type_parameter_list()
                    .map(|tpl| tpl.parameters().next().is_some())
                    .unwrap_or(false),
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: Vec::new(),
                named_locals: Vec::new(),
                is_private: f.visibility() == skotch_syntax::Visibility::Private,
                is_static: false,
                default_call_masks: Vec::new(),
                needs_leading_nop: false,
                local_generic_args: rustc_hash::FxHashMap::default(),
            });
            fn_id += 1;
        }
    }

    module
}

/// Lower a const initializer expression (val/property RHS) to a
/// `MirConst`. Only the simplest literal forms are recognized; more
/// complex initializers run inside <clinit> at runtime. Mirrors the
/// legacy `lower_const_init`.
fn lower_const_init_typed(e: skotch_ast::KtExpr<'_>) -> Option<skotch_mir::MirConst> {
    use skotch_ast::KtExpr;
    use skotch_mir::MirConst;
    match e {
        KtExpr::Boolean(_) => {
            // The boolean composite child is a KW_TRUE / KW_FALSE token.
            let is_true = skotch_ast::children(e.syntax())
                .iter()
                .any(|c| c.kind == skotch_syntax::SyntaxKind::KW_TRUE);
            Some(MirConst::Bool(is_true))
        }
        KtExpr::Integer(_) => {
            // Pull the integer literal text from the child INTEGER_LITERAL.
            let text = skotch_ast::children(e.syntax()).iter().find_map(|c| {
                if c.kind == skotch_syntax::SyntaxKind::INTEGER_LITERAL {
                    if let skotch_sil::SilData::Token { text } = &c.data {
                        return Some(text.as_str());
                    }
                }
                None
            })?;
            let v: i64 = text.parse().ok()?;
            // Mirror legacy: Int by default (cast).
            Some(MirConst::Int(v as i32))
        }
        KtExpr::Float(_) => {
            let text = skotch_ast::children(e.syntax()).iter().find_map(|c| {
                if matches!(
                    c.kind,
                    skotch_syntax::SyntaxKind::FLOAT_LITERAL
                        | skotch_syntax::SyntaxKind::DOUBLE_LITERAL
                ) {
                    if let skotch_sil::SilData::Token { text } = &c.data {
                        return Some(text.as_str());
                    }
                }
                None
            })?;
            let v: f64 = text.trim_end_matches(['f', 'F']).parse().ok()?;
            // Disambiguate Float vs Double from suffix.
            if text.ends_with('f') || text.ends_with('F') {
                Some(MirConst::Float(v as f32))
            } else {
                Some(MirConst::Double(v))
            }
        }
        KtExpr::Null(_) => Some(MirConst::Null),
        KtExpr::Parenthesized(p) => skotch_ast::children(p.syntax())
            .iter()
            .find_map(|c| KtExpr::cast(c).and_then(lower_const_init_typed)),
        // String templates require MirModule access to intern strings,
        // so defer until call sites can pass module in.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower(src: &str, wrapper: &str) -> MirModule {
        let parsed = skotch_ast::parse("test.kt", src);
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = skotch_typeck::typed::type_check(
            parsed.file(),
            &resolved,
            &mut interner,
            &mut diags,
            None,
        );
        lower_file(
            parsed.file(),
            &resolved,
            &typed,
            &mut interner,
            &mut diags,
            wrapper,
            None,
        )
    }

    #[test]
    fn typed_lower_file_scaffold_returns_wrapper() {
        let module = lower("fun main() {}", "TestKt");
        assert_eq!(module.wrapper_class, "TestKt");
    }

    #[test]
    fn typed_lower_fun_main_produces_mir_function() {
        let module = lower("fun main() {}", "TestKt");
        assert_eq!(module.functions.len(), 1);
        let f = &module.functions[0];
        assert_eq!(f.name, "main");
        assert_eq!(f.params.len(), 0);
        assert_eq!(f.return_ty, Ty::Unit);
        assert_eq!(f.blocks.len(), 1);
        assert!(matches!(f.blocks[0].terminator, Terminator::Return));
    }

    #[test]
    fn typed_lower_fun_with_params_records_signature() {
        let module = lower("fun add(a: Int, b: Int): Int = 0", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.required_params, 2);
        assert_eq!(f.return_ty, Ty::Int);
        assert_eq!(f.param_names, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(f.locals, vec![Ty::Int, Ty::Int]);
    }

    #[test]
    fn typed_lower_multi_funs_get_sequential_ids() {
        let module = lower("fun a() {}\nfun b() {}\nfun c() {}", "TestKt");
        assert_eq!(module.functions.len(), 3);
        assert_eq!(module.functions[0].id.0, 0);
        assert_eq!(module.functions[1].id.0, 1);
        assert_eq!(module.functions[2].id.0, 2);
        assert_eq!(module.functions[0].name, "a");
        assert_eq!(module.functions[2].name, "c");
    }

    #[test]
    fn typed_lower_suspend_inline_flags_propagate() {
        let module = lower("suspend inline fun foo() {}", "TestKt");
        let f = &module.functions[0];
        assert!(f.is_suspend);
        assert!(f.is_inline);
    }

    #[test]
    fn typed_lower_private_fun_marked_private() {
        let module = lower("private fun secret() {}", "TestKt");
        let f = &module.functions[0];
        assert!(f.is_private);
    }

    #[test]
    fn typed_lower_const_val_emits_top_level_const() {
        let module = lower("const val MAX: Int = 42", "TestKt");
        assert_eq!(module.top_level_consts.len(), 1);
        let (name, _ty, c) = &module.top_level_consts[0];
        assert_eq!(name, "MAX");
        assert!(matches!(c, skotch_mir::MirConst::Int(42)));
    }

    #[test]
    fn typed_lower_top_val_emits_top_level_prop() {
        let module = lower("val HALF: Double = 0.5", "TestKt");
        assert_eq!(module.top_level_props.len(), 1);
        assert!(module.top_level_prop_names.contains("HALF"));
        let (name, _ty, c) = &module.top_level_props[0];
        assert_eq!(name, "HALF");
        assert!(matches!(c, skotch_mir::MirConst::Double(d) if (*d - 0.5).abs() < 1e-9));
    }

    #[test]
    fn typed_lower_top_val_with_no_literal_init() {
        // Non-literal init: const lowering returns None → MirConst::Null
        // placeholder; the real <clinit> path handles the actual init.
        let module = lower("val X = foo()", "TestKt");
        assert_eq!(module.top_level_props.len(), 1);
        assert!(matches!(
            module.top_level_props[0].2,
            skotch_mir::MirConst::Null
        ));
    }
}
