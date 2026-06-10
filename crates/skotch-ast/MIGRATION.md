# Migration plan: legacy `skotch-syntax/ast.rs` → typed `skotch-ast`

## Status

**Done:**
- [x] SIL grammar parses every fixture input (1086/1086, was 1011/1086).
- [x] `skotch-ast` crate: typed wrapper types over `SilNode`, ~110
      composite kinds + token shims, `KtDecl` / `KtExpr` enum unions.
- [x] `skotch_ast::parse(file, source) -> ParsedFile` entry point.
- [x] `skotch_parser::parse_to_sil(file, source) -> SilTree` bridge.
- [x] **Comprehensive accessor surface** on typed AST: visibility,
      modifiers (data/open/abstract/sealed/inner/suspend/inline/operator/
      infix/tailrec/lateinit/const), annotations (with short-name
      resolution and use-site target), extension receivers, type
      references (KtUserType, KtFunctionType, KtNullableType, with
      qualifier/dotted-name walking and REFERENCE_EXPRESSION-aware
      ident extraction), supertype list entries (with SuperTypeEntry
      union covering plain/call/delegated forms).
- [x] **SIL emits a single `CLASS` composite** for `class`, `interface`
      and `enum class`. `KtClass` / `KtInterface` / `KtEnumClass` casts
      branch on the presence of `KW_INTERFACE` or `KW_ENUM` modifier.
- [x] `skotch_resolve::typed` — **feature-parity Pass 1 (gather)**:
      TypeRef → JVM descriptor / Ty with typealias substitution +
      Kotlin-to-Java collection erasure; top-level fun/val gathering
      with descriptor / param_tys / return_ty / receiver / is_extension
      / has_default; class/interface/object/enum gathering with full
      ExternalClassDecl shape (fields, ctor_params, methods,
      secondary_ctors, companion_methods, has_companion, super_class,
      interfaces, is_open/abstract/inner, enum_entries, annotations,
      has_type_params, has_init_blocks); nested Outer\$Inner JVM names;
      per-class imports overlay; cross-file same-package decl visibility
      and typealias side-table (`AliasTarget` carrying a pinned SilNode
      pointer for re-walking).
- [x] `skotch_resolve::typed::resolve_file` — top-level decl
      registration, stdlib intrinsic threading, ExternalPackage cross-
      file entries, per-function ResolvedFunction with parameter symbols.
- [x] **15 typed-resolve unit tests + 13 legacy-vs-typed parity tests**
      covering top-level fun descriptors, primary ctor fields,
      data-class/enum/interface/object/typealias kinds, extension
      receivers, nullable descriptor erasure, package prefix on JVM
      name, nested Outer\$Inner class naming, cross-file same-package
      class import threading.
- [x] `skotch_typeck::typed::type_check` — **Pass 1 signature collection
      with full Ty conversion**: top-level fun param/return Ty,
      top-level val Ty, typealias-to-Function substitution,
      expression-body literal return inference.
- [x] **8 typed-typeck unit tests + 5 legacy-vs-typed parity tests**
      covering fun int arithmetic, fun string param, top-val string,
      nullable param/return, unit return.
- [x] `skotch_mir_lower::typed::lower_file` (scaffold only).
- [x] `skotch_driver::typed::compile_source` (scaffold, wires the
      typed pipeline parse→resolve→typeck→lower).

**Next concrete migration steps** (per crate, dependency-order):

1. **`skotch_resolve::typed` — body-walk Resolver impl**
   - The current `resolve_file` registers top-level decls but does
     not walk function bodies to track parameter / local / when-arm
     smart-cast scopes.
   - The legacy `Resolver` struct (`crate::Resolver` impl) is the
     recursion target; the typed body walk needs `KtBlock`,
     `KtExpr::Reference`, `KtIsExpression`, `KtWhen` accessors.

2. **`skotch_typeck::typed` — Pass 2 body inference**
   - Top-level fun/val signatures land via Pass 1 (done). Pass 2 walks
     each function body bidirectionally; each `KtExpr` variant maps
     to one inference rule.
   - Class/interface/enum/object member signatures need to land at
     gather time so member-method calls resolve cross-file.
   - `when` exhaustiveness over enum and sealed subjects.
   - `requireNotNull` / `checkNotNull` smart-cast narrowing.

3. **`skotch_mir_lower::typed::lower_file`** — the dominant cost.
   - Legacy `lower_file` is 27.7k LOC across 22 top-level `lower_*` /
     `emit_*` functions (lower_function, lower_stmt, lower_expr,
     lower_val_stmt, lower_template_part, lower_enum, lower_object,
     lower_class, lower_interface).
   - 1107 `Expr::*` / `Stmt::*` / `Decl::*` pattern arms to rewrite
     onto `KtExpr` / `KtBlock` / typed-AST equivalents.
   - Strategy: port one decl kind at a time (top-level fun, then
     top-level val, then if/when/for/while, then classes, then
     coroutines etc.) with golden tests covering each stage.

4. **`skotch_driver::typed::compile_source`** — already wires the
   typed pipeline parse→resolve→typeck→lower. Cuts over to the
   legacy entry point becoming a shim once #1–#3 reach parity.

5. **`skotch-lsp` and `skotch-repl`** — direct AST consumers. Each
   has ~10 pattern-match sites on `Decl::Fun/Class/Val`. Mechanical
   translation to typed wrappers.

6. **Tests + golden refresh.** ~7 backend test modules and ~30 unit
   tests hard-code `skotch_parser::parse_file` + Box AST literals.
   Rewrite to use `skotch_ast::parse`. Then regenerate all ~1313
   fixture goldens (jvm + dex + llvm + klib targets).

7. **Delete legacy.** `crates/skotch-syntax/src/ast.rs` (744 LOC) +
   `crates/skotch-parser/` (5606 LOC). Remove from workspace
   Cargo.toml; `skotch-syntax` keeps only the token / visibility /
   operator enums that `skotch-types` and `skotch-ast` reference.

**Coverage gates** (run after each crate's body fills in):
- `cargo test -p <crate>` for the crate's own tests.
- `cargo test -p skotch-resolve --test typed_parity` for resolve.
- `cargo test -p skotch-typeck --test typed_parity` for typeck.
- `cargo test --package skotch-driver --test fixture_compare` for
  the bytecode goldens.
- `cargo run -p xtask -- gen-fixtures --target {jvm,dex} --skotch-only`
  to refresh the goldens.

## Suggested order

Bottom-up by dependency: smaller, leafier crates first so the API
shape stabilizes before mir-lower picks it up.

1. **`skotch-resolve`** — relatively self-contained. **Pass 1 done.**
2. **`skotch-typeck`** — depends on resolve + the same AST types.
   **Pass 1 done; Pass 2 body inference pending.**
3. **`skotch-hir`** — typically a thin layer over the AST.
4. **`skotch-mir-lower`** — the heavyweight. Lower-level functions
   pattern-match on every expression form. ~27k lines; budget at
   least 2 days of focused work.
5. **Backends** — most call into mir-lower; minimal direct AST
   contact.
6. **`skotch-driver`, `skotch-db`, `skotch-cli`, `skotch-lsp`,
   `skotch-repl`** — orchestration only; mostly call into the above.
7. **Delete `skotch-syntax/ast.rs`** and the legacy `skotch-parser`
   crate.

## Pattern to follow

### Before (legacy)
```rust
use skotch_syntax::{Decl, Expr, FunDecl, KtFile};

fn collect_fn_names(file: &KtFile) -> Vec<String> {
    file.decls
        .iter()
        .filter_map(|d| match d {
            Decl::Fun(FunDecl { name, .. }) => Some(name.to_string()),
            _ => None,
        })
        .collect()
}
```

### After (typed wrappers)
```rust
use skotch_ast::{KtDecl, KtFile};

fn collect_fn_names(file: KtFile<'_>) -> Vec<String> {
    file.decls()
        .filter_map(|d| match d {
            KtDecl::Fun(f) => f.name().map(|s| s.to_string()),
            _ => None,
        })
        .collect()
}
```

The typed wrappers are `Copy` and lifetime-bound to the underlying
`SilTree`; they're zero-cost newtypes. The legacy `Box<Expr>`
recursion becomes traversal through `children()` / typed accessors.

## Per-crate migration recipe

1. **Add the dep:** `skotch-ast = { workspace = true }` to
   the crate's `Cargo.toml`.
2. **Search-and-replace import statements:**
   `skotch_syntax::{Decl, Expr, ...}` → `skotch_ast::{KtDecl,
   KtExpr, ...}`.
3. **Function signatures:**
   - `&KtFile` → `KtFile<'_>` (typed wrapper).
   - `&Expr` → `KtExpr<'_>`.
   - `&Block` → `KtBlock<'_>`.
   - `&[Decl]` → `impl Iterator<Item = KtDecl<'_>>`.
4. **Match arms:** Each `match expr { Expr::Foo { a, b } => ... }`
   becomes `match KtExpr::cast(node) { Some(KtExpr::Foo(f)) =>
   { let a = f.a(); let b = f.b(); ... } _ => ... }`.
5. **Field access:** legacy `decl.name` becomes `decl.name()`
   (Option-returning accessor on typed wrapper).
6. **Spans:** `decl.span` → `decl.span()` (same).
7. **Run the crate's tests:** confirm behavior matches.
8. **Add parity tests** in `tests/typed_parity.rs` that fan both
   pipelines through the same source and assert shape equality.

## Risk and mitigations

- **Risk:** Migrating mir-lower partially breaks every backend test.
  - **Mitigation:** Port one decl kind at a time with parity tests
    against fixtures. Keep both legacy and typed entry points live
    until full parity is verified end-to-end.
- **Risk:** Fixture goldens drift on bytecode level if the migration
  introduces small lowering differences.
  - **Mitigation:** Run `cargo xtask gen-fixtures --target jvm/dex`
    after each major migration step and commit golden refreshes
    alongside the code change.
- **Risk:** Salsa tracking depends on input/output types being
  stable. Switching AST types may invalidate Salsa caches.
  - **Mitigation:** Acceptable; recompile-all on schema change is
    the expected behavior.

## Open questions

- Should the typed wrappers eventually become the `KtFile` in
  `skotch-syntax`, or stay in their own crate? Current decision:
  separate crate, so the legacy AST can sit alongside during
  migration without circular dependencies.
- Should we add a `SilNode → Box<Expr>` adapter for piecewise
  migration? Not implemented; recommend full per-crate cutover
  instead so we don't grow a long-lived bridge layer.

## Session log

### 2026-06-10 (session 4)

- Added 962 LOC of typed AST accessors (visibility, modifiers,
  annotations, type-ref/user-type/function-type/nullable, supertype
  entries, primary/secondary constructor surface, type parameters,
  property accessors, import-directive name parts).
- Fixed `KtClass` / `KtInterface` / `KtEnumClass` cast routing — SIL
  emits a single `CLASS` composite; the typed wrappers branch on
  `KW_INTERFACE` and `KW_ENUM` (modifier list) presence.
- Fixed `KtUserType::name` to dig through `REFERENCE_EXPRESSION` for
  the IDENTIFIER token (SIL stores the ident as a nested ref-expr,
  not as a direct child).
- Fixed `KtNullableType::inner_*` to surface `KtUserType` /
  `KtFunctionType` directly (SIL omits the inner TYPE_REFERENCE
  wrapper after `?`).
- Fixed `KtPackageDirective::name` to walk
  `DOT_QUALIFIED_EXPRESSION → REFERENCE_EXPRESSION → IDENTIFIER` for
  dotted package names.
- Ported `skotch_resolve::typed` to feature-parity Pass 1 over the
  typed AST (~1180 LOC of new code). Added 22 unit tests + 13 parity
  tests against the legacy `gather_declarations`.
- Fixed enum super-class JVM erasure: typed gather now emits
  `Some("java/lang/Enum")` (kotlinc-erased form) to match legacy.
- Expanded `skotch_typeck::typed` to Pass 1 with full Ty conversion
  (was Ty::Any placeholder). 10 unit tests + 5 parity tests.
- Added Pass 2 body walk in `skotch_resolve::typed::resolve_function_body`:
  param/local/`this`/top-level/super references tracked through
  KtBlock children (PROPERTY for local val/var, KtExpr for nested
  expressions). Handles Reference, This, Super, Call, Binary,
  DotQualified, SafeAccess, If, Return, Throw, Block,
  Parenthesized, Prefix/Postfix/Unary, String template.
- Added Pass 2 local-type harvest in `skotch_typeck::typed`:
  per-fn body walk records local val/var types in TypedFunction.local_tys.

**Verification: 449/449 workspace lib tests pass, 0 failures across
40 test suites. Clippy clean on changed crates.**

**Session 4 add-ons:**
- Body walk extended in `skotch_resolve::typed`: for-loop var,
  while/do-while conditions, when-arm subjects, try-catch
  exception vars, lambda params.
- `skotch_typeck::typed` Pass 2 synth_expr ported: literals,
  references against scope, parenthesized passthrough, binary ops
  with Int/Long/Double promotion + String concat + boolean
  comparisons, unary/prefix/postfix passthrough, bare-callee Call
  resolution via the top-level fn_returns map. Each fn body's
  scope is seeded with that fn's own parameters so initializer
  refs resolve correctly.
- `skotch-repl::classify_input` migrated to typed AST: first
  consumer crate that no longer touches the legacy
  `skotch_syntax::Decl` enum (the highlighter still uses
  TokenKind, which is the token-kind enum, not the AST).
- `skotch_mir_lower::typed` initial port: top-level KtFun decls
  emit MirFunction with FuncId / name / params / locals /
  return_ty / single-block Return terminator / suspend/inline/
  private/has_type_params flags / param_names. 6 unit tests
  verify the typed pipeline end-to-end (parse → typeck →
  mir-lower).

**Verification: 456/456 workspace lib tests pass on second run
after the body-walk and synth_expr additions; +7 from baseline.**

**Remaining (multi-session):**
- Full bidirectional inference in typeck Pass 2 (operator
  overloading, smart casts, member lookups, when exhaustiveness,
  cycle detection across top vals)
- Mir-lower port: body statement lowering, expression lowering,
  class/interface/enum/object lowering — the legacy `lower_file`
  is ~27k LOC of dense pattern-matching across Stmt / Expr /
  Decl variants
- LSP migration (~12 pattern-match sites, DocumentState shape
  change)
- Driver cutover: switch `skotch_driver::compile_source` to call
  the typed pipeline exclusively once the typed mir-lower
  reaches feature parity
- Test/golden migration: ~7 backend test modules + ~30 unit
  tests hardcode `skotch_parser::parse_file`; rewrite to use
  `skotch_ast::parse` and regenerate ~1313 fixture goldens
- Delete legacy parser + AST: `crates/skotch-syntax/src/ast.rs`
  (744 LOC) + `crates/skotch-parser/` (5606 LOC)
