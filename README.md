# skotch

A Rust toolchain that replaces the Kotlin compiler, Gradle, and Android SDK
build tools with a single CLI. Compiles Kotlin 2 sources to five target
formats:

| Target | Output | Pipeline |
|---|---|---|
| JVM | `.class` (Java 17, class file v61) | MIR → JVM bytecode |
| DEX | `.dex` (Dalvik v035) | MIR → DEX bytecode |
| klib | `.klib` (zip with serialized IR) | MIR → JSON IR → zip |
| LLVM IR | `.ll` (textual LLVM 19+) | MIR → klib → LLVM IR |
| Native | host executable | MIR → klib → LLVM IR → clang |

The shipping binary has no dependency on `kotlinc`, `kotlinc-native`, `javac`,
`d8`, `dx`, `gradle`, `aapt2`, or `apksigner`. The only external tool it
invokes is `clang`, for the native target's link step.

## Project goals

1. **One binary, fast builds.** Skip the JVM warm-up, Gradle daemon, and
   multi-GB SDK downloads. `skotch emit hello.kt -o hello.class` should
   feel instant.
2. **Multi-target from a single front-end.** One lex/parse/typeck/MIR
   pipeline; pluggable backends for JVM, DEX, native, wasm. Adding a new
   target means writing one backend crate.
3. **Validate against real toolchains.** Every supported fixture is built
   by skotch *and* by the corresponding reference tool (`kotlinc`, `d8`,
   `kotlinc-native`); outputs are committed to git so CI never needs the
   JDK or Android SDK.
4. **Parallel by default.** Modules × files × functions via Rayon's nested
   work-stealing.
5. **Modular workspace.** ~25 small crates with a strict dependency DAG;
   no crate knows about anything in a higher layer.

> **Status:** JVM, DEX, klib, LLVM IR, and native targets are shipping.
> Build orchestration, REPL, JAR packaging, and unsigned APK assembly are
> implemented. 107 language-feature fixtures validated (~15–20% of the Kotlin spec).

## Installation

### Homebrew (macOS / Linux)

```sh
brew install skotlang/tap/skotch
```

### Shell (Linux / macOS)

```sh
curl -fsSL https://github.com/skotlang/skotch/releases/latest/download/skotch-cli-installer.sh | sh
```

### PowerShell (Windows)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/skotlang/skotch/releases/latest/download/skotch-cli-installer.ps1 | iex"
```

### Pre-built binaries

Binaries for Linux, macOS, and Windows are published with each GitHub
release. Download the latest from the project's
[Releases](https://github.com/skotlang/skot/releases) page.

### From source

```sh
git clone https://github.com/skotlang/skot.git
cd skot
cargo build --release
# The binary lives at target/release/skotch
```

## Quick start: hello world on every target

```sh
# Write a Kotlin source file.
cat > hello.kt <<'EOF'
fun main() {
    println("Hello, world!")
}
EOF

# JVM:
skotch emit --target jvm hello.kt -o HelloKt.class
java -cp . HelloKt                     # → Hello, world!

# DEX (drop into an APK; verify with Android dexdump):
skotch emit --target dex hello.kt -o classes.dex
dexdump -d classes.dex                 # disassembles cleanly

# klib (the multi-stage pipeline waist):
skotch emit --target klib hello.kt -o hello.klib
unzip -l hello.klib                    # default/manifest, default/ir/, ...

# LLVM IR (consumes the klib internally):
skotch emit --target llvm hello.kt -o hello.ll
cat hello.ll                           # 13 lines of textual LLVM IR

# Native binary (clang link step):
skotch emit --target native hello.kt -o hello
./hello                                # → Hello, world!
```

## CLI subcommands

| Command | What it does | Status |
|---|---|---|
| `skotch emit --target T <input.kt> -o <out>` | Compile a single Kotlin file directly to target T. | shipping |
| `skotch build [-C dir] [--target T]` | Discover `build.gradle.kts`, compile and package (JAR/APK). | shipping |
| `skotch repl [--exec CODE] [--file F]` | Interactive REPL backed by in-process JVM. | shipping |
| `skotch run <script.kts>` | Execute a KotlinScript file. | shipping |
| `skotch test` | Discover `@Test` annotations, run the tests. | planned |

`skotch emit` is the testing surface: it bypasses build orchestration so
the lexer/parser/typeck/MIR/backend pipeline can be exercised directly
on a single source file. Build orchestration follows once the format
emitters are stable.

## Architectural rules

1. The shipping binary never invokes `kotlinc`, `kotlinc-native`, `javac`,
   `d8`, or `dx`. Reference outputs are produced by `cargo xtask gen-fixtures`
   (which *does* shell out to those tools) and committed to git, so CI needs
   no JDK or Android SDK. A `tests/no_external_compiler.rs` test enforces
   this by grepping the release binary for forbidden tool names.
2. Parsing and format emission first. Packaging (JAR/APK), signing, and the
   build orchestrator come after the front-end and emitters are validated by
   golden fixtures.
3. Hand-rolled bytecode writers. Constant-pool forward references make
   `binrw`/`scroll` awkward; `byteorder` is the workhorse for `.class`
   and `.dex`.
4. Textual LLVM IR. No `inkwell`/`llvm-sys` dependency — avoids the
   `libLLVM` system requirement and the long build-time hit.
5. `clang` is the only external tool the binary invokes. Native linking
   goes through `clang`; everything else is in-process Rust.

## Fixture-driven validation

Tests under `tests/fixtures/inputs/<name>/input.kt` are compiled by skotch
*and* by the corresponding reference tool. Both outputs are committed to
`tests/fixtures/expected/<target>/<name>/`:

```
tests/fixtures/expected/
    jvm/<f>/
        skotch.class               # skotch's bytes
        skotch.norm.txt            # normalized text
        kotlinc.class            # reference from kotlinc
        kotlinc.norm.txt
        run.stdout               # expected program output
    dex/<f>/
        skotch.dex
        skotch.norm.txt
        d8.dex                   # reference from kotlinc → d8
        d8.norm.txt
    klib/<f>/
        skotch.klib
        skotch.norm.txt
        kotlinc-native.klib      # reference from kotlinc-native
    llvm/<f>/
        skotch.ll
        skotch.norm.txt
        kotlinc-native.summary.txt   # tiny extract from kotlinc-native's IR
    native/<f>/
        run.stdout               # skotch binary's stdout
        kotlinc-native.run.stdout    # cross-compiler agreement check
```

The "normalized" text forms (produced by `skotch-classfile-norm`,
`skotch-dex-norm`, `skotch-llvm-norm`) strip cosmetic differences (constant
pool ordering, debug attributes, kotlin metadata, target triples) so
two compilers can be diffed without false positives. Byte-exact "self
golden" tests still catch regressions in skotch's own emitter.

## Kotlin language support

**Estimated coverage: ~15–20% of the Kotlin language specification.** The compiler
handles the core procedural subset — functions, control flow, basic types, and
expressions — but does not yet support classes, generics, lambdas, nullable
types, or the standard library collection APIs. 107 test fixtures validated across
JVM, DEX, LLVM IR, and klib targets.

### Implemented and stable

| Feature | Spec reference | Notes |
|---|---|---|
| [Function declarations](https://kotlinlang.org/spec/declarations.html#function-declaration) | §4.1 | Top-level `fun`, parameters, return types |
| [Expression body functions](https://kotlinlang.org/spec/declarations.html#function-declaration) | §4.1 | `fun f() = expr` shorthand |
| [Extension functions](https://kotlinlang.org/spec/declarations.html#extension-function-declaration) | §4.1.3 | `fun Int.isEven()`, `this` receiver, method chaining |
| [Local functions](https://kotlinlang.org/spec/declarations.html#local-function-declaration) | §4.1.4 | `fun` inside blocks, recursive calls |
| [Variable declarations](https://kotlinlang.org/spec/declarations.html#property-declaration) | §4.2 | `val` (immutable), `var` (mutable), type annotations |
| [Integer literals](https://kotlinlang.org/spec/expressions.html#integer-literals) | §7.1.1 | Decimal, hex (`0xFF`), binary (`0b1010`), underscores (`1_000`), `L` suffix |
| [Character literals](https://kotlinlang.org/spec/expressions.html#character-literals) | §7.1.5 | `'A'`, escape sequences (`'\n'`, `'\t'`, `'\\'`) |
| [Boolean literals](https://kotlinlang.org/spec/expressions.html#boolean-literals) | §7.1.3 | `true`, `false` |
| [String literals](https://kotlinlang.org/spec/expressions.html#string-interpolation-expressions) | §7.1.4 | Regular, raw (`"""`), templates (`$x`, `${expr}`) |
| [Arithmetic operators](https://kotlinlang.org/spec/expressions.html#arithmetic-expressions) | §7.5 | `+`, `-`, `*`, `/`, `%` on `Int` |
| [String concatenation](https://kotlinlang.org/spec/expressions.html#arithmetic-expressions) | §7.5 | `String + String`, `String + Int` |
| [Comparison operators](https://kotlinlang.org/spec/expressions.html#comparison-expressions) | §7.6 | `==`, `!=`, `<`, `>`, `<=`, `>=` (Int and String) |
| [Logical operators](https://kotlinlang.org/spec/expressions.html#logical-disjunction-expression) | §7.8–7.9 | `&&`, `\|\|` with short-circuit evaluation |
| [Unary operators](https://kotlinlang.org/spec/expressions.html#unary-expressions) | §7.3 | `-` (negation), `!` (not) |
| [Compound assignment](https://kotlinlang.org/spec/expressions.html#assignments) | §7.12 | `+=`, `-=`, `*=`, `/=`, `%=` |
| [If expression](https://kotlinlang.org/spec/expressions.html#conditional-expressions) | §7.4.1 | As statement and expression, with/without else |
| [When expression](https://kotlinlang.org/spec/expressions.html#when-expressions) | §7.4.2 | With subject, without subject, comma patterns, `in range`, string/int matching, nested |
| [Else-if chains](https://kotlinlang.org/spec/expressions.html#conditional-expressions) | §7.4.1 | `if {} else if {} else {}` (as statements) |
| [For loop](https://kotlinlang.org/spec/statements.html#for-loop-statements) | §8.2 | `for (i in start..end) { }` with `Int` ranges |
| [While loop](https://kotlinlang.org/spec/statements.html#while-loop-statements) | §8.3 | `while (cond) { }` |
| [Do-while loop](https://kotlinlang.org/spec/statements.html#do-while-loop-statements) | §8.3 | `do { } while (cond)` |
| [Break and continue](https://kotlinlang.org/spec/expressions.html#break-and-continue-expressions) | §7.10 | In `for`, `while`, and `do-while` loops (including nested in `if`) |
| [Return](https://kotlinlang.org/spec/expressions.html#return-expressions) | §7.10 | Early return from functions (guard clauses) |
| [Function calls](https://kotlinlang.org/spec/expressions.html#function-calls-and-property-access) | §7.2 | Direct, nested, recursive, mutual recursion, extension method syntax |
| [`println`](https://kotlinlang.org/api/core/kotlin-stdlib/kotlin.io/println.html) | stdlib | `println()`, `println(Int)`, `println(String)`, `println(Boolean)` |
| [String templates in expressions](https://kotlinlang.org/spec/expressions.html#string-interpolation-expressions) | §7.1.4 | `"$var"`, `"${expr}"` usable anywhere (val, return, args) |

### Not yet implemented

| Feature | Spec reference | Difficulty | Notes |
|---|---|---|---|
| Classes and objects | [§4.5](https://kotlinlang.org/spec/declarations.html#class-declaration) | Hard | Constructors, properties, methods, inheritance |
| Data classes | [§4.5.6](https://kotlinlang.org/spec/declarations.html#data-class-declaration) | Hard | Synthesized equals/hashCode/toString/copy |
| Interfaces | [§4.5.3](https://kotlinlang.org/spec/declarations.html#interface-declaration) | Hard | Declaration, implementation, default methods |
| Enums | [§4.5.7](https://kotlinlang.org/spec/declarations.html#enum-class-declaration) | Medium | Enum constants, entries, values() |
| Sealed classes | [§4.5.5](https://kotlinlang.org/spec/declarations.html#sealed-class-declaration) | Hard | Sealed hierarchies, exhaustive when |
| Generics | [§4.6](https://kotlinlang.org/spec/declarations.html#type-parameters) | Hard | Type parameters, bounds, variance |
| Lambdas | [§7.2.10](https://kotlinlang.org/spec/expressions.html#lambda-literals) | Hard | Lambda literals, closures, `it` parameter |
| Nullable types | [§3.3](https://kotlinlang.org/spec/type-system.html#nullable-types) | Medium | `T?`, safe call `?.`, elvis `?:`, smart casts |
| Collections | stdlib | Hard | `listOf`, `map`, `filter`, `fold` (needs generics + lambdas) |
| Default arguments | [§4.1.1](https://kotlinlang.org/spec/declarations.html#function-declaration) | Medium | Synthetic `$default` overload |
| Named arguments | [§7.2.2](https://kotlinlang.org/spec/expressions.html#named-and-default-arguments) | Medium | Call-site argument reordering |
| Varargs | [§4.1.2](https://kotlinlang.org/spec/declarations.html#function-declaration) | Medium | `vararg` parameter, spread `*` |
| Type aliases | [§4.7](https://kotlinlang.org/spec/declarations.html#type-alias) | Easy | `typealias` erased at compile time |
| Destructuring | [§8.1](https://kotlinlang.org/spec/statements.html#destructuring-declarations) | Medium | `val (a, b) = pair` via `componentN()` |
| Try/catch/finally | [§7.4.5](https://kotlinlang.org/spec/expressions.html#try-expression) | Medium | Exception handling, exception tables |
| Coroutines | [§7.2.11](https://kotlinlang.org/spec/expressions.html#coroutine-builder-invocations) | Very Hard | `suspend`, state machine CPS transform |
| Annotations | [§4.8](https://kotlinlang.org/spec/declarations.html#annotation-declaration) | Medium | Declaration, retention, reflection |
| Operator overloading | [§7.5](https://kotlinlang.org/spec/expressions.html#overloadable-operators) | Medium | `plus`, `minus`, `compareTo`, `invoke` |
| `else if` chains with return | — | Medium | All-branches-return in nested if (use `when` as workaround) |
| Float/Double literals | [§7.1.2](https://kotlinlang.org/spec/expressions.html#real-literals) | Easy | `3.14`, `2.5e10` |

## Running the tests

```sh
# All unit + integration tests; needs no JDK or Android SDK.
cargo test --workspace

# Lint check (treat warnings as errors).
cargo clippy --workspace -- -D warnings

# Verify skotch's output against committed goldens for one target.
cargo xtask verify --target jvm
cargo xtask verify --target dex
cargo xtask verify --target klib
cargo xtask verify --target llvm
```

## Regenerating fixture goldens

Reference outputs are produced by the **xtask** binary, which is the
*only* place in the workspace allowed to invoke `kotlinc`, `d8`,
`kotlinc-native`, etc.

```sh
# JVM goldens (needs kotlinc + java):
cargo xtask gen-fixtures --target jvm

# DEX goldens (needs kotlinc + d8 from Android SDK build-tools):
cargo xtask gen-fixtures --target dex

# klib goldens (needs kotlinc-native):
cargo xtask gen-fixtures --target klib

# LLVM IR goldens (needs kotlinc-native + clang):
cargo xtask gen-fixtures --target llvm

# Native binaries + run.stdout (needs kotlinc-native + clang):
cargo xtask gen-fixtures --target native

# Skip reference tools, regenerate just skotch's own goldens:
cargo xtask gen-fixtures --target jvm --skotch-only
```

xtask auto-locates `d8` under `$ANDROID_HOME/build-tools/<latest>/` (or
`$ANDROID_SDK_ROOT/build-tools/<latest>/`, the older variable name still
recognized by Android Studio and many CI runners), then falls back to
`d8` on `PATH`. The DEX e2e test uses the same lookup for `dexdump`.
`kotlin-stdlib.jar` is auto-located next to the `kotlinc` binary.
Missing tools log a warning and skip their slice of the reference
outputs rather than failing the run.

## Workspace layout

The crates form a strict DAG — every crate has 1–6 internal dependencies
and lower layers know nothing about higher ones.

```
Layer 0 — primitives:    span, intern, config, diagnostics
Layer 1 — front-end:     syntax, lexer, parser
Layer 2 — semantic:      resolve, types, typeck
Layer 3 — IRs:           hir, mir, mir-lower
Layer 4 — backends:      backend-jvm, backend-dex, backend-llvm,
                         backend-klib, backend-wasm
Layer 5 — normalizers:   classfile-norm, dex-norm, llvm-norm
Layer 6 — orchestration: driver
Layer 7 — CLI:           cli (binary `skotch`)

xtask                    fixture-generation helper (only crate
                         allowed to invoke external compilers)
```

## Supported Kotlin syntax (current)

- Top-level `fun` declarations with parameters and return types
- Local `val` and `var` declarations with type inference for literals
- String literals (with escape sequences)
- Integer literals (positive and negative, all `bipush`/`sipush`/`ldc` forms)
- Boolean literals
- Integer arithmetic: `+ - * / %` with operator precedence
- `println(string)`, `println(int)` — built-in intrinsic
- Top-level function-to-function calls (`invokestatic` / `invoke-static`)
- Multi-statement function bodies
- Line comments (`//`) and block comments (`/* */`)

Stub fixtures for upcoming features (classes, data classes, sealed,
generics, when, lambdas, coroutines, extension functions, ...) live
under `tests/fixtures/inputs/2X-*/` with `status = "stub"` in their
`meta.toml` so they can graduate to "supported" as the corresponding
backend support lands.

## License

Apache-2.0
