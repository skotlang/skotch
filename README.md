# skotch

A Rust toolchain that replaces the Kotlin compiler, Gradle, and Android SDK
build tools with a single CLI.

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

## Running the parity benchmarks

`parity/` holds growing end-to-end Kotlin programs (hello-world through
graph/parser/interpreter examples). Each one is compiled by both `skotch`
and `kotlinc`, then run on the JVM; the bench records compile time for both
and verifies the stdouts are byte-identical.

```sh
cargo build --release -p skotch-cli      # build the release skotch binary
./scripts/parity_bench.sh                # run every parity/NN-* example
./scripts/parity_summary.sh _bench/parity_bench.tsv   # render markdown table
```

`parity_bench.sh` writes `_bench/parity_bench.tsv` (one row per example:
`name`, `status`, `kotlinc_ms`, `skotch_ms`) and per-failure triage files
under `_bench/diffs/`. The script always exits 0 — failures are data, not
errors. `parity_summary.sh` reads the TSV and prints a markdown summary
with the mean speedup ratio plus inline diffs for any non-passing example;
CI pipes it into `$GITHUB_STEP_SUMMARY`.

You can also run a single example directly:

```sh
parity/01-hello-world/run_both.sh        # run both compilers, diff stdouts
parity/01-hello-world/run_skotch.sh      # skotch only
parity/01-hello-world/run_kotlinc.sh     # kotlinc only
```
