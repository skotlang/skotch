# skotch

A from-scratch Rust toolchain that replaces the Kotlin compiler, Gradle, and
Android SDK build tools with a single fast, parallel CLI. Compiles **Kotlin 2**
sources to four target formats:

| Target | Output | Pipeline |
|---|---|---|
| **JVM** | `.class` (Java 17, class file v61) | MIR → JVM bytecode |
| **DEX** | `.dex` (Dalvik v035) | MIR → DEX bytecode |
| **klib** | `.klib` (zip with serialized IR) | MIR → JSON IR → zip |
| **LLVM IR** | `.ll` (textual LLVM 19+) | MIR → klib → LLVM IR |
| **Native** | host executable | MIR → klib → LLVM IR → clang |

…with **no dependency** on `kotlinc`, `kotlinc-native`, `javac`, `d8`, `dx`,
`gradle`, `aapt2`, or `apksigner`. The shipping `skotch` binary depends only
on `clang` for the native target's link step (and that's the only external
toolchain it ever invokes).

## Project goals

1. **Replace the entire Kotlin build toolchain with one fast Rust binary.**
   No JVM warm-up tax for `kotlinc`. No Gradle daemon. No 1+ GB Android SDK.
   `skotch emit hello.kt -o hello.class` should be instant.
2. **Multi-target from a single front-end.** One lex/parse/typeck/MIR
   pipeline; pluggable backends for JVM, DEX, native, wasm. Adding a new
   target means writing one backend crate.
3. **Validate every output against the real toolchains.** Every supported
   fixture is built by skotch *and* by the corresponding reference tool
   (`kotlinc`, `d8`, `kotlinc-native`); their outputs are committed to git
   so CI never needs the JDK or Android SDK.
4. **Strict parallelism.** Modules in parallel × files in parallel ×
   functions in parallel via Rayon's nested work-stealing.
5. **Modular workspace.** ~25 small crates with a strict dependency DAG;
   no crate knows about anything in a higher layer.

> **Status:** PR #4 — JVM, DEX, klib, LLVM IR, and native targets all green.
> Build orchestration (`skotch build`), test runner (`skotch test`), REPL
> (`skotch repl`), and Android APK packaging follow in subsequent PRs.

## Installation

From source:

```sh
git clone https://github.com/<user>/skotlang.git
cd skotlang
cargo build --release
# The binary lives at target/release/skotch
```

Pre-built binaries for Linux, macOS, and Windows are published with each
GitHub release (see `.github/workflows/release.yml`). Download the latest
from the project's Releases page.

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
| `skotch emit --target T <input.kt> -o <out>` | Compile a single Kotlin file directly to target T. | **shipping** |
| `skotch build` | Discover `build.gradle.kts`, build the project. | stub (PR #5) |
| `skotch test` | Discover `@Test` annotations, run the tests. | stub (PR #6) |
| `skotch repl` | Interactive REPL backed by the JVM target. | stub (PR #7) |

`skotch emit` is the testing surface: it bypasses build orchestration so
the lexer/parser/typeck/MIR/backend pipeline can be exercised directly
on a single source file. Build orchestration follows once the format
emitters are stable.

## Architectural rules

1. **The shipping `skotch` binary never invokes `kotlinc`, `kotlinc-native`,
   `javac`, `d8`, or `dx`.** Reference outputs for the validation tests are
   produced by `cargo xtask gen-fixtures` (which *does* shell out to those
   tools) and committed to git, so CI needs no JDK or Android SDK. A
   `tests/no_external_compiler.rs` test enforces this by greppping the
   release binary for forbidden tool names.
2. **Parsing and format emission first.** Packaging (jar/APK), signing,
   and the build orchestrator come *after* the front-end and emitters
   are validated by golden fixtures.
3. **Hand-rolled bytecode writers.** Constant-pool forward references make
   `binrw`/`scroll` awkward; `byteorder` is the workhorse for `.class`
   and `.dex`.
4. **Textual LLVM IR.** No `inkwell`/`llvm-sys` dependency — avoids the
   `libLLVM` system requirement and the 30+ second build-time hit.
5. **clang is the *only* external tool the binary ever invokes.** Native
   linking goes through `clang`; everything else is in-process Rust.

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
