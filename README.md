# skotch

A Rust toolchain that replaces the Kotlin compiler, Gradle, and Android SDK
build tools with a single CLI. Compiles Kotlin 2 sources to five targets:

| Target | Output | Pipeline |
|---|---|---|
| JVM | `.class` (Java 17, v61) | MIR → JVM bytecode |
| DEX | `.dex` (Dalvik v035) | MIR → DEX bytecode |
| klib | `.klib` (zip with serialized IR) | MIR → JSON IR → zip |
| LLVM IR | `.ll` (textual LLVM 19+) | MIR → klib → LLVM IR |
| Native | host executable | MIR → klib → LLVM IR → clang |

The shipping binary has no dependency on `kotlinc`, `javac`, `d8`, `gradle`,
`aapt2`, or `apksigner`. `clang` is the only external tool invoked, for the
native link step.

## Installation

```sh
# Homebrew (macOS/Linux)
brew install skotlang/tap/skotch

# Shell installer (Linux/macOS)
curl -fsSL https://github.com/skotlang/skotch/releases/latest/download/skotch-cli-installer.sh | sh

# PowerShell (Windows)
powershell -ExecutionPolicy Bypass -c "irm https://github.com/skotlang/skotch/releases/latest/download/skotch-cli-installer.ps1 | iex"

# From source
git clone https://github.com/skotlang/skotch.git && cd skotch && cargo build --release
```

## Quick start

```sh
cat > hello.kt <<'EOF'
fun main() {
    println("Hello, world!")
}
EOF

skotch emit --target jvm hello.kt -o HelloKt.class
java -cp . HelloKt                           # → Hello, world!

skotch emit --target dex hello.kt -o classes.dex
skotch emit --target klib hello.kt -o hello.klib
skotch emit --target llvm hello.kt -o hello.ll
skotch emit --target native hello.kt -o hello && ./hello
```

## CLI

| Command | Purpose |
|---|---|
| `skotch emit --target T <input.kt> -o <out>` | Compile one Kotlin file to target T. |
| `skotch build [-C dir] [--target T]` | Read `build.gradle.kts`, compile, package (JAR/APK). |
| `skotch run <script.kts>` | Execute a KotlinScript file. |
| `skotch repl [--exec CODE] [--file F]` | Interactive REPL (in-process JVM). |
| `skotch lsp` | Language Server Protocol on stdio. |
| `skotch test` | (planned) Discover and run `@Test` annotations. |

## Architecture

Front-end: lex → parse → resolve → typecheck → MIR lower. All backends read
the same `MirModule` IR. The `salsa` layer memoizes per-file compilation;
`rayon` parallelizes Phase 2 (per file) and Phase 3 (per module).

**Two-phase build:**
- **Gather** (sequential): lex + parse all files, build `PackageSymbolTable`.
- **Compile** (parallel): resolve, typecheck, lower per file with cross-file
  symbols visible. Each file produces its own `MirModule` (e.g. `GreeterKt`).
  Cross-file calls emit `invokestatic OtherFileKt.method(desc)`.

**Incremental:** salsa hashes source files via blake3. Body-only edits leave
`FileExports` unchanged, so only the edited file is recompiled; signature
edits invalidate every dependent. Validated by
`salsa_incremental_body_change_skips_dependents`.

**Crate layers** (strict DAG; no crate imports from a higher layer):

```
0  span, intern, config, diagnostics
1  syntax, lexer, parser
2  resolve, types, typeck
3  hir, mir, mir-lower
4  backend-{jvm,dex,llvm,wasm,klib,native}
5  classfile-norm, dex-norm, llvm-norm
6  jar, axml, apk, sign, classinfo
7  db (salsa)
8  build, driver, cli, repl, lsp
```

xtask is the only crate allowed to invoke external compilers — used solely
for generating fixture goldens.

## Fixture-driven validation

Each fixture under `tests/fixtures/inputs/<name>/input.kt` is compiled by
skotch AND by the reference tool (`kotlinc`, `d8`, `kotlinc-native`). Both
outputs and the expected stdout are committed under
`tests/fixtures/expected/<target>/<name>/`. Normalizers strip cosmetic
differences (CP ordering, debug attrs, kotlin metadata, target triples) so
two compilers can be diffed without false positives.

## Kotlin language support

Coverage is broad: 397/398 supported e2e fixtures compile and run with the
expected stdout. The compiler handles classes (regular, data, enum, sealed,
inner, nested, object, companion, abstract), interfaces with default
methods, generics with variance and reified params, lambdas with closure
capture, scope functions, smart casts, sealed-when exhaustiveness, suspend
functions, and a substantial portion of the Kotlin stdlib.

### Implemented

| Feature | Notes |
|---|---|
| Top-level `fun` | Default params, named args, expression body, varargs, infix |
| Extension functions/properties | `fun Int.isEven()`, method chaining |
| Local functions | Including recursive |
| Classes | Primary + secondary constructors, `init` blocks, `var`/`val` fields, visibility modifiers (parsed; not enforced), `open`/`abstract` |
| Data classes | Auto `toString`/`equals`/`hashCode`/`copy`/`componentN` |
| Enum classes | Entries, `.name`, constructor params, `values()`, `valueOf(String)`, when matching, abstract methods per entry |
| Sealed classes/interfaces | Exhaustive when + `is` patterns, smart-cast narrowing |
| Object declarations | `object X { fun y() }` as static methods |
| Companion objects | `ClassName.method()` dispatch |
| Inner/nested classes | `inner class` with `this$0`, `class Outer { class Nested }` |
| `@JvmField` / `@JvmStatic` | Field-only emission; companion static delegate |
| Inheritance | `open class`, `override fun`, `super` calls, 3-level chains, inherited fields |
| Interfaces | Abstract + default methods, `invokeinterface`, multi-implementation |
| Property getters/setters | Custom `get()`/`set(v) { field = v }` with backing-field keyword |
| Property delegation | `val x by lazy { … }` (eager-init in ctor); `val x by viewModels()` |
| Interface delegation | `class X : Base by b` auto-forwards |
| Operator overloading | `operator fun plus/invoke/compareTo/get/set/contains` |
| Variables | `val`, `var`, `lateinit var`, `const val` |
| Numeric types | Int, Long, Double, Float, Byte, Short, Char — full arithmetic, conversions |
| Literals | Decimal/hex/binary/underscored integers; `L`/`f` suffixes; scientific notation |
| String | Templates `"$x"` / `"${expr}"`, raw `"""…"""`, 20+ methods (length, uppercase, substring, contains, …) |
| Nullable types | `T?`, elvis `?:`, safe call `?.`, non-null `!!`, smart-cast narrowing |
| Generics | Functions/classes, upper bounds, `in`/`out` variance, star projection, type erasure, reified `T` in inline fns |
| Type aliases | `typealias Name = Type` |
| Control flow | `if`/`else`, `when` (with/without subject, ranges, comma patterns), `for` over ranges and collections, `while`, `do-while`, `break`/`continue`, labeled returns |
| Exceptions | `try`/`catch`/`finally`, `throw`, full JVM exception tables, `e.message`, `try` as expression |
| Lambdas | Closure capture (val + var with Ref boxing), trailing lambda, `it`, nested lambdas, LambdaMetafactory dispatch |
| Function types | `(Int) -> String`, receiver types, erased to Object on JVM |
| Higher-order functions | Synthetic `$FunctionN` interfaces, autoboxing |
| Function references | `::name` and `Class::method` desugar to lambdas |
| SAM conversions | `Interface { lambda }` → anonymous object |
| Scope functions | `let`, `also`, `run`, `apply`, `with`, `repeat` as MIR intrinsics |
| Destructuring | `val (a, b) = pair`, `for ((k, v) in map)` |
| Collections | `listOf`, `mutableListOf`, `mapOf`, `setOf`, real `kotlin.Pair`, `to` infix; `.map`/`.filter`/`.fold`/`.forEach`/… via real stdlib |
| Ranges | `..`, `until`, `downTo`, `step`, `in`/`!in` |
| Coroutines (suspend) | `suspend fun`, `runBlocking`, `launch`, `async`/`await`, `delay`, `withContext`/`withTimeout`, structured concurrency. CPS state-machine extraction; ~58% byte-parity vs kotlinc |
| Compose `@Composable` | `$composer`/`$changed` injection, restart-scope lambdas, skip-optimization, `remember { }` return-type propagation, state holders |
| Annotations | `@Suppress`, `@Deprecated`, `@field:JvmField`, `@JvmStatic`, `@Composable` |
| Java interop | Real `.class` parsing from JDK jmods + CLASSPATH; deferred resolution; clear errors |
| kotlin-stdlib | `kotlin-stdlib.jar` on JVM classpath; stdlib calls dispatch to real implementations |
| Packages | `package com.example` → `com/example/InputKt.class` |
| `kotlin.math` | `abs`, `sqrt`, `ceil`, `floor`, `round`, `pow`, `sin`, `cos`, `tan`, `log`, `exp` → `java.lang.Math` |
| LSP | Real-time diagnostics, semantic tokens, hover, go-to-definition, completions |

### Partial / known gaps

| Feature | Gap |
|---|---|
| Coroutines parity | 393/673 fixtures byte-identical with kotlinc; runtime behavior correct |
| Compose group keys | Hashes differ from kotlinc's compose plugin; runtime works |
| Generic upper-bound erasure | Missing call-site coercion for `Box<Int>.get() → int` unbox |
| JetChat (Compose Android app) | Launches and reaches NavActivity with no crashes; `MaterialTheme.colorScheme` dispatch picks wrong stub arity → UI renders blank |
| `tailrec` | Parsed; optimization deferred |
| Bit-shift infix | `shl`/`shr`/`and`/`or`/`xor` |

## Running the tests

```sh
cargo test --workspace                      # all tests
cargo clippy --workspace -- -D warnings     # lint
cargo fmt --all -- --check                  # format
```

Every fixture with `status = "supported"` in its `meta.toml` and a
`run.stdout` is compiled and run end-to-end. There is no skip list — adding
"supported" means the fixture must pass.

## Regenerating fixture goldens

`xtask` is the only crate that invokes external compilers.

```sh
cargo xtask gen-fixtures --target jvm        # kotlinc + java
cargo xtask gen-fixtures --target dex        # kotlinc + d8
cargo xtask gen-fixtures --target klib       # kotlinc-native
cargo xtask gen-fixtures --target llvm       # kotlinc-native + clang
cargo xtask gen-fixtures --target native     # kotlinc-native + clang
cargo xtask gen-fixtures --target jvm --skotch-only   # skip refs
```

xtask auto-locates `d8` under `$ANDROID_HOME/build-tools/<latest>/` (or
`$ANDROID_SDK_ROOT`), falls back to `PATH`. `kotlin-stdlib.jar` is located
next to `kotlinc`. Missing tools log a warning and skip their slice rather
than failing.
