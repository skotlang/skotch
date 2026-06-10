# Migration plan: legacy `skotch-syntax/ast.rs` → typed `skotch-ast`

## Status

**Done:**
- [x] SIL grammar parses every fixture input (1086/1086, was 1011/1086).
- [x] `skotch-ast` crate: typed wrapper types over `SilNode`, ~80
      composite kinds + token shims, `KtDecl` / `KtExpr` enum unions.
- [x] `skotch_ast::parse(file, source) -> ParsedFile` entry point.
- [x] `skotch_parser::parse_to_sil(file, source) -> SilTree` bridge.
- [x] **`typed` module scaffolds** in `skotch-resolve`,
      `skotch-typeck`, `skotch-mir-lower`, `skotch-driver` —
      end-to-end typed-AST compilation pipeline (parse → resolve →
      typeck → lower) plumbed and tested. Each scaffold is the
      migration target; consumer migration fills in the body one
      composite kind at a time.

**Next concrete migration steps** (each maps to a specific
`typed` module body):

1. `skotch_resolve::typed::gather_declarations` — currently handles
   top-level `fun` + `val`. Add `KtClass`/`KtInterface`/`KtObject`/
   `KtEnumClass`/`KtTypeAlias` arms. Each arm builds the same
   `ExternalFunDecl`/`ExternalClassDecl`/etc. record as the legacy
   pattern-match arm at `crate::gather_declarations`. Port descriptor
   computation (`build_descriptor_with_aliases`) to walk typed
   `KtValueParameter` / `KtTypeReference` children.

2. `skotch_resolve::typed::resolve_file` — currently registers
   top-level fun/val. Add body-level resolution: parameter scopes,
   local val/var scopes, when-arm `is`/`as` smart-cast scopes.
   Match the recursion structure in the legacy `Resolver` impl.

3. `skotch_typeck::typed::type_check` — currently returns empty.
   Fill in the two-pass bidirectional checker: pass 1 collects
   signatures, pass 2 walks each function body. Each `KtExpr`
   variant maps to one inference rule from the legacy
   `infer_expr` switch.

4. `skotch_mir_lower::typed::lower_file` — currently returns
   wrapper-only `MirModule`. The legacy `lower_file` is 27k lines;
   port one decl kind at a time, with golden tests covering each
   stage. Start with: top-level `fun`, top-level `val`, basic
   `if`/`when`/`for`/`while`, string templates, then classes,
   inheritance, generics, coroutines, etc.

5. `skotch_driver::typed::compile_source` — already wires the
   typed pipeline; once the underlying scaffolds gain coverage, the
   legacy `crate::compile_source` becomes a thin shim around it.

**Coverage gates** (run after each crate's body fills in):
- `cargo test -p <crate>` for the crate's own tests.
- `cargo test --package skotch-driver --test fixture_compare` for
  the bytecode goldens.
- `cargo run -p xtask -- gen-fixtures --target {jvm,dex} --skotch-only`
  to refresh the goldens.

**Not yet done — these are the per-crate migration steps:**

| Crate              | LOC     | Status            | Notes |
|--------------------|---------|-------------------|-------|
| `skotch-parser`    |  5,606  | retain for now    | Replace with SIL-driven wrapper once consumers are migrated. |
| `skotch-resolve`   |  2,057  | uses legacy AST   | ~12 AST types used; needs `Decl`, `Expr`, `Stmt`, `Block`, `Type*` rewritten on typed wrappers. |
| `skotch-typeck`    |  2,410  | uses legacy AST   | Similar surface to resolve. |
| `skotch-hir`       |  *small*  | uses legacy AST   | |
| `skotch-mir-lower` | 27,665  | uses legacy AST   | The big one — lowering matches every `Expr`/`Stmt` variant. |
| `skotch-backend-*` |  ~5,000 | uses legacy AST   | 5 backends, mostly call into mir-lower. |
| `skotch-driver`    |    ~500 | uses legacy AST   | Compile-source orchestration. |
| `skotch-db`        |     81  | uses legacy AST indirectly | Salsa-tracked compile pipeline. |
| `skotch-lsp`       |  ~few k | uses legacy AST   | LSP server reads parsed files. |
| `skotch-cli`       |   ~few k | uses legacy AST   | CLI orchestration. |
| `skotch-repl`      |   ~few k | uses legacy AST   | REPL session. |

**Estimated remaining effort:** ~30-60 hours of focused engineering for
the full migration. The per-crate work is mostly mechanical (replace
`match expr { Expr::Foo(a, b, c) => ... }` with `if let Some(call) =
KtCallExpression::cast(node) { let args = call.value_argument_list()
... }`), but every consumer needs to be rewritten and every test re-run.

## Suggested order

Bottom-up by dependency: smaller, leafier crates first so the API
shape stabilizes before mir-lower picks it up.

1. **`skotch-resolve`** — relatively self-contained, uses `KtFile`,
   `Decl`, `FunDecl`, `ClassDecl`, `ValDecl`, `Block`, `Expr`, `Stmt`,
   `Param`, `ConstructorParam`, `Annotation`, `TypeRef`, `Visibility`.
2. **`skotch-typeck`** — depends on resolve + the same AST types.
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

## Risk and mitigations

- **Risk:** Migrating mir-lower partially breaks every backend test.
  - **Mitigation:** Migrate mir-lower in one go with all tests
    disabled until complete, OR introduce a thin compatibility shim
    that converts `KtExpr` to the legacy `Expr` enum for backwards
    compat during migration.
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
