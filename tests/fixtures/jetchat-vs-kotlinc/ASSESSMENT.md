# JetChat: skotch vs kotlinc compilation gap analysis

## Setup

Two builds of the same JetChat sources:

* **gradle (kotlinc)** — `./gradlew :app:compileDebugKotlin`
  output: `app/build/intermediates/built_in_kotlinc/debug/compileDebugKotlin/classes` — **92 classes**.
* **skotch** — `skotch build --target android` (then the .class files before d8 conversion)
  output: `build/d8-input-original/com/example/compose/jetchat/**` — comparable subset matches.

Comparison: `diff_jetchat.sh` runs `javap -p -c` on each pair and dumps diffs into
`diffs/<fqcn>.diff`. Per-class line counts in `summary.tsv`.

## Headline numbers

| Bucket                     | Count |
|----------------------------|------:|
| Identical                  | **0** |
| Differs                    |    38 |
| Missing entirely in skotch |    54 |

**No class compiles byte-identically.** Every diff is at least cosmetic (field
flags, source-file attribute) and most are structural.

## Missing-class taxonomy (54 total)

| Pattern                                                  | Count | Source                                                                              |
|----------------------------------------------------------|------:|-------------------------------------------------------------------------------------|
| `*$$inlined$<fn>$N` (per-callsite inlined-lambda class)  |    27 | kotlinc inlines `animateFloat`, `remember`, `LaunchedEffect`, `activityViewModels`. |
| `ComposableSingletons$<File>Kt` (stable-lambda hoists)   |    14 | kotlinc hoists capture-free content lambdas to a per-file singleton holder.         |
| `Outer$body$N$inner$M…` (deep nested-lambda anonymous)   |     7 | kotlinc names lambdas by lexical nesting; skotch flattens to `Outer$Lambda$N`.      |
| `*$WhenMappings` (sealed-when enum-ordinal pivot)        |     1 | kotlinc materializes a small `int[]` per when-on-enum site.                         |
| Misc (`*$Indicator$1$1`, `*$Messages$1$2$1$1`)           |     5 | Same deep-nesting issue.                                                            |

These are all kotlinc *synthetic* outputs we don't (yet) produce. Each maps to a
specific compiler feature skotch lacks.

## Differing-class diff buckets (38 classes; ordered by impact)

### 1. Property/field machinery (every class affected)

* **`private final` → `public`** on every backing field.
  kotlinc emits `private final` and synthesizes a public getter; skotch emits the
  field as `public` directly with no getter. Breaks JVM accessor conventions and
  callers that go through `getX()`.
* **Erased generic types in field signatures.**
  kotlinc keeps `MutableStateFlow<Boolean>` as the signature with a `Signature`
  attribute and a typed descriptor; skotch types the field as `java.lang.Object`
  for anything not statically resolved. Example:
  `MainViewModel.drawerShouldBeOpened: Ljava/lang/Object;` (skotch) vs
  `Lkotlinx/coroutines/flow/StateFlow;` (kotlinc).
* **`$stable` field missing.**
  Every `@Stable`/`@Immutable` class in Compose gets a `public static final int
  $stable` written into `<clinit>`. Skotch never emits it; downstream Compose
  skip-checks see `stable=0` and over-recompose (but won't crash).
* **No `<init>` property-initializer lowering.**
  `class MainViewModel { val flow = MutableStateFlow(false) }` should emit the
  initializer call inside `<init>` (kotlinc does). Skotch emits an empty `<init>`
  and the field stays uninitialized → NPE on first read.
* **`Intrinsics.checkNotNullParameter` missing.**
  kotlinc inserts `checkNotNullParameter` calls at the top of every method with
  non-null reference parameters. Skotch never inserts these — Kotlin null-safety
  invariants aren't enforced at runtime.

### 2. Enum-class lowering

* **Enums don't extend `java.lang.Enum`.**
  `enum class ExpandableFabStates { Collapsed, Extended }` is lowered by kotlinc
  as `final class extends Enum<E>` with synthesized `values()`, `valueOf()`,
  `getEntries()`, `$VALUES`, `$ENTRIES`. Skotch lowers it as
  `class ExpandableFabStates { public String name; }` with no enum runtime
  support — completely separate type that fails `instanceof Enum`, has no
  `.ordinal()`, no reflection. Affects 5 enums (ExpandableFabStates, Visibility,
  EmojiStickerSelector, SymbolAnnotationType, others).
* **`$WhenMappings` companion array missing** — kotlinc emits a per-callsite
  `int[]` mapping enum ordinal → branch index for `when (x: SomeEnum)` dispatch.

### 3. Property-call dispatch

* **Property writes target field on an interface.**
  `_drawerShouldBeOpened.value = true` is lowered as
  `putfield kotlinx/coroutines/flow/MutableStateFlow.value:Z` — but
  `MutableStateFlow` is an *interface*, has no field, has a `setValue(Object)`
  method (the actual property setter). kotlinc emits
  `invokeinterface MutableStateFlow.setValue(Ljava/lang/Object;)V` with a boxed
  Boolean. Skotch's call dispatch confuses property-as-field-access vs
  property-as-setter.

### 4. Method overload + name machinery

* **`copy$default` (synthetic data-class copy dispatcher) not emitted.**
  Skotch emits `copy(...)` but the synthetic `copy$default(this, args..., mask,
  marker)` used by callers with default params is missing.
* **`@Composable` function descriptors emit `(Object^N)Object` fallback** rather
  than the real param types when classinfo can't resolve them. Already
  documented in #292 / #293.
* **Mangled value-class names (`get<Prop>-0d7_KjU`)** were addressed in #291 /
  #298 but the diff harness shows many other mangled forms (e.g.
  `darkColorScheme-_VG5OTI`, `measure-BRTryo0`) where the caller still emits
  the unmangled name.

### 5. @Metadata annotation entirely missing

Every kotlinc class file carries a `Lkotlin/Metadata;` annotation with proto-
encoded data: function names, param names, generic signatures, visibility, etc.
Skotch emits *no* Kotlin metadata at all. Side effects:

* Cross-module Kotlin features (named args, default params on external classes,
  reflection, `kotlin-reflect`) can't see param names → drives most of the
  rendering bugs in JetChat.
* IDE/tools can't read Kotlin-level structure from skotch's output.
* `KClass.simpleName`, `Class.isData`, etc. return wrong values at runtime.

### 6. Source file attribute drift (cosmetic)

`Compiled from "Themes.kt"` (kotlinc) vs `Compiled from "com/example/compose/jetchat/theme/Themes.kt"`
(skotch). Cosmetic; debug stack traces still work but show the full path.

### 7. Lambda-class naming + count

* kotlinc names anonymous lambdas by **lexical position**:
  `Foo$body$2$1$1.class`. Skotch flattens all lambdas in a file to
  `FooKt$Lambda$N.class` with arbitrary N.
* The class counts also differ: kotlinc emits more classes (one per lambda
  capture site, plus `ComposableSingletons` singletons). Skotch's per-file flat
  numbering shadows kotlinc's tree numbering.

### 8. Inlining (root cause of most missing classes)

skotch does no Kotlin function inlining. kotlinc inlines:

* `inline fun <reified T> activityViewModels()` → emits one
  `$special$$inlined$activityViewModels$default$N` per call site (3 in
  `ConversationFragment`).
* `inline fun animateFloat(...)` → emits one
  `$$inlined$animateFloat$N` per call site (4 in `AnimatingFabContent`).
* `inline fun remember(...)`, `LaunchedEffect(...)`, etc. — many more.

Without inlining the body has to call the function out-of-line, which means the
synthesized lambda classes are never created at all (the lambda lives inside
the called function instead). This explains 27 of the 54 missing classes.

## What it would take to close each gap

| Gap                                          | Effort  | Unblocks                                                       |
|----------------------------------------------|---------|----------------------------------------------------------------|
| Emit `private final` + getter for properties | Medium  | StateFlow / Compose property reads everywhere                  |
| Run property initializers in `<init>`        | Medium  | MainViewModel and any `val foo = X()` style classes            |
| Resolve `property.value = x` to setter call  | Medium  | All MutableStateFlow / mutableStateOf writes                   |
| Enum class lowering to `extends Enum`        | Large   | All enum usage (5 enums in JetChat); reflection                |
| `$WhenMappings` table for when-on-enum       | Small   | Sealed-class / enum when-dispatch                              |
| Generic signatures in field/method descs     | Medium  | Generic-aware Compose APIs                                     |
| Insert `Intrinsics.checkNotNullParameter`    | Small   | Runtime null-safety; no actual rendering impact                |
| Emit `$stable` for @Stable/@Immutable        | Small   | Compose skip-checks (perf, not correctness)                    |
| Emit `copy$default` for data classes        | Small   | Anyone calling `dataClassInstance.copy(field=…)`               |
| Emit `kotlin.Metadata` annotation            | **XL**  | Named args, default-param dispatch, reflection — JetChat UI    |
| Function inlining (`inline fun`)             | **XL**  | Compose APIs (`remember`, `animateFloat`, …); 27 missing class |
| `ComposableSingletons$XKt` lambda hoist      | Medium  | Compose performance; 14 missing classes                        |
| Mangled-name emit at call site (full)       | Medium  | Already partly in place (#291, #298) but incomplete            |

## Progress (running tally)

Updates as fixes land; commit `summary.tsv` to see line-counts drop.

| Date       | Fix                                                                    | Tasks   |
|------------|------------------------------------------------------------------------|---------|
| 2026-05-19 | Synthetic `public final getX():T` getters for every class field        | #304    |
| 2026-05-19 | Non-literal property initializers run inside `<init>` via FnBuilder    | #305    |
| 2026-05-19 | Interface call dispatch finds sibling Kt facade (`MutableStateFlow`)   | #305    |
| 2026-05-19 | Prim/ref mismatch check allows autobox to `Object`/`Number`/`Any`      | #305    |
| 2026-05-19 | Non-`open` non-abstract Kotlin classes emit with `ACC_FINAL`           | #307    |
| 2026-05-20 | `Foo::method` desugar re-shapes to FunctionN when callsite expects N>1 | #308    |
| 2026-05-20 | External Kt-facade descriptor lookup feeds expected-arity to expansion | #308    |
| 2026-05-20 | Trailing-lambda placement skipped when positional arg fits slot 0      | #308    |
| 2026-05-20 | `Any → String`/`Any → Class(C)` autoboxes via `checkcast` at staticjava | #308    |

Now MainViewModel correctly emits
`invokestatic StateFlowKt.MutableStateFlow(Object)MutableStateFlow` and stores it
in `_drawerShouldBeOpened`. The second val (`drawerShouldBeOpened =
_drawerShouldBeOpened.asStateFlow()`) is still null-stubbed pending `asStateFlow`
extension-method resolution (#306-area).

## Recommended next-iteration order

1. **Property semantics rewrite** (gaps 1+3, medium effort) — fixes the
   `MutableStateFlow.value = x` bug that blocks MainViewModel and unblocks
   every `mutableStateOf` write in Compose. This single change cascades.
2. **`<init>` property initializers** — required to make any class with
   property initializers actually work.
3. **Kotlin Metadata emission** — the gateway for proper named-arg + default-
   param dispatch. Once param names are visible to skotch's call lowering, the
   pile of null-stub hacks (`#287`, `#289`, `#292`, `#293`, `#295`) can be
   replaced with real dispatch.
4. **Enum class lowering** — biggest correctness gap; small set (5 enums) but
   each is structurally wrong. Affects `when` dispatch widely.
5. **Function inlining** — the largest single feature. Most Compose APIs assume
   inlining; without it the code path is structurally different. Postponable
   until the above are done since skotch's out-of-line calls still execute
   (just less efficiently / differently named).

The diff harness in `tests/fixtures/jetchat-vs-kotlinc/` re-runs in seconds and
generates `summary.tsv` + per-class `.diff` files — usable as goldens for any
of the above fixes.
