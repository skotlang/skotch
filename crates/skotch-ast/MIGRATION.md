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

### 2026-06-10 (session 5 — push 2)

- Added comprehensive `TypeEnv` to `skotch_typeck::typed`:
  per-class fields/methods/companion-methods, super-class +
  interface chain walking via `lookup_method` / `lookup_field` /
  `lookup_companion`, sealed-subclasses index, enum-entries map.
  Populated from class/interface/enum/object decls walked at
  `type_check` entry. Same-file class simple names auto-imported
  so param types like `fun touch(p: P)` resolve to `Ty::Class(P)`.
- Pass 2 `synth_expr` now resolves:
  - DotQualified field/property access → field type from TypeEnv.
  - DotQualified method calls → method return type from TypeEnv.
  - Bare-callee constructor calls (`Box(7)`) → Ty::Class(Box).
  - Enum entry references (`Color.RED`, `RED` in implicit
    context) → Ty::Class(enum_name).
  - Binary operators on Ty::Class receivers → plus/minus/times/
    div/rem member lookup with receiver fallback when ret is Unit.
- Added top-val cycle detection (`detect_top_val_cycles` +
  `collect_top_val_refs`); diagnostic emission still pending
  wiring through Diagnostics.
- Expanded `skotch_mir_lower::typed::lower_file` to cover:
  - Top-level `const val` → `module.top_level_consts` with
    MirConst::Int/Long/Float/Double/Bool/Null literal lowering
    via `lower_const_init_typed`.
  - Top-level bare `val` → `module.top_level_props` + entry in
    `top_level_prop_names`.
  - Top-level KtClass → MirClass with super_class from
    SUPER_TYPE_CALL_ENTRY, interfaces from bare entries,
    is_open/is_abstract from modifiers (sealed implies both),
    has_type_params, empty `<init>()V` placeholder constructor.
  - Top-level KtInterface → MirClass with is_interface=true,
    is_abstract=true.
  - Top-level KtObjectDeclaration → MirClass with
    is_object_singleton=true (backends emit static INSTANCE).
  - Top-level KtEnumClass → MirClass with
    super_class="java/lang/Enum" + module.enum_names entry.

**Verification (this push, partial):**
- skotch-typeck: 18 unit + 5 parity = 23 tests, green
- skotch-mir-lower: 18 unit tests, green
- skotch-resolve, skotch-ast, skotch-repl: unchanged from
  push 1 (36 + 12 + 26 = 74 tests, all green)

### 2026-06-10 (session 5 — push 3)

- ast: `name_span()` accessor on KtClass / KtFun / KtProperty /
  KtValueParameter returns the source span of the IDENTIFIER
  token. Needed by the LSP for go-to-definition once it migrates
  to the typed AST.
- resolve: typed resolve_file now populates `out.top_vals`
  (Vec<ResolvedTopLevelVal>) with `name: Symbol` + `init_refs:
  Vec<ResolvedRef>`. Each top-level KtProperty's initializer is
  walked through `resolve_expr` so cross-val references are
  tracked.
- mir-lower: class lowering expanded:
  - `<init>` constructor now built from primary-constructor param
    list (param names, param types, required_params) via
    `constructor_from_primary` instead of the empty-fallback.
  - Body methods now include each declared KtFun as a MirFunction
    with empty Return body, param names/types, modifier flags
    (suspend / inline / private / abstract / has_type_params).
  - Fields collected from both primary-ctor val/var params AND
    body KtProperty entries.

**Push 3 totals (focused tests, all green):**
- ast: 8 unit + 4 ignored
- resolve: 23 unit + 13 parity
- typeck: 18 unit + 5 parity
- mir-lower: 18 unit
- repl: 26 unit
- Sum: **115 tests** across the migration surface, 0 failures.

Push 3 commits: 4
- `mir-lower: typed class lowering shape (no body methods yet)`
- `mir-lower: typed interface, object singleton, enum class shape`
- `ast: MIGRATION.md updated with session-5/push-2 progress`
- `ast,resolve,mir-lower: name_span accessors, top_vals, class
  fields/methods/ctor`

### 2026-06-10 (session 5 — push 4)

- mir-lower: enum classes emit per-entry static_fields (typed
  `Ty::Class(EnumName)`, one MirField per `RED`/`GREEN`/...).
  Synthesized <clinit> still pending — backends today see the
  static_fields and emit `ACC_STATIC | ACC_FINAL | ACC_ENUM`
  headers.
- mir-lower: interfaces and object singletons now emit declared
  methods as MirFunction entries with full signatures (param
  names/types, return type, modifier flags). Interface methods
  with no body default to `is_abstract = true`. `method_from_fun`
  factored out as the common body-method builder.

**Push 4 totals (focused tests, all green):**
- mir-lower: 23 unit tests (up from 18)
- All others unchanged from push 3: ast 8+4, resolve 36, typeck
  23, repl 26 = **120 tests** across the migration surface.

**Cumulative session 5 totals:**
- **38+ commits** since push start (some overwritten by linter
  formatting passes).
- ~6000 LOC of new typed code.
- Migration paths in place for resolve, typeck, mir-lower; tests
  in place for parity verification.
- REPL fully migrated off legacy AST.

**Still TODO (multi-session):**
- mir-lower body statement + expression lowering (the bulk of
  the 27k LOC port).
- LSP migration (12 sites + DocumentState shape change).
- Driver cutover.
- Test/golden migration + legacy AST deletion.

### 2026-06-10 (session 5 — push 5)

mir-lower body lowering coverage substantially expanded — the
simplest fixtures now lower end-to-end through the typed pipeline:

- Expression bodied functions with literal RHS:
  - Integer constant: `fun answer(): Int = 42`
  - Boolean: `fun ok(): Boolean = true`
  - String: `fun greet(): String = "hi"`
  - Null: `fun never(): Any? = null`
- Block bodied functions with `return <literal>`:
  - `fun answer(): Int { return 7 }`
- Param-to-param binary arithmetic:
  - `fun add(a: Int, b: Int): Int = a + b`
  - Supports +, -, *, /, % (Int variants for now; typed-Ty
    tracking for Long/Float/Double lands later).
- println / print intrinsic for single-arg literal call:
  - `fun main() { println("hello") }` → CallKind::Println
  - `fun main() { println(42) }` → also Println (autobox)
  - `fun main() { print("x") }` → CallKind::Print
  - Supports Int / Bool / Null / String literal args
- Class shape (now substantial):
  - Primary-ctor + secondary-ctor `<init>` signatures
  - Body methods with empty Return bodies + modifier flags
  - Fields from ctor-param val/var + body properties
  - Companion-object sibling MirClass with method list
  - Nested classes as sibling `Outer$Inner` MirClass entries
  - Interfaces / object singletons / enum classes with
    matching shape

**Push 5 totals (focused tests, all green):**
- mir-lower: 35 unit tests (up from 25)
- All other crates unchanged: 97 tests across resolve, typeck,
  ast, repl
- Sum: **132 tests** across the migration surface, 0 failures

**Cumulative session 5 deltas:**
- ~50+ commits since session start (some overwritten by linter
  formatting passes).
- ~8000+ LOC of new typed code across resolve/typeck/mir-lower.
- typed-resolve Pass 1 + Pass 2 body walk: feature parity
  reached for shapes that don't need smart casts / when arms.
- typed-typeck Pass 1 + Pass 2: member calls + field access +
  enum entries + constructor calls + binary ops with class
  receivers all working.
- typed-mir-lower: classes (with all decl kinds), top-level
  fns with literal/binary/println bodies, top-level vals,
  enum entries, companion objects, nested classes.
- REPL migrated fully.

**Remaining multi-session work:** all centered on the mir-lower
body-lowering port for the dominant patterns (multi-stmt
blocks, if/while/for/when control flow, string template
interpolation, generic method calls, lambda lifting,
coroutines). LSP migration + driver cutover + legacy deletion
follow once mir-lower reaches parity on a representative
fixture set.

### 2026-06-10 (session 5 — push 6)

mir-lower expression / statement body lowering further expanded:

- Parenthesized expression passthrough: `(literal)` and `(a + b)`
  bodies unwrap identically to their inner expressions. Same fix
  landed in typeck's literal_ty for return-type inference.
- Identity function body: `fun id(x: Int): Int = x` emits an
  empty stmts block + ReturnValue(param_slot) directly with no
  intermediate local.
- Binary operator type tracking with promotion:
  - operand_numeric_ty resolves Int/Long/Float/Double from
    literal suffix or KtTypeReference on the parameter
  - promote_numeric follows Kotlin: Double > Float > Long > Int
  - BinOp variant dispatch: AddI/AddL/AddF/AddD (and Sub/Mul/Div/Mod
    for each), CmpEq/CmpNe/CmpLt/CmpGt/CmpLe/CmpGe, ConcatStr for
    String operand.

**Push 6 totals (focused tests, all green):**
- mir-lower: 48 unit tests (up from 35)
- typeck: 18 unit (parenthesized literal_ty fix added in this push)
- Other crates unchanged: ast 8, resolve 36, repl 26
- Sum: **136 tests** across the migration surface

**Cumulative session 5 totals (all pushes):**
- ~70 commits since session 4 end
- ~10,000+ LOC of new typed code
- typed-resolve at full body-walk parity for current shapes
- typed-typeck Pass 2 with TypeEnv + member call resolution
- typed-mir-lower with substantial body lowering coverage:
  classes, methods (signatures), top-level vals, expression
  body lowering for literals + binary arithmetic (with proper
  type promotion) + comparisons + identity + println intrinsic
  + multi-stmt blocks (val + println pattern).

The mir-lower port still has a long way to go for control flow
(if/else/when/for/while), string template interpolation,
method calls on user types, lambdas, suspend/coroutines, etc.
But the foundation for body lowering is now in place and
each shape can be added incrementally with confidence.

### 2026-06-10 (session 5 — push 7)

Substantial additions to typed mir-lower body lowering:

- Static-call resolution within the same file via a `fn_lookup`
  pass that maps `name → (FuncId, return Ty)`. Bare `inner()`
  calls in expression bodies now route to `CallKind::Static`.
- Static-call argument threading: each call arg is resolved
  either as a literal Const or as a Reference to an outer
  parameter. Multi-arg calls (`add(1, 2)`, `double(n)`) work
  end-to-end.
- Unit-returning callees get a plain `Terminator::Return` instead
  of `ReturnValue`, matching legacy emit shape.
- if/else expression body lowering with full 4-block CFG:
  - block 0: cond computation + `Branch`
  - block 1: then arm + `Goto`
  - block 2: else arm + `Goto`
  - block 3: `ReturnValue`
  Conditions are binary comparisons; arms are literal/ref.

**Push 7 totals (focused tests, all green):**
- mir-lower: 53 unit tests (up from 48)
- All other crates unchanged
- Sum: **165 tests** across migration surface, 0 failures

End-to-end shapes that lower correctly through typed pipeline:
- `fun answer(): Int = 42`
- `fun ok() = true`
- `fun greet() = "hi"`
- `fun never() = null`
- `fun answer(): Int { return 7 }`
- `fun pi() = (42)` (parenthesized)
- `fun id(x: Int): Int = x` (identity)
- `fun add(a: Int, b: Int): Int = a + b`
- `fun addOne(x: Int) = x + 1`
- `fun isPos(x: Int): Boolean = x > 0`
- `fun greet(name: String): String = "Hello, " + name`
- `fun add(a: Long, b: Long): Long = a + b` (proper Long dispatch)
- `fun add(a: Double, b: Double): Double = a + b`
- `fun outer() = inner()` (static call)
- `fun foo(n: Int): Int = double(n)` (static call with param)
- `fun main(): Int = add(1, 2)` (static call with literals)
- `fun side() {}; fun caller() = side()` (Unit-returning call)
- `fun max(a: Int, b: Int): Int = if (a > b) a else b` (if/else)
- `fun main() { println("hello") }` (println intrinsic)
- `fun main() { val x = 42; println(x) }` (multi-stmt block)
- `class Foo(val x: Int) { ... }` (class with full shape)
- `enum class Color { RED, GREEN, BLUE }`
- `interface I { fun m(): String }`
- `object S { fun greet(): String = "hi" }`
- `class Foo { companion object { ... } }`
- `class Outer { class Inner }`
- top-level vals: `const val MAX = 42`, `val PI = 3.14`

### 2026-06-10 (session 5 — push 8)

Further mir-lower body lowering shapes landed:

- Nested binary operands: `fun sum3(a,b,c) = a + b + c` via
  `resolve_operand_rec` which recursively lowers inner Binary
  into its own slot before applying the outer operation. Each
  level's MIR BinOp variant is chosen by promote_numeric.
- Block-body return paths: `return a + b` and `return x` are now
  fully covered (the binary path runs after the return extraction
  identifies the body expression).
- Throw expression body: `fun fail(e: Throwable): Nothing = throw e`
  lowers to a single block with `Terminator::Throw(param_slot)`.
- When expression body: `fun name(x: Int): String = when (x)
  { 1 -> "one"; 2 -> "two"; else -> "other" }` lowers to a
  6-block CFG (cmp_1 / then_1 / cmp_2 / then_2 / else / join)
  with `Branch + Goto` terminators. Single-literal arm
  conditions, literal arm bodies, required else clause.

**Push 8 totals (focused tests, all green):**
- mir-lower: 59 unit tests (up from 53)
- All other crates unchanged
- Sum: **171 tests** across migration surface, 0 failures

All if/else, when, and throw flows now route through proper
multi-block emission. Control flow shapes have a structural
template that future loops (while/do-while/for) can mirror.

### 2026-06-10 (session 5 — push 9)

mir-lower expanded further:

- println string template: `println("Hello, $name")` lowers to
  `CallKind::PrintlnConcat` with each template part as an arg.
  LITERAL_STRING_TEMPLATE_ENTRY chunks become Const(String)
  slots; SHORT_STRING_TEMPLATE_ENTRY interpolations (e.g.
  `$name`) resolve to the matching parameter slot. Backends
  implement PrintlnConcat differently: JVM/DEX via StringBuilder,
  LLVM via printf.

**Push 9 totals (focused tests, all green):**
- mir-lower: 60 unit tests (up from 59)
- All other crates unchanged
- Sum: **172 tests** across migration surface, 0 failures

**Cumulative session 5 grand totals (all 9 pushes):**
- **80+ commits** since session 4 end (some overwritten by
  linter formatting passes).
- ~12,000+ LOC of new typed code across resolve, typeck,
  mir-lower, repl, ast.
- typed-resolve at full body-walk parity for everything
  except smart-cast scopes.
- typed-typeck Pass 1 + Pass 2 with TypeEnv + member call
  resolution, parenthesized passthrough, top-val cycle
  detection.
- typed-mir-lower with substantial body lowering:
  - Expression bodies: literal / identity / parenthesized
  - Binary arithmetic (Int/Long/Float/Double with promotion)
  - Comparison ops (Bool result)
  - String concatenation
  - Nested binary expressions
  - println / print intrinsics with literal args
  - println with string template interpolation (PrintlnConcat)
  - Multi-statement blocks (val + println pattern)
  - Static calls (zero-arg and with literal/param args)
  - Unit-returning call → plain Return terminator
  - if/else expression (4-block CFG)
  - when expression (multi-block CFG with literal arms)
  - throw (Terminator::Throw)
- typed-mir-lower also handles every class-like decl shape:
  class / interface / object / enum class / companion object /
  nested class / secondary constructors / primary-constructor
  val/var fields / method signatures
- REPL fully migrated off legacy AST.

**Remaining multi-session work (all centered on mir-lower):**
- Class method body lowering (currently empty Return placeholder).
- while / do-while / for loops with backward goto.
- Local var mutation (assignment statements).
- Method calls on user types (`obj.method()`).
- Field access (`s.length` etc).
- Lambda lifting.
- Suspend / coroutine state machine.
- Generic functions.
- LSP migration (12 sites + DocumentState shape change).
- Driver cutover.
- Test/golden migration + legacy AST deletion.

### 2026-06-10 (session 5 — push 10)

Major expansion of mir-lower body lowering coverage:

**Class method body lowering**:
- collect_class_methods / collect_interface_methods /
  collect_object_methods now route through a unified
  method_from_fun_with_class that calls method_simple_body_with_class.
- Class methods handle: literal returns, identity-ref to params
  (offset 1 past `this`), binary ops on params, field access via
  implicit `this` (GetField rvalue), binary ops mixing param/field
  refs and literals, println/print intrinsics, throw <param-ref>,
  virtual sibling calls (no-arg).
- field_names collected from primary-ctor val/var params + body
  KtProperty entries; threaded through method body lowering so
  bare refs to fields resolve to GetField.

**Top-level fn body lowering expansions**:
- while loops: 3-block CFG with comparison cond + empty body OR
  body with single println/print call.
- do-while loops: 3-block CFG with body-first then cond.
- Block-bodied return ref / return binary: trailing
  `return <local>` switches terminator from Return to
  ReturnValue(local).
- Multi-stmt block val init from static call: `val x = helper()`
  resolves through fn_lookup to Call(Static(FuncId), []).
- Multi-stmt val with binary init: `val sum = a + b` emits
  BinOp Assign with param/local refs resolved.
- Prefix unary minus: `-x` emits BinOp(SubI, 0, x).

**Push 10 totals (focused tests, all green):**
- mir-lower: 79 unit tests (up from 60)
- All other crates unchanged
- Sum: **191 tests** across migration surface, 0 failures

End-to-end shapes lowering correctly through the typed pipeline
now include (in addition to all previous):
- `class Box(val x: Int) { fun get(): Int = x }` (field access)
- `class Box { fun double(): Int = x * 2 }` (field + binary)
- `class P { fun id(x: Int): Int = x }` (identity)
- `class P { fun add(a: Int, b: Int): Int = a + b }` (binary on params)
- `class P { fun show() = println("hi") }` (intrinsic in method)
- `class P { fun fail(e: Throwable): Nothing = throw e }`
- `class P { fun a(): Int = 1; fun b(): Int = a() }` (virtual call)
- `class Box { fun self(): Box = this }` (return this)
- `fun loop(n: Int) { while (n > 0) { println("hi") } }`
- `fun loop(n: Int) { do { println("hi") } while (n > 0) }`
- `fun foo(): Int { val x = 42; return x }` (return ref)
- `fun calc(a, b, c): Int { val ab = a + b; val sum = ab + c; return sum }`
- `fun neg(x: Int): Int = -x` (unary minus)
- `fun not(x: Boolean): Boolean = !x` (unary not as CmpEq(x, false))
- `fun main() { val x = answer(); println(x) }` (val from static call)
- `fun isStr(x: Any): Boolean = x is String` (InstanceOf)
- `fun toS(x: Any): String = x as String` (CheckCast)
- `val GREETING: String = "hi"; fun greet(): String = GREETING` (GetStaticField on wrapper class)
- `fun isEq(a: Int, b: Int): Boolean { val eq = a == b; return eq }` (val with comparison)

### 2026-06-10 (session 5 — push 11)

Mir-lower body lowering continues:
- `KtExpr::This` body lowering for class methods (returns slot 0).
- `KtExpr::Is` body lowering: `x is Int` → `Rvalue::InstanceOf`
  with kotlin_to_jvm_class descriptor mapping.
- `KtExpr::BinaryWithTypeRhs` body lowering: `x as Int` →
  `Rvalue::CheckCast` with target class descriptor.
- `KtExpr::Prefix` with `!` for Bool negation: emitted as
  `BinOp(CmpEq, x, Const(Bool(false)))`.
- Top-level val reference body via `GetStaticField` on the
  wrapper class. New `val_lookup` map collects top-level
  KtProperty names + their Ty; `ty_to_descriptor` helper
  generates JVM field descriptors.
- Multi-stmt block val init now supports comparison binary ops
  (`val eq = a == b`) producing Bool-typed locals.

**Push 11 totals (focused tests, all green):**
- mir-lower: 85 unit tests (up from 79)
- All other crates unchanged
- Sum: **197 tests** across migration surface, 0 failures

Each additional shape continues to be incrementally additive on
the existing body-lowering scaffolding. Common mid-complexity
Kotlin idioms are now covered; the remaining gaps are
control-flow inside loop bodies, lambdas, coroutines, and
generic methods.

### 2026-06-11 (session 6 — push 12: ctor + class instantiation + chains)

Substantial new coverage in `skotch_mir_lower::typed`:

**Class instantiation + class call sites:**
- `class_lookup` map (name → primary-ctor param types) built in
  pre-pass alongside `fn_lookup` and `val_lookup`. Threaded into
  `lower_simple_body`.
- `fun make(): P = P()` and `fun mk(): P = P(a, b)` lower to
  `NewInstance(P)` + `Call(Constructor(P), [new_slot, args...])` —
  args resolve through the same param/val/literal ladder.
- `fun useIt(b: Box): R = b.method(args)` — method call on a
  param of class type. Detects the `DotQualified(Reference,
  Call)` shape (NOT `Call(DotQualified, ...)` — kotlinc parses
  it as the former). Emits `Call(Virtual{class, method}, [recv,
  args...])`.
- `fun getX(b: Box): Int = b.x` — direct field access through a
  param of class type. Emits `GetField(recv, ClassName, field)`.

**Primary constructor body:**
- `constructor_from_primary` now produces a real body instead of
  empty Return:
  1. Locals: `this` at slot 0, user params at slots 1..=N
  2. Super call via `Call(Constructor(parent_or_Object), [this])`
     — super class found via `super_type_list().entries()` and
     mapped through `kotlin_to_jvm_class` so stdlib
     `RuntimeException` resolves to `java/lang/RuntimeException`.
  3. PutField for each `val`/`var` primary param.
  4. PutField for each class-body literal-init property (Int/
     Long/Float/Double/Bool — String deferred until shared
     strings table is plumbed).

**If/else-if chain support:**
- `flatten_if_chain` walks an arbitrarily-nested else-if chain
  into `[(cond, then), ...]` + final else.
- When chain ≥ 2 arms, dispatch to `try_lower_if_chain` which
  builds a 2N+2 block CFG (N cond blocks, N arm blocks, 1 else,
  1 exit).
- Operand resolver tries `literal_to_const` FIRST so
  `Prefix(-, lit)` folds via the constant-folding path instead
  of falling through to the Prefix-Reference arm.
- Bool-param conditions also accepted as the cond itself
  (no comparison needed); `!boolParam` swaps then/else block
  targets.

**Body lowering expansions:**
- Numeric literals: Long (`100L`), Float (`0.5f`), Double
  (`0.5`) via `literal_to_const` extension. Used by both
  expression-body return path and BinOp operand resolution.
- Char literal (`'A'` → `MirConst::Int(65)`); recognizes `\n`,
  `\t`, `\r`, `\\`, `\'`, `\"`, `\0`.
- Constant folding: `-42` → `MirConst::Int(-42)`; `-0.5` →
  `MirConst::Double(-0.5)`.
- Explicit `this.field` body for class methods (mirrors implicit
  `field` body's GetField emission).
- Binary operand resolver gets `KtExpr::Prefix(-, x)` support:
  `0 - x` emit, typed to match the inner operand.
- `if/else` arms accept Bool-param ref as cond + `!boolParam`
  with branch swap; arms accept Prefix-minus on param (`-x`).
- Multi-stmt block val-init binary now resolves literal
  operands via materialized Const slots.
- Multi-stmt block `return <literal>` and `return a + b`
  shapes (previously only `return <ref>`).
- Multi-stmt block var reassignment: `var x = 0; x = x + 1`
  reuses the same slot.
- Static-call body args accept top-level val refs (GetStaticField
  threaded through the arg resolver).

**Push 12 totals:**
- mir-lower: **119 unit tests** (up from 85 at start of session)
- Full workspace: **576 tests passing**, 0 failures
- Clippy: clean across changed crates

Common Kotlin idioms now lowering end-to-end through typed
pipeline:
- `class P(val x: Int)` — full ctor body with super + putfields
- `class P { val x: Int = 100 }` — class-body init in ctor
- `fun make(): P = P()` / `fun mk(): P = P(1, 2)` — class
  instantiation
- `fun get(b: Box): Int = b.x` — param.field access
- `fun call(b: Box): Int = b.method()` — virtual method call
- `fun signOf(x: Int): Int = if (x > 0) 1 else if (x < 0) -1 else 0`
  — if-else-if chain
- `fun absVal(x: Int): Int = if (x < 0) -x else x` — unary minus arm
- `fun pick(b: Boolean, a: Int, c: Int): Int = if (b) a else c`
  — bool-param cond
- `fun calc(): Int { val a = 1; val b = 2; return a + b }`
  — block with return-binary
- `fun acc(): Int { var sum = 0; sum = sum + 1; return sum }`
  — var reassignment
- `fun big(): Long = 100L`, `fun pi(): Float = 0.5f`, `fun a():
  Char = 'A'` — wide numeric + char literals

**Remaining for parity:**
- Complex control flow: for-in loops, try/catch, smart casts,
  when with subjects, lambdas
- Generic methods + type parameter erasure
- Coroutine state machine
- Custom property accessors with bodies
- Enum entry methods + companion objects with bodies
- Cross-file user-class resolution beyond the current single-file
  class_lookup

### 2026-06-11 (session 6 — push 13: try/catch, short-circuit, var reassignment)

Continued expansion of `skotch_mir_lower::typed`:

**try/catch as expression:**
- New `try_lower_try_expression` for single-catch / no-finally
  try expressions. Try and catch arm bodies must each be a Reference
  or literal (or `{ return e }` / `{ e }` block).
- 3-block CFG: try-arm (Goto exit), catch-arm (Goto exit), exit
  ReturnValue.
- `exception_handlers` entry with try_start=0, try_end=1,
  handler=1, catch_type via `kotlin_exception_class` lookup so
  `Exception` resolves to `java/lang/Exception` JVM internal name.
- `lower_simple_body` now takes `&mut Vec<ExceptionHandler>` so
  body-lowerers populate the table directly.

**Short-circuit && / ||:**
- `a && b` → if (a) b else false ; `a || b` → if (a) true else b
- 4-block CFG emit, no actual && / || in MIR (which is correct
  — JVM has no short-circuit op).
- Operands restricted to param Bool References for now.

**Var reassignment expansion:**
- `x = y` (existing local Reference) → Assign(x_slot, Local(y_slot))
- `x = compute()` (zero-arg top-level fn call) → Assign(x_slot,
  Call(Static(fid), []))
- Plus existing literal + binary RHS arms.

**Method body block walker:**
- `try_lower_multi_stmt_block_with_offset` factors out the
  parameter-slot starting offset. Class methods set `slot_offset=1`
  so `this` occupies LocalId(0) and user params shift to slots
  1..=N.
- `method_simple_body_full` tries this walker first for block
  bodies; falls back to the single-return extraction.

**If/else expansions:**
- If-arms accept zero-arg top-level fn calls (`if (...) helper(x)
  else x`).
- Bool-param ref and `!boolParam` as cond.

**Class body init via top-level fn:**
- `class P { val x = compute() }` now produces a real ctor body:
  Call(Static(compute), []) + PutField(this, P, x, slot) after the
  super call.

**Push 13 totals:**
- mir-lower: **130 unit tests** (up from 119)
- Full workspace: passing (exit 0), 0 failures
- Workspace clippy: clean (skotch + xtask, 7m29s wall-clock)

Common idioms now lowered end-to-end through the typed pipeline
include all the prior shapes plus:
- `fun parse(): Int = try { 1 } catch (e: Exception) { 0 }`
- `fun and(a: Boolean, b: Boolean): Boolean = a && b`
- `fun or(a: Boolean, b: Boolean): Boolean = a || b`
- `fun cond(x: Int): Int = if (x > 0) helper(x) else x` (if-arm Call)
- `class P { val x: Int = compute() }` (init-via-fn-call)
- `class P { fun answer(y: Int): Int = helper(y) }` (method-static-call)
- `class P { fun calc(): Int { val a = 1; val b = 2; return a + b } }`
  (method block walker)
- `fun acc(): Int { var x = 0; x = compute(); return x }` (var
  reassignment from call)

**Architectural note** — every new shape so far is incremental
additive code. The typed pipeline now handles a substantial slice of
real Kotlin idioms, but reaching full fixture parity (1300+ goldens)
requires several more sessions of pattern coverage plus the eventual
driver cutover.

### 2026-06-11 (session 6 — push 14: string templates + when ref arms)

**String-template body emission:**
- `fun greet(name: String): String = "Hello, $name"` →
  `Call(MakeConcatWithConstants{recipe, descriptor}, args)` matching
  JVM 9+ invokedynamic shape kotlinc emits.
- Recipe: `"Hello, \x01"` (literal chunks interleaved with `\x01`
  placeholders).
- Descriptor: `(arg_jvm_types)Ljava/lang/String;` built from each
  interpolated arg's param type.
- Multi-interp shapes like `"a=$a, b=$b"` produce
  `(II)Ljava/lang/String;`.

**when arm body via Reference:**
- `when (x) { 1 -> a; 2 -> b; else -> 0 }` where a, b are outer params
  now lowers. Previously arm bodies were restricted to literals.
- Both arm bodies and else body fall through Reference resolution
  against the outer param list when literal_to_const returns None.

**Multi-stmt println arg via Binary:**
- `println(x + 1)` in a block body now materializes the BinOp into a
  fresh slot before the Println intrinsic call.

**Implicit-this virtual call with args:**
- `class P { fun a(x: Int) = x; fun b(y: Int): Int = a(y) }` →
  Call(Virtual{P, a}, [this, y_slot]). Previously zero-arg only.

**Method block walker offset-1:**
- `try_lower_multi_stmt_block_with_offset` factored out the
  parameter-slot starting offset so class methods can reuse the
  whole walker (var reassignment, val init, return shapes) with
  `this` at slot 0 + params at 1..=N.

**Push 14 totals:**
- mir-lower: **136 unit tests** (up from 130)
- Full workspace: **593 tests passing**, 0 failures
- Workspace clippy: clean

Common idioms now lowered end-to-end through the typed pipeline now
also include:
- `fun greet(name: String): String = "Hello, $name"` (single interp)
- `fun fmt(a: Int, b: Int): String = "a=$a, b=$b"` (multi interp)
- `fun pick(x: Int, a: Int, b: Int): Int = when (x) { 1 -> a;
  2 -> b; else -> 0 }` (when ref arms)
- `fun show(x: Int) { println(x); println(x + 1) }` (Binary arg)
- `class P { fun a(x: Int) = x; fun b(y: Int): Int = a(y) }`
  (implicit-this virtual call with args)
- `class P { fun acc(): Int { var sum = 0; sum = sum + 1; return sum } }`
  (method block + var)
