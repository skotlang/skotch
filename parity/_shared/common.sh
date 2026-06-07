#!/usr/bin/env bash
# Shared helpers used by each example's run_*.sh.
#
# Usage from an example script:
#   #!/usr/bin/env bash
#   set -euo pipefail
#   here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "$here/../_shared/common.sh"
#   run_with_skotch "$here"        # or run_with_kotlinc / run_both
#
# Environment overrides:
#   SKOTCH_BIN     — path to the skotch binary
#   KOTLINC_BIN    — path to kotlinc
#   KOTLIN_STDLIB  — explicit kotlin-stdlib jar
#   KOTLINX_COROUTINES — explicit kotlinx-coroutines-core-jvm jar

set -euo pipefail

SHARED_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARITY_DIR="$(cd "$SHARED_DIR/.." && pwd)"
# parity/ lives inside the skotch repo. The workspace root is the
# directory containing Cargo.toml two levels up from this script
# (parity/_shared → parity → repo root). For backward compatibility
# with the older sibling-checkout layout (skotch-examples standalone
# repo), fall back to ../skotch if Cargo.toml isn't directly above.
# SKOTCH_DIR / SKOTCH_BIN env vars still win over both.
if [[ -f "$PARITY_DIR/../Cargo.toml" ]]; then
    SKOTCH_DIR="${SKOTCH_DIR:-$(cd "$PARITY_DIR/.." && pwd)}"
else
    SKOTCH_DIR="${SKOTCH_DIR:-$PARITY_DIR/../skotch}"
fi

SKOTCH_BIN="${SKOTCH_BIN:-$SKOTCH_DIR/target/release/skotch}"

# --- tool discovery -----------------------------------------------------

find_kotlinc() {
    if [[ -n "${KOTLINC_BIN:-}" ]]; then
        echo "$KOTLINC_BIN"; return 0
    fi
    if command -v kotlinc >/dev/null 2>&1; then
        command -v kotlinc; return 0
    fi
    return 1
}

# Locate a kotlin-related jar by base name. Tries common Homebrew, Linux,
# and Gradle wrapper paths.
locate_kotlin_jar() {
    local stem="$1"  # e.g. "kotlin-stdlib" or "kotlinx-coroutines-core-jvm"
    local d
    for d in \
        /opt/homebrew/Cellar/kotlin/*/libexec/lib \
        /usr/local/Cellar/kotlin/*/libexec/lib \
        /usr/share/kotlinc/lib \
        /usr/local/lib/kotlinc/lib \
        "$HOME"/.gradle/wrapper/dists/gradle-*/*/gradle-*/lib; do
        [[ -d "$d" ]] || continue
        local match
        match=$(ls "$d/$stem".jar 2>/dev/null | head -1) || true
        if [[ -n "${match:-}" ]]; then
            echo "$match"; return 0
        fi
        match=$(ls "$d/$stem"-*.jar 2>/dev/null | sort -V | tail -1) || true
        if [[ -n "${match:-}" ]]; then
            echo "$match"; return 0
        fi
    done
    return 1
}

find_kotlin_stdlib() {
    if [[ -n "${KOTLIN_STDLIB:-}" ]]; then
        echo "$KOTLIN_STDLIB"; return 0
    fi
    locate_kotlin_jar "kotlin-stdlib"
}

find_kotlinx_coroutines() {
    if [[ -n "${KOTLINX_COROUTINES:-}" ]]; then
        echo "$KOTLINX_COROUTINES"; return 0
    fi
    locate_kotlin_jar "kotlinx-coroutines-core-jvm"
}

# --- compile + run ------------------------------------------------------

# Collect every .kt file in the example dir, sorted; Main.kt last so any
# top-level declarations it depends on are already visible to the merged
# file. Emits one path per line on stdout.
list_kt_files() {
    local dir="$1"
    local f
    for f in "$dir"/*.kt; do
        [[ -e "$f" ]] || continue
        if [[ "$(basename "$f")" != "Main.kt" ]]; then
            echo "$f"
        fi
    done
    if [[ -e "$dir/Main.kt" ]]; then
        echo "$dir/Main.kt"
    fi
}

# Build the runtime classpath for `java` given an output directory.
runtime_classpath() {
    local out_dir="$1"
    local cp="$out_dir"
    local stdlib coroutines
    if stdlib=$(find_kotlin_stdlib); then
        cp="$cp:$stdlib"
    else
        echo "ERROR: kotlin-stdlib.jar not found (set KOTLIN_STDLIB to override)" >&2
        return 1
    fi
    if coroutines=$(find_kotlinx_coroutines); then
        cp="$cp:$coroutines"
    fi
    echo "$cp"
}

# --- timing helpers -----------------------------------------------------
#
# `LAST_KOTLINC_MS` / `LAST_SKOTCH_MS` hold the wall-clock duration of the
# most recent compile, in milliseconds. `run_both` reads these for the
# end-of-run timing summary so the two compilers can be compared side-by-
# side.
#
# Timing uses bash's `time` builtin with `TIMEFORMAT='%3R'`, which prints
# the real elapsed time in seconds with 3-decimal precision (e.g.
# `0.123`). We then convert to ms with pure bash arithmetic — no
# external `python`, `awk`, `date +%s%N` (GNU-only), or `$SECONDS`
# (integer-only).
LAST_KOTLINC_MS=0
LAST_SKOTCH_MS=0
TIMED_MS=0

# Run `"$@"` and set `TIMED_MS` to the wall-clock duration in
# milliseconds. The wrapped command's stdout/stderr pass through
# unchanged via saved file descriptors 3 and 4; only the `time` builtin's
# own output (sent to stderr by the inner block, then captured via the
# outer `2>&1`) is consumed here. Exit status of the wrapped command is
# propagated through `$?`.
time_cmd() {
    local TIMEFORMAT='%3R'
    local elapsed rc
    # FD 3 = saved real stdout, FD 4 = saved real stderr.
    exec 3>&1 4>&2
    elapsed=$({ time { "$@" 1>&3 2>&4; }; } 2>&1)
    rc=$?
    exec 3>&- 4>&-
    # `elapsed` is e.g. "0.234" or "1.456". Split on the dot and pad/
    # truncate the fractional part to exactly 3 digits (ms), then
    # combine. `10#` forces base-10 so a leading zero (e.g. "045")
    # doesn't trigger bash's octal interpretation.
    local secs="${elapsed%.*}"
    local frac="${elapsed#*.}"
    while [[ ${#frac} -lt 3 ]]; do frac="${frac}0"; done
    frac="${frac:0:3}"
    TIMED_MS=$(( 10#${secs:-0} * 1000 + 10#${frac} ))
    return $rc
}

# Compile every .kt in the dir with kotlinc → out_dir/. Returns 0/1.
# The output directory is wiped before each invocation so the recorded
# timing reflects a from-scratch compilation, not an incremental one.
compile_with_kotlinc() {
    local dir="$1"
    local out_dir="$2"
    local kotlinc
    if ! kotlinc=$(find_kotlinc); then
        echo "ERROR: kotlinc not found on PATH" >&2; return 1
    fi
    rm -rf "$out_dir"
    mkdir -p "$out_dir"
    local kt_args=()
    while IFS= read -r line; do
        kt_args+=("$line")
    done < <(list_kt_files "$dir")
    # kotlinc auto-includes kotlin-stdlib but not kotlinx-coroutines —
    # add it to the compile classpath so suspend/runBlocking resolve.
    local cp_args=()
    local coroutines
    if coroutines=$(find_kotlinx_coroutines); then
        cp_args=(-classpath "$coroutines")
    fi
    time_cmd "$kotlinc" "${cp_args[@]}" "${kt_args[@]}" -d "$out_dir"
    local rc=$?
    LAST_KOTLINC_MS=$TIMED_MS
    return $rc
}

# Compile every .kt in the dir with skotch → out_dir/.
# Uses `skotch kotlinc -d out/ *.kt`, which is the drop-in kotlinc-CLI
# emulator: same multi-file semantics, same per-class output layout.
# The output directory is wiped before each invocation so the recorded
# timing reflects a from-scratch compilation, not an incremental one.
compile_with_skotch() {
    local dir="$1"
    local out_dir="$2"
    if [[ ! -x "$SKOTCH_BIN" ]]; then
        echo "ERROR: skotch binary not found at $SKOTCH_BIN" >&2
        echo "  build with: cargo build --release" >&2
        return 1
    fi
    rm -rf "$out_dir"
    mkdir -p "$out_dir"
    local kt_args=()
    while IFS= read -r line; do
        kt_args+=("$line")
    done < <(list_kt_files "$dir")
    time_cmd "$SKOTCH_BIN" kotlinc -d "$out_dir" "${kt_args[@]}"
    local rc=$?
    LAST_SKOTCH_MS=$TIMED_MS
    return $rc
}

run_main() {
    local out_dir="$1"
    local cp
    cp=$(runtime_classpath "$out_dir")
    java -cp "$cp" MainKt
}

# --- top-level entry points ---------------------------------------------

run_with_kotlinc() {
    local dir="$1"
    local out_dir="$dir/.out-kotlinc"
    echo "── kotlinc ──" >&2
    compile_with_kotlinc "$dir" "$out_dir" >&2
    echo "kotlinc compile: ${LAST_KOTLINC_MS} ms" >&2
    run_main "$out_dir"
}

run_with_skotch() {
    local dir="$1"
    local out_dir="$dir/.out-skotch"
    echo "── skotch ──" >&2
    compile_with_skotch "$dir" "$out_dir" >&2
    echo "skotch  compile: ${LAST_SKOTCH_MS} ms" >&2
    run_main "$out_dir"
}

# Run both compilers, diff stdout, exit 0 only if both succeed and agree.
# Also prints a side-by-side compile-time comparison.
run_both() {
    local dir="$1"
    local kc_out sk_out kc_rc sk_rc
    set +e
    # `run_with_kotlinc` / `run_with_skotch` are subshells, so we have to
    # surface `LAST_KOTLINC_MS` / `LAST_SKOTCH_MS` through stderr (where
    # the timing line is emitted) and parse them back. The marker prefix
    # is unique enough that grep won't false-match on real compiler
    # output.
    kc_out=$(run_with_kotlinc "$dir" 2>/tmp/_kc.err); kc_rc=$?
    sk_out=$(run_with_skotch  "$dir" 2>/tmp/_sk.err); sk_rc=$?
    set -e

    local kc_ms sk_ms
    kc_ms=$(grep -oE '^kotlinc compile: [0-9]+ ms' /tmp/_kc.err | tail -1 | grep -oE '[0-9]+' | head -1)
    sk_ms=$(grep -oE '^skotch  compile: [0-9]+ ms' /tmp/_sk.err | tail -1 | grep -oE '[0-9]+' | head -1)

    echo
    echo "── kotlinc stdout (rc=$kc_rc) ──"
    printf '%s\n' "$kc_out"
    if [[ -s /tmp/_kc.err ]]; then
        echo "── kotlinc stderr ──"
        cat /tmp/_kc.err
    fi

    echo
    echo "── skotch  stdout (rc=$sk_rc) ──"
    printf '%s\n' "$sk_out"
    if [[ -s /tmp/_sk.err ]]; then
        echo "── skotch stderr ──"
        cat /tmp/_sk.err
    fi

    echo
    echo "── timing (fresh build, output dir wiped first) ──"
    printf '  kotlinc: %s ms\n' "${kc_ms:-?}"
    printf '  skotch : %s ms\n' "${sk_ms:-?}"
    if [[ -n "$kc_ms" && -n "$sk_ms" && "$sk_ms" -gt 0 ]]; then
        # Speedup factor as integer ratio with two decimals. Bash has
        # no float math, so scale by 100 and split: `kc_ms * 100 /
        # sk_ms` → "193" → "1.93x" (skotch faster) or "47" → "0.47x"
        # (skotch slower). The raw ms numbers above make the direction
        # obvious without a sign.
        local ratio_x100=$(( kc_ms * 100 / sk_ms ))
        local whole=$(( ratio_x100 / 100 ))
        local cents=$(( ratio_x100 % 100 ))
        printf '  skotch is %d.%02dx kotlinc\n' "$whole" "$cents"
    fi

    echo
    if [[ $kc_rc -ne 0 || $sk_rc -ne 0 ]]; then
        echo "RESULT: at least one compiler failed"
        return 1
    fi
    if [[ "$kc_out" == "$sk_out" ]]; then
        echo "RESULT: ✓ stdout identical"
        return 0
    else
        echo "RESULT: ✗ stdout differs"
        diff <(printf '%s\n' "$kc_out") <(printf '%s\n' "$sk_out") || true
        return 2
    fi
}
