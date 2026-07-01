# parity

Plain-`.kt` mini-projects that demonstrate Kotlin features and let you
compare `skotch` output against `kotlinc` side-by-side — the
end-to-end byte-parity test suite that complements the unit-level
fixtures under `tests/fixtures/`. No Gradle, no build files — just
`.kt` files and three shell scripts per example:

- `run_kotlinc.sh` — compile with `kotlinc`, run with `java`
- `run_skotch.sh` — compile with `skotch emit`, run with `java`
- `run_both.sh` — run both, diff the stdout

## Layout

Examples are split into two buckets:

- `parity/unit/` — small, focused single-file examples that exercise
  one Kotlin feature (or a small combination). No external
  dependencies, no cloning. This is where new bail-driven repros land.
- `parity/full/` — "project mode" examples that clone a real
  open-source Kotlin project (clikt, ktlint, kotlin-result, hash) and
  compile the whole thing against a pinned tag. Each folder carries a
  `project.sh` describing the repo, ref, and how to enumerate `.kt`
  files.

`parity/_shared/common.sh` is the shared helper both buckets source; it
resolves the skotch binary from the workspace's `target/release/skotch`
automatically — build it once with `cargo build --release` and every
example will pick it up.

Override `SKOTCH_BIN` (or `SKOTCH_DIR`) to point at a different
binary or source tree.

CI runs every example (both buckets) via `scripts/parity_bench.sh` and
publishes a per-example status + speedup ratio to the workflow summary;
see `.github/workflows/ci.yml`'s `parity-bench` job.

Each script sources `_shared/common.sh`, which discovers `kotlinc`,
`kotlin-stdlib.jar`, and `kotlinx-coroutines-core-jvm.jar` from common
locations (Homebrew, `/usr/share/kotlinc`, the Gradle wrapper). Overrides:

| Variable | Default | Purpose |
|---|---|---|
| `SKOTCH_DIR` | `../skotch` | Path to skotch source tree |
| `SKOTCH_BIN` | `$SKOTCH_DIR/target/release/skotch` | Prebuilt binary |
| `KOTLINC_BIN` | `kotlinc` on PATH | Reference compiler |
| `KOTLIN_STDLIB` | auto-located | `kotlin-stdlib.jar` |
| `KOTLINX_COROUTINES` | auto-located | `kotlinx-coroutines-core-jvm.jar` |

`skotch emit` only takes one file, so multi-file examples are compiled by
concatenating the `.kt` files into a single `Main.kt` under
`.out-skotch/` before invocation. `kotlinc` is given all `.kt` files
directly.

## Examples

Sample unit examples (`parity/unit/`):

| # | Folder | Features |
|---|---|---|
| 1 | `01-hello-world` | `fun main`, expression body, `String` interpolation |
| 2 | `02-vars-and-control-flow` | `val`/`var`, `if`/`when`/`for`/`while`, ranges, `firstOrNull` |
| 3 | `03-classes-and-data` (multi-file) | `class`, `data class`, `copy()`, `==`, `private var`, default params |
| 4 | `04-collections-and-stdlib` | `listOf`/`filter`/`map`/`fold`/`zip`/`groupBy`/`joinToString`/`sum`/`max`/`average` |
| 5 | `05-sealed-classes-and-when` (multi-file) | `sealed class`, exhaustive `when (is …)`, polymorphism |
| 6 | `06-coroutines` | `suspend fun`, `runBlocking`, `launch`, `async`/`await`, `delay`, `join` |

Full project examples (`parity/full/`):

| # | Folder | Repo |
|---|---|---|
| 100 | `100-clikt` | [ajalt/clikt](https://github.com/ajalt/clikt) — CLI argument parser |
| 101 | `101-hash` | [appmattus/crypto](https://github.com/appmattus/crypto) — pure-Kotlin hash algorithms |
| 102 | `102-result` | [michaelbull/kotlin-result](https://github.com/michaelbull/kotlin-result) — `Result<V, E>` type |
| 103 | `103-ktlint` | [pinterest/ktlint](https://github.com/pinterest/ktlint) — Kotlin linter |

## Run an example

```sh
cd unit/01-hello-world
./run_kotlinc.sh        # reference output
./run_skotch.sh         # skotch's output
./run_both.sh           # both, with a stdout diff
```

`run_both.sh` exits 0 only when both compile and produce identical
stdout.
