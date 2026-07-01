#!/usr/bin/env bash
# Run every parity example under parity/unit/*/ and parity/full/*/
# and record (status, kotlinc_ms, skotch_ms, similarity) into a TSV.
# Per-failure diff snippets are written to $OUT_DIR/diffs/. This
# script ALWAYS exits 0 — example failures are data, not CI failures.
#
# Usage:
#   scripts/parity_bench.sh
#
# Environment:
#   OUT_DIR        output directory  (default: <repo>/_bench)
#   OUT_TSV        TSV path          (default: $OUT_DIR/parity_bench.tsv)
#   KOTLINC_RUNS   number of kotlinc runs per example, take the min
#                  (default: 3 — JVM startup + JIT warmup easily double
#                  the first-run timing; min-of-N gives a fair number)

set -u
set -o pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PARITY_DIR="$REPO_ROOT/parity"

# Source the shared helpers (provides run_with_kotlinc / run_with_skotch
# plus tool discovery). SKOTCH_BIN defaults to the workspace's release
# build; callers can override either by environment.
# shellcheck source=/dev/null
source "$PARITY_DIR/_shared/common.sh"

OUT_DIR="${OUT_DIR:-$REPO_ROOT/_bench}"
OUT_TSV="${OUT_TSV:-$OUT_DIR/parity_bench.tsv}"
DIFFS_DIR="$OUT_DIR/diffs"
KOTLINC_RUNS="${KOTLINC_RUNS:-3}"
if ! [[ "$KOTLINC_RUNS" =~ ^[1-9][0-9]*$ ]]; then
    echo "ERROR: KOTLINC_RUNS must be a positive integer, got '$KOTLINC_RUNS'" >&2
    exit 2
fi
mkdir -p "$DIFFS_DIR"

TMP_KC_ERR="$(mktemp -t bench_kc_err.XXXXXX)"
TMP_SK_ERR="$(mktemp -t bench_sk_err.XXXXXX)"
trap 'rm -f "$TMP_KC_ERR" "$TMP_SK_ERR"' EXIT

printf 'name\tstatus\tkotlinc_ms\tskotch_ms\tsimilarity\n' > "$OUT_TSV"

# Iterate parity examples in two passes so the natural numeric
# ordering survives — bash globs sort lexicographically, which would
# otherwise put `100-` before `02-`. The `unit/` bucket holds the
# standalone code examples (2- and 3-digit slots); the `full/` bucket
# holds "project mode" examples that clone an external repository and
# compile the project itself.
shopt -s nullglob
parity_dirs=(
    "$PARITY_DIR"/unit/[0-9][0-9]-*/
    "$PARITY_DIR"/unit/[0-9][0-9][0-9]-*/
    "$PARITY_DIR"/full/[0-9][0-9][0-9]-*/
)
for dir in "${parity_dirs[@]}"; do
    # Preserve the `unit/…` or `full/…` prefix in the TSV name column so
    # downstream tools (summary link generation, artifact triage) can
    # locate the source folder without re-globbing.
    name="${dir#"$PARITY_DIR"/}"
    name="${name%/}"

    # Run kotlinc $KOTLINC_RUNS times and keep the minimum compile
    # time. JVM startup + class loading + JIT warmup add a multi-hundred-
    # ms tax to the first run that's gone by the second; min-of-N
    # measures steady-state cost instead of cold-start.
    #
    # We keep the LAST run's stdout/stderr for status classification
    # (each run wipes the output dir and compiles fresh, so a successful
    # run's stdout is deterministic).
    kc_rc=0
    kc_ms=0
    set +e
    for ((run = 1; run <= KOTLINC_RUNS; run++)); do
        kc_out=$(run_with_kotlinc "$dir" 2>"$TMP_KC_ERR"); kc_rc=$?
        run_ms=$(grep -oE '^kotlinc compile: [0-9]+ ms' "$TMP_KC_ERR" \
            | tail -1 | grep -oE '[0-9]+' | head -1)
        run_ms="${run_ms:-0}"
        # Adopt the first non-zero timing, then keep only smaller ones.
        if [[ $kc_ms -eq 0 ]] || ([[ $run_ms -gt 0 ]] && [[ $run_ms -lt $kc_ms ]]); then
            kc_ms=$run_ms
        fi
        # If the first run failed, don't waste time on retries — kotlinc
        # failures are deterministic (compile error in the source).
        if [[ $kc_rc -ne 0 ]]; then
            break
        fi
    done
    sk_out=$(run_with_skotch "$dir" 2>"$TMP_SK_ERR"); sk_rc=$?
    set -e

    sk_ms=$(grep -oE '^skotch  compile: [0-9]+ ms' "$TMP_SK_ERR" \
        | tail -1 | grep -oE '[0-9]+' | head -1)
    : "${sk_ms:=0}"

    # Status taxonomy:
    #   pass           — both compiled, java ran, stdouts byte-identical
    #   fail-kotlinc   — kotlinc compile or java run failed (rare in CI)
    #   fail-skotch    — skotch compile or java run failed
    #   fail-diff      — both ran but produced different stdout
    if [[ $kc_rc -ne 0 ]]; then
        status="fail-kotlinc"
    elif [[ $sk_rc -ne 0 ]]; then
        status="fail-skotch"
    elif [[ "$kc_out" == "$sk_out" ]]; then
        status="pass"
    else
        status="fail-diff"
    fi

    if [[ "$status" != "pass" ]]; then
        # Capture everything a triager would want into one file: both
        # stdouts (with rc), both stderrs (the compile + run console
        # output, which often surfaces the actual error), and a
        # unified diff of the stdouts. summary script truncates to a
        # readable snippet; the artifact has the full thing.
        # Flatten `unit/01-…` → `unit__01-…` for the diff filename so
        # the file lives directly under $DIFFS_DIR (no subdirs).
        diff_slug="${name//\//__}"
        diff_file="$DIFFS_DIR/$diff_slug.txt"
        {
            echo "=== $name ($status) ==="
            echo "--- kotlinc stdout (rc=$kc_rc) ---"
            printf '%s\n' "$kc_out"
            echo "--- kotlinc stderr ---"
            cat "$TMP_KC_ERR"
            echo "--- skotch stdout (rc=$sk_rc) ---"
            printf '%s\n' "$sk_out"
            echo "--- skotch stderr ---"
            cat "$TMP_SK_ERR"
            echo "--- unified diff (kotlinc → skotch) ---"
            diff -u \
                <(printf '%s\n' "$kc_out") \
                <(printf '%s\n' "$sk_out") || true
        } > "$diff_file"
    fi

    # Class-file similarity: javap-disassemble every .class on both
    # sides and run a line-level diff. Project-mode examples carry
    # their interesting bytecode in `.out-<tool>-lib/` (the compiled
    # external project); standalone examples carry everything in
    # `.out-<tool>/`. Either way the comparison is between matching
    # slots, so the percentage answers the question "how byte-similar
    # is skotch's output to kotlinc's, ignoring constant-pool
    # reordering?".
    if [[ -f "$dir/project.sh" ]]; then
        kc_class_dir="$dir/.out-kotlinc-lib"
        sk_class_dir="$dir/.out-skotch-lib"
    else
        kc_class_dir="$dir/.out-kotlinc"
        sk_class_dir="$dir/.out-skotch"
    fi
    similarity=$(class_similarity_pct "$kc_class_dir" "$sk_class_dir")
    : "${similarity:=—}"

    printf '%s\t%s\t%s\t%s\t%s\n' \
        "$name" "$status" "$kc_ms" "$sk_ms" "$similarity" >> "$OUT_TSV"

    # Live progress to stderr (CI step log) so the run isn't silent.
    printf '  %-12s %-40s  kotlinc=%6s ms  skotch=%6s ms  sim=%4s%%\n' \
        "$status" "$name" "$kc_ms" "$sk_ms" "$similarity" >&2
done

echo "wrote $OUT_TSV" >&2

# Always succeed — example failures are reported, not fatal.
exit 0
