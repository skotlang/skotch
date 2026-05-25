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

---

## 2026-05-24 refresh (rebuilt both sides)

Rebuilt the oracle (`./gradlew :app:compileDebugKotlin` under **JDK 17** — JDK 26
fails the Gradle script with "Unsupported class file major version 70") and the
skotch side with the current binary, then re-ran `diff_jetchat.sh`.

**Headline unchanged: 0 identical · 38 differ · 54 missing.** The intervening
type-inference work (enum singletons, remember-/iterable-unifier, collection
builders) did not move JetChat's diffs — JetChat's gaps are the *big* compiler
features (inlining, full `@Composable` lowering, `@Metadata` emission), not the
small inference cases. The big composables still emit ~15–25 % of kotlinc's
bytecode (e.g. `UserInputKt` 1586 vs 8910 javap lines) — i.e. partial/stubbed
bodies, not full lowering.

### Concrete fix landed this session — build no longer stalls

`skotch build` on JetChat **hung at 98 % CPU with 0 classes emitted** (killed at
14 min). `sample` pinned it to `lower_expr → find_external_static_descriptor →
lookup_method_descriptor → load_jdk_class → load_class_from_jar →
ZipArchive::new`: on every CLASSPATH-cache *miss* (the many Compose/AndroidX
classes that aren't in the JDK), `load_jdk_class` re-ran its **entire** search —
open `java.base.jmod`, iterate *all* jmods, scan CLASSPATH jars, scan Kotlin
stdlib jars — re-reading each archive's central directory from scratch, thousands
of times during MIR-lowering. Added a process-wide memo (`JDK_CLASS_CACHE`,
`crates/skotch-classinfo/src/lib.rs`) caching hits **and** misses (the classpath
is fixed for a build). Build now completes in **~5.4 min, 300 classes** instead of
hanging. (Still slow — other lookup paths likely re-open dep jars too; a shared
`ZipArchive` handle cache is the obvious follow-up.)

### New/changed observations since the last assessment

* **`$stable` now emitted** (gap closed, #307) — present on `MainViewModel` etc.
* **Enums are now reference-correct but shaped unlike kotlinc.** The new
  singleton lowering emits `public static final <Enum> ENTRY` + `<clinit>` so
  `==` works, but skotch still (a) does **not** `extends java/lang/Enum`, and
  (b) leaks per-entry accessor fns (`VISIBLE()`, `Visibility$values()`,
  `Visibility$valueOf()`) onto the **file facade** (e.g. they show up inside
  `JumpToBottomKt`). kotlinc inlines `getstatic` at refs and puts
  `values()`/`valueOf()` on the enum class. Correct at runtime, wrong shape.
* **`@Composable` default-arg dispatcher param missing.** kotlinc:
  `JumpToBottom(boolean, Function0, Modifier, Composer, int, int)` — two trailing
  ints (`$changed` + `$default` mask). skotch emits only one (`…Composer, int`),
  so default-argument call sites can't pass the mask. Also a spurious `<T>`
  type-param is inferred on some composables.
* **Restart-scope / extra lambdas still partial.** kotlinc's `JumpToBottom` has
  `lambda$1` (the `State<Dp>` reader) + `lambda$2` (restart scope); skotch emits
  one mis-typed `lambda$0(Object)`.
* **Property machinery still open** (gaps 1/3 below): fields are `public` raw
  (not `private final` + `Signature`), `drawerShouldBeOpened` is `Object`
  because `.asStateFlow()`'s return type isn't inferred, and skotch emits
  getters for *private* backing fields that kotlinc never exposes.

### Major differences & what they need (priority order, this refresh)

| Gap | Where it shows | Effort | Required work |
|-----|----------------|--------|---------------|
| **Function inlining** (`inline fun`) | 27 of 54 missing classes (`$$inlined$animateFloat$N`, `viewModels`, `remember`, `LaunchedEffect`) | **XL** | Inline-body copy with per-call-site lambda-class synthesis + reified-type substitution. Biggest single lever; most Compose APIs assume it. |
| **`@Composable` lowering completeness** | every big composable (15–25 % body size) | **L** | `$default` mask param; full Composer threading; restart-scope + replaceable-group lambdas; correct lambda descriptors. |
| **`ComposableSingletons$XKt` hoist** | 14 missing classes | **M** | Hoist capture-free content lambdas into a per-file singleton holder with `getLambda$N` accessors. |
| **`@Metadata` emission** | all 92 classes (we now *read* it, #297, but never *write* it) | **XL** | Encode the protobuf + `BitEncoding` we already decode; unblocks named-args / default-param dispatch / reflection. |
| **Property/field machinery** | every class with state | **M** | `private final` + synthetic getter; generic `Signature` on fields; infer `asStateFlow()`/extension returns instead of `Object`. |
| **Enum shape parity** | 5 enums + their `when` sites | **M** | `extends java/lang/Enum`; move `values`/`valueOf` onto the enum class; inline `getstatic` at refs instead of facade accessors; `$WhenMappings`. |
| **Nested-lambda lexical naming** | 7 missing classes | **M** | Name anonymous lambdas by lexical nesting (`Outer$body$2$1`) instead of flat `Kt$Lambda$N`. |
| **Build perf (follow-up)** | whole build (~5 min) | **S–M** | Cache opened `ZipArchive` handles / parsed central directories across all dep-jar lookups (the JDK path is now cached; dep jars still re-open). |

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
| 2026-05-24 | `$stable` confirmed emitted; enums now reference-correct singletons       | #307/enum |
| 2026-05-24 | `JDK_CLASS_CACHE` memo — build no longer stalls (hung→~5.4 min, 300 cls)  | #347    |

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
