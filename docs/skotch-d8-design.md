# `skotch d8` — native Rust DEX compiler: design & implementation plan

Status: **proposed** (planning only — no code in this round).
Reference: Android `d8` **8.10.9-dev** (`$ANDROID_HOME/build-tools/36.0.0/d8`),
sources in `android/r8/` (label `main`).

---

## 1. Goal and scope

Provide a self‑contained `.class → .dex` compiler so that Skotch can assemble
Android APKs **without shelling out to the SDK `d8`**. This is required — not
optional — because real projects depend on Maven `.jar` / `.aar` artifacts that
ship only compiled JVM `.class` files (no MIR, no source, never DEX). Today the
build pipeline shells out to the real `d8` (`crates/skotch-build/src/pipeline.rs`
`compile_app_classes_with_d8` / `Command::new(d8)`); this is the last
load‑bearing SDK‑tool shell‑out after `aapt2` (`skotch-aapt2`) and `apksigner`
(`skotch-apksig`) were natively reimplemented.

**This round delivers D8 (the dexer + desugarer), not R8 (the optimizer).**
R8 is whole‑program shrinking/inlining/minification — it affects app *size*, not
*correctness*; a working APK never needs it. But the architecture is designed so
an `skotch-r8` driver can be added later **by addition, not refactoring** (§8).

### In scope (this round)
- A standalone tool, surfaced as **`skotch d8`** and as a multi‑call alias
  (`("d8", "d8")` in `crates/skotch-cli/src/multicall.rs`, already stubbed).
- Reads `.class`, `.jar`, `.zip`, `.apk`, and `.dex` inputs; writes `.dex`
  (directory or zip), matching most of the real `d8` CLI (§6).
- The three desugarings that real dependencies actually need at common
  `--min-api`: lambdas (`invokedynamic`/`LambdaMetafactory`), default & static
  interface methods, string concatenation (`makeConcatWithConstants`), plus a
  table of backported `java.*` methods.
- Multidex splitting (64K reference limit), debug vs release.
- Independent of Skotch's own compiler backends (no dependency on
  `skotch-mir`, `skotch-mir-lower`, `skotch-backend-jvm`, `skotch-backend-dex`).

### Out of scope (this round)
- R8 (tree shaking, optimization, minification) — designed‑for, not built.
- `--desugared-lib` / L8 desugared‑library backports of `java.time`,
  `java.util.stream`, etc. (phase‑2+).
- `--art-profile`, `--startup-profile` dex layout, `--pg-map` application
  (parsed/passed‑through but not acted on initially).
- Exact byte‑parity with `d8`. DEX is **not** a canonical format the way a
  signed APK is — register allocation and instruction selection are compiler
  choices, two correct dexers differ in bytes, and r8 ships no golden `.dex`.
  Our oracle is **structural + behavioral equivalence** (§9), not byte identity.
- Retiring Skotch's current MIR→DEX path. **Leave it untouched** for now; §10
  describes the eventual switch‑over as separate future work.

---

## 2. What D8 does, and the r8 code we lean on

D8's pipeline (the parts we replicate), with the `android/r8` packages that
implement each stage:

```
.class/.jar/.dex inputs
   │  read + parse                 graph/JarClassFileReader, cf/code/* (62 files), cf/CfCode
   ▼
CF model (per-class, per-method CfCode)
   │  desugar (CF → CF)            ir/desugar/* — CfInstructionDesugaring,
   │                              LambdaClass, InterfaceMethodRewriter,
   │                              D8StringConcat, BackportedMethodRewriter
   ▼
desugared CF model
   │  build IR (per method)        ir/conversion/CfSourceCode + IRBuilder → ir/code/* (148 files, SSA)
   ▼
high-level SSA IR
   │  register allocation          ir/regalloc/LinearScanRegisterAllocator (+~17 support classes)
   │  IR → DEX instruction select  ir/conversion/DexBuilder → dex/code/* (277 instruction classes)
   ▼
DEX code per method
   │  index assignment, layout     dex/ApplicationWriter, FileWriter, IndexedItemCollection,
   │  multidex split, write        MixedSectionCollection, VirtualFile, ObjectToOffsetMapping
   ▼
classes.dex (+ classes2.dex …)
```

The single most important fact for our re‑use goal: **D8 and R8 share everything
in that diagram.** R8 is the *same* reader → desugar → IR → regalloc → DexBuilder
→ FileWriter, with two additions wrapped around it:
1. a richer **global knowledge** object (`AppInfoWithLiveness` instead of D8's
   `AppInfo`), produced by the tree‑shaking `Enqueuer`; and
2. a longer **IR optimization pass list** in the `IRConverter`.

Everything is parameterized over an `AppView<Info>`. That seam is what we
replicate so R8 is additive (§8).

---

## 3. Crate layout

Four new crates, forming an island independent of Skotch's compiler. Names map
to r8 packages; the split is chosen so the format leaves are reusable and the
compiler core is shared by D8 and a future R8.

| Crate | Role | r8 analogue | Depends on |
|---|---|---|---|
| `skotch-dex` | DEX file **format**: item model, opcode tables, LEB128/MUTF‑8, binary **writer** and **reader**, checksums, map_list | `dex/`, `graph/Dex*` | byteorder, sha1, adler |
| `skotch-classfile` | JVM `.class` **reader**: constant pool, members, `Code`, `StackMapTable`, all CF instructions, annotations | `cf/`, `graph/JarClassFileReader` | byteorder |
| `skotch-dexcore` | Shared compiler core: program **graph**, **SSA IR**, **IRBuilder** (CF→IR), **regalloc**, **DexBuilder** (IR→DEX), **desugar** framework + core desugarings, **`AppView<Info>`** seam, IR **pass** pipeline | `graph/`, `ir/code`, `ir/conversion`, `ir/regalloc`, `ir/desugar` | `skotch-dex`, `skotch-classfile`, rayon |
| `skotch-d8` | The D8 **driver**: option parsing, input reading, orchestration, multidex, output; the thin "minimal optimization" processor | `D8.java`, `D8Command.java`, `PrimaryD8L8IRConverter` | `skotch-dexcore`, `skotch-dex`, `skotch-classfile`, rayon |
| *(future)* `skotch-r8` | R8 driver: `Enqueuer` (shaking), optimization passes, minifier | `R8.java`, `enqueue/`, `optimize/`, `naming/` | `skotch-dexcore` (unchanged) |

CLI wiring in `skotch-cli`: a `Command::D8 { args: Vec<String> }` variant
(trailing‑var‑arg, `disable_help_flag`, like `Kotlinc`/`Apksigner`), a
`src/d8.rs` module, and uncommenting `("d8", "d8")` in `multicall.rs`.

> Rationale for two format‑leaf crates: `skotch-dex` and `skotch-classfile` are
> stable, dependency‑light, individually testable, and reusable (e.g.
> `skotch-dex` could eventually back the current MIR→DEX path too — but not in
> this round). `skotch-dexcore` holds the parts that are genuinely shared
> between D8 and R8.

---

## 4. Parallelism model

DEX compilation is embarrassingly parallel at method granularity, then has a
single‑threaded global assembly phase — the same shape as `skotch-backend-dex`
and the apksig two‑pass approach.

```
phase A (parallel, rayon par_iter over methods):
    for each method:  desugar → build IR → regalloc → DexBuilder
    → produces (DexCode, set<symbolic refs: strings/types/protos/fields/methods>)
    Each method collects its symbolic references into thread-local sets.

phase B (single-threaded):
    merge all symbolic-reference sets → sort per DEX spec → assign final indices
    multidex split (assign classes to virtual files under the 64K limit)

phase C (parallel per output dex file):
    patch each method's instructions with final indices
    lay out sections, write bytes, compute Adler32 + SHA-1
```

Class‑*synthesizing* desugarings (lambda classes, interface companions) run in a
pre‑pass that may add classes/methods to the program; once the program set is
fixed, phase A parallelizes cleanly. The interner (`DexItemFactory` analogue)
is **not** a shared mutable bottleneck: phase A uses thread‑local collection,
phase B merges. This avoids lock contention and is deterministic (sort in B
gives stable indices regardless of thread scheduling).

`--thread-count` maps to a rayon thread pool size.

---

## 5. Component design

### 5.1 `skotch-dex` (format)
- **Item model**: `DexString` (MUTF‑8), `DexType`, `DexProto`, `DexField`,
  `DexMethod`, `DexMethodHandle`, `DexCallSite` (for `invoke-custom`),
  `DexClassDef`, `DexCode`, `DexInstruction` (typed per format), `EncodedField/
  Method`, `Annotation*`, `DebugInfo`.
- **Opcode tables**: all DEX formats (10x, 12x, 22x, 35c, 3rc, 31i, 51l, …) with
  size + operand layout; encode/decode. Mirror `dex/code/*` (277 classes) but as
  a data‑driven table where possible to avoid 277 Rust types.
- **Writer**: section layout (header, string_ids, type_ids, proto_ids,
  field_ids, method_ids, class_defs, call_site_ids, method_handles, data,
  map_list), offset back‑patching, Adler32 + SHA‑1 (reuse the approach in
  `skotch-backend-dex/writer.rs`; do **not** modify that crate).
- **Reader**: parse a `.dex` into the item model — needed for `.dex` inputs,
  merging, and the normalizer/validator (§9). r8: `dex/DexParser`,
  `DexReader`.
- **Validator**: re‑parse + internal‑consistency checks (offsets in bounds,
  pools sorted & deduped, map ordering, sizes) used as a cheap offline oracle.

### 5.2 `skotch-classfile` (reader)
- Full constant pool (incl. `InvokeDynamic`, `MethodHandle`, `MethodType`,
  `Dynamic`), access flags, fields, methods, `Code` (max_stack/locals,
  bytecode, exception table), `StackMapTable`, `BootstrapMethods`,
  `LineNumberTable`, `LocalVariableTable`, annotations, `InnerClasses`,
  `Signature`, nest attributes.
- A typed CF instruction stream (the ~200 JVM opcodes) → consumed by the IR
  builder. Mirror r8 `cf/code/*`.
- Reads from a single `.class` or iterates a `.jar`/`.zip`/`.apk` (zip entry
  walk). r8: `JarClassFileReader`.
- *Evaluate but do not assume reuse* of `skotch-classinfo` /
  `skotch-classfile-norm` — those parse subsets; we need the full attribute set,
  including `Code` and `StackMapTable`. Likely a fresh, complete reader; share
  only trivial helpers.

### 5.3 `skotch-dexcore` (shared core)
- **graph**: `Program`, `DexClass`, `DexEncodedMethod`/`Field` holding *either*
  `CfCode` or `DexCode` (so D8 can pass `.dex` inputs through and merge). r8:
  `graph/`.
- **ir/code**: SSA IR — `BasicBlock`, `Value` (SSA def), `Phi`, `Instruction`
  (Const, Binop, Compare, If, Goto, Invoke{Static,Virtual,Direct,Interface,
  Custom}, InstanceGet/Put, StaticGet/Put, ArrayGet/Put, NewInstance, NewArray,
  CheckCast, InstanceOf, MonitorEnter/Exit, Throw, Return, Switch, …),
  `Position` (debug). r8: `ir/code/*` (148 files).
- **ir_builder**: CF → SSA IR per method, building basic blocks from the
  bytecode CFG, SSA construction (dominance‑frontier phi placement or the
  simpler on‑the‑fly local‑value tracking r8's `CfSourceCode` + `IRBuilder`
  uses), exception‑handler edges, types from `StackMapTable` / abstract
  interpretation. r8: `CfSourceCode`, `IRBuilder`.
- **regalloc**: `LinearScanRegisterAllocator` — live‑interval computation,
  linear scan, spill insertion, **DEX register constraints** (the hard part:
  non‑range `invoke` needs args in v0–v15; `*-range` forms need a contiguous run;
  wide values occupy register pairs; `move-result` adjacency). r8:
  `ir/regalloc/*`.
- **dexbuilder**: IR → typed `DexInstruction`s — instruction selection
  (`const/4` vs `const/16` vs `const` vs `const/high16` by value; `if-*` vs
  `if-*z`; invoke kind & format; binop register widths), branch‑offset sizing,
  `try`/`catch` table emission, debug‑info stream. r8: `DexBuilder`.
- **desugar**: a `Desugaring` trait set mirroring r8's three kinds —
  `CfInstructionDesugaring` (rewrite one instruction, may request a synthetic
  method), `CfClassSynthesizerDesugaring` (synthesize whole classes), and
  `CfPostProcessingDesugaring`. Core implementations:
  - `LambdaDesugaring`: `invokedynamic` → `invoke-custom` when
    `min-api ≥ 26`, else synthesize a lambda class (r8: `LambdaClass`,
    `LambdaRewriter`).
  - `InterfaceMethodDesugaring`: default/static/private interface methods →
    companion class + forwarders when `min-api < 24` (r8:
    `InterfaceMethodRewriter`).
  - `StringConcatDesugaring`: `makeConcatWithConstants` → `StringBuilder`
    chain (r8: `D8StringConcat`).
  - `BackportedMethodRewriter`: table of `java.*` static methods backported for
    low `min-api` (start with the common ~50; r8 has a large table).
- **appview**: `AppView<I: AppInfo>` carrying min‑api, the class hierarchy /
  library resolution (`--lib`, `--classpath`, `android.jar`), and the `DexItemFactory`
  interner. D8 supplies a plain `AppInfo`; R8 will later supply
  `AppInfoWithLiveness`. **All IR/desugar/dex code is generic over `I`.**
- **passes**: a `Pass` trait and a pass runner. D8 runs a near‑empty list (dead
  code from desugaring, trivial peepholes); R8 will register many. The runner is
  the `IRConverter` analogue; D8's processor is `PrimaryD8L8IRConverter`.

### 5.4 `skotch-d8` (driver)
- Option model (`D8Command` analogue) + `OptionsParser` (reuse the pattern from
  `skotch-apksig` CLI: `--name value` / `--name=value`, `@argfile` expansion,
  repeatable `--lib`/`--classpath`, multiple positional inputs).
- Orchestration: read inputs → build program graph → run class‑synth desugar
  pre‑pass → parallel phase A → phase B index/multidex → phase C write.
- Output sinks: single `classes.dex`, multidex `classesN.dex`,
  `--file-per-class[-file]`, dir or zip. `--intermediate` / `--globals[-output]`
  for the synthetic‑sharing merge protocol (phase‑3).

---

## 6. CLI surface (vs real `d8`)

| Option | MVP | Notes |
|---|---|---|
| `<input-files>` (`.class/.jar/.zip/.apk/.dex`) | ✅ | `.dex` input ⇒ pass‑through/merge |
| `@<argfile>` | ✅ | one arg per line |
| `--output <dir\|zip>` | ✅ | |
| `--min-api <n>` | ✅ | gates desugaring + opcode availability |
| `--release` / `--debug` | ✅ | debug ⇒ emit line/local debug info |
| `--lib <file\|jdk>` / `--classpath <file>` | ✅ | type/hierarchy resolution; `android.jar` |
| `--no-desugaring` | ✅ | |
| `--file-per-class` / `--file-per-class-file` | ✅ (phase 3) | |
| `--intermediate`, `--globals`, `--globals-output` | ◻ phase 3 | global‑synthetics merge protocol |
| `--main-dex-list`, `--main-dex-list-output` | ◻ phase 3 | primary‑dex pinning |
| `--main-dex-rules` | ◻ later | needs keep‑rule parser |
| `--thread-count <n>` | ✅ | rayon pool |
| `--force-(en/dis)able-assertions`, `--force-ah` | ◻ phase 4 | javac/kotlinc assertion rewriting |
| `--map-diagnostics`, `--version`, `--help` | ✅ | |
| `--pg-map`, `--art-profile`, `--startup-profile`, `--android-platform-build` | ◻ later | parse/accept; act later |
| `--desugared-lib`, `--desugared-lib-pg-conf-output` | ◻ later (L8) | |

"✅ MVP" = phases 1–2; "◻ phase 3/4/later" = subsequent.

---

## 7. R8‑readiness (built this round as *seams*, not code)

To make R8 additive later, three abstractions are designed in now:

1. **`AppView<I: AppInfo>`** — every consumer (IRBuilder, each desugaring, the
   DexBuilder, regalloc) takes `&AppView<I>`. D8 instantiates `I = AppInfo`
   (hierarchy only). R8 later instantiates `I = AppInfoWithLiveness` (adds
   reachability/keep info) **without changing those consumers**.
2. **Pass pipeline** — the IR converter runs `&[Box<dyn Pass>]`. D8 passes a
   tiny list. R8 registers optimization passes against the *same* IR. No
   consumer hard‑codes "D8" or "R8".
3. **Program mutation API** — desugaring and (future) shaking both add/remove
   classes & methods through one `ProgramBuilder` API, so the Enqueuer reuses it.

Explicitly *not* designed now (would be pure addition): `Enqueuer`, the
optimization passes themselves, the minifier/`Namer`. None of them require
changing `skotch-dex`, `skotch-classfile`, or the core IR/regalloc/dexer.

---

## 8. Test plan

DEX output is not byte‑canonical, so the oracle is **structural + behavioral
equivalence to real `d8`**, plus self‑consistency. Tiers run at different
cadences; goldens are committed so day‑to‑day CI needs neither `d8` nor an
emulator (matching Skotch's "external tools only at `xtask` fixture‑gen time"
policy).

### Tier 0 — unit (per crate, inner loop, milliseconds)
- `skotch-dex`: LEB128 (s/u) enc/dec, MUTF‑8, **every opcode format** encode↔
  decode, checksum/signature, write→read round‑trip, map ordering, pool
  sort/dedup.
- `skotch-classfile`: parse the JDK's own `rt.jar` / `android.jar` classes,
  round‑trip the constant pool, decode every JVM opcode, `StackMapTable` parse,
  `invokedynamic`/`BootstrapMethods`.
- `skotch-dexcore`: IRBuilder IR snapshots on small methods; **regalloc on
  crafted hard cases** (loops, overlapping live ranges, wide values, invoke
  register pressure forcing spills/`*-range`); DexBuilder instruction‑selection
  matrices (const/if/invoke/binop width); per‑desugaring synthesized‑output
  snapshots.

### Tier 1 — structural parity vs real d8 (primary oracle, offline, fast)
- **Corpus**: (a) hand‑written micro‑cases per feature — arithmetic, control
  flow, fields, all invoke kinds, arrays, `try/catch/finally`, lambdas,
  default/static interface methods, string templates, enums, `when`, generics,
  inner/synthetic classes, `companion`; (b) **r8's own `src/test/examples*`
  Java sources** (lambdadesugaring, invokecustom, interfacemethods, …);
  (c) real jars: Kotlin stdlib + a couple of AndroidX artifacts.
- `xtask gen-fixtures --target d8` (fixture‑gen time only) compiles inputs with
  `javac`/`kotlinc` and runs **real `d8`** → reference `.dex`, committed.
- `skotch d8` produces its `.dex`.
- A **DEX normalizer** (extend `skotch-dex-norm` to full instruction
  disassembly) compares structurally with documented tolerances:
  - **Must match**: set of (possibly desugared) classes, methods, fields; per
    method the instruction *sequence by semantics* and control‑flow structure;
    constant pool *contents* (as a set).
  - **Allowed to differ**: register numbers (allocation choice), synthetic‑class
    *names* (compared up to a canonical renaming), pool *ordering/indices*,
    section layout.
- Committed `*.norm.txt` goldens give a stable regression check even when `d8`
  isn't present.

### Tier 2 — DEX validity (offline, fast)
- Re‑parse our own output with the `skotch-dex` reader; assert internal
  consistency. Optionally run `dexdump`/`baksmali`/`d8`‑as‑validator at
  fixture‑gen time to catch format errors.

### Tier 3 — behavioral / ART execution (gold standard, slow, emulator CI lane)
- For each micro‑case and r8 example: source → `.class` → `skotch d8` → `.dex`
  → run on ART (emulator or host `art`), assert stdout == the same program run
  on the JVM (or run on real‑d8's `.dex`). This catches desugaring/regalloc bugs
  that structural diff cannot. Reuses r8's executable examples — this is the
  "use their exercises" path. Gated behind emulator availability, like the
  existing JetChat lane.

### Tier 4 — differential / property / fuzz
- Random/mutated small valid classfiles → `skotch d8` → validator must pass; if
  `d8` available, structural‑diff. Property: output always re‑parses and
  validates. `.dex` read→write→read stable.

### Tier 5 — conformance harness
- A reusable harness over r8's `examplesAndroidO`/`AndroidN`/`AndroidP` inputs
  that runs both compilers and asserts structural (Tier 1) + behavioral (Tier 3)
  equivalence. Drives the desugaring‑breadth coverage.

**CI lanes**: Tiers 0–2 + 4 in normal CI (offline, committed goldens,
self‑contained). Tier 3 + 5‑behavioral in the emulator lane. `xtask`
regenerates Tier‑1/2 reference `.dex` and `.norm.txt` when `d8`/`javac`/
`kotlinc` are present.

---

## 9. Phasing, parallelization, and milestones

Phases are sequenced by dependency; within a phase, the listed tracks are
concurrent (separate engineers/agents behind agreed crate interfaces).

**Phase 0 — format leaves (fully parallel).**
Track A `skotch-dex` (model + writer + reader + validator + opcode tables).
Track B `skotch-classfile` (full reader).
Milestone: each crate round‑trips real artifacts; Tier‑0 green. *No dependency
between A and B.*

**Phase 1 — core dexer, no desugaring (critical path).**
Behind a fixed IR interface, three concurrent tracks:
C `ir_builder` (CF→SSA IR), D `regalloc` (linear scan + DEX constraints),
E `dexbuilder` (IR→DEX) + integrate `skotch-dex` writer; plus the `skotch-d8`
driver MVP (read → dex single classes → write).
Milestone: dex a lambda‑free `javac`/`kotlinc` class; runs on ART; Tier‑1
structural‑diff vs real d8 passes for non‑desugared code. **This is the hardest
phase** (SSA + register allocation under DEX constraints).

**Phase 2 — desugaring (parallel behind the framework).**
F lambda, G interface methods, H string concat, I backported methods — each an
independent `Desugaring` impl. Milestone: real Kotlin lambdas + interface
defaults + string templates dex and run on ART; Tier‑5 lambda/interface
examples pass.

**Phase 3 — output modes, multidex, merging.**
Multidex split + 64K accounting, `.dex` inputs + merge, `--file-per-class[-file]`,
`--intermediate`/`--globals`, `--main-dex-list`, dir/zip output. Milestone: dex
a real app's full dependency set (Kotlin stdlib + AndroidX) into multidex; APK
installs & launches.

**Phase 4 — CLI completeness + hardening.**
`@argfile`, `--lib/--classpath` resolution incl. `android.jar`, debug‑info
emission for `--debug`, assertions handling, diagnostics mapping, `--pg-map`
pass‑through, error parity, performance. Milestone: drop‑in for the pipeline's
current `d8` invocation; emulator lane green across the corpus.

**Phase 5 — pipeline switch‑over (separate, later).**
Replace `compile_app_classes_with_d8`'s shell‑out with `skotch_d8` for *all*
classes; have `skotch-backend-jvm` (already byte‑identical to kotlinc, with a
`d8‑safe` mode) feed first‑party classfiles to the same dexer; retire
`skotch-backend-dex` (MIR→DEX). **Not in scope now** — the current MIR→DEX path
stays untouched until `skotch d8` is proven.

Relative effort (engineering‑weeks, one strong compiler engineer; ranges, not
promises): P0 ≈ 3–5, **P1 ≈ 6–10 (critical)**, P2 ≈ 4–7, P3 ≈ 3–5, P4 ≈ 3–5.
This is by far the largest of the SDK‑tool reimplementations (aapt2/apksig are
mechanical; this is a real compiler backend).

---

## 10. Risks and mitigations

- **Register allocation under DEX constraints** (biggest risk): non‑range invoke
  ⇒ args in v0–v15; `*-range` ⇒ contiguous; wide ⇒ pairs; `move-result`
  adjacency. *Mitigation*: port r8's `LinearScanRegisterAllocator` faithfully;
  Tier‑0 crafted spill/pressure cases; Tier‑3 ART execution as ground truth.
- **Desugaring correctness** (runtime crashes, hard to see offline).
  *Mitigation*: Tier‑3 behavioral tests from r8's examples per desugaring;
  structural snapshots vs d8's synthesized classes.
- **Long tail of real‑world classfiles** (old `jsr/ret`, odd stackmaps, exotic
  attributes). *Mitigation*: corpus from real jars; fuzz tier; fail‑loud with
  clear diagnostics rather than miscompile.
- **No byte‑parity oracle** — slower than apksig's offline diff. *Mitigation*:
  structural normalizer + committed goldens for the fast loop; ART for
  correctness; accept that "matches d8 byte‑for‑byte" is a non‑goal.
- **Scope creep into R8.** *Mitigation*: hard line — D8 + 3 desugarings +
  multidex this round; R8 is seams only (§7).

---

## 11. Immediate next actions (when execution begins)

1. Scaffold `skotch-dex` and `skotch-classfile` (Phase 0, parallel) with Tier‑0
   tests; round‑trip real `.dex`/`.class`.
2. Add `xtask gen-fixtures --target d8` and the extended `skotch-dex-norm`
   disassembler to stand up the Tier‑1 oracle early.
3. Land the `skotch-dexcore` IR + regalloc + DexBuilder skeleton behind the
   `AppView<I>` seam (Phase 1).
4. Wire `Command::D8` + `("d8","d8")` multicall once the driver can dex one
   class.
