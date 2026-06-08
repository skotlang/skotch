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

# --- bytecode similarity ------------------------------------------------
#
# `disasm_dir DIR` walks every `*.class` file under DIR (sorted by
# relative path for stability), runs the host `javap -p -c` on the lot,
# and pipes through a normalizer that rewrites constant-pool indices
# (`#NNN`) to a single placeholder. Constant-pool slot numbers differ
# trivially between any two compilers — even one that's otherwise
# byte-identical — and dominate the diff if you don't strip them.
#
# `class_similarity_pct KC_DIR SK_DIR` produces an integer 0..100
# describing how close the two disassemblies are. 100 = every line
# matched, 0 = every line differed. The metric is line-level on the
# concatenated, normalized output: total = kc_lines + sk_lines;
# changed = count of `<`/`>` lines in `diff`; pct = 100 * (total -
# changed) / total. When both sides are empty we print `—`.

disasm_dir() {
    local dir="$1"
    if [[ ! -d "$dir" ]]; then return 0; fi
    # Build a sorted list of class files using portable bash. `find -print0`
    # + `sort -z` keeps NUL-delimited records so paths with spaces survive.
    local files=()
    while IFS= read -r -d '' f; do
        files+=("$dir/$f")
    done < <(cd "$dir" && find . -type f -name '*.class' -print0 | sort -z)
    if [[ ${#files[@]} -eq 0 ]]; then return 0; fi
    # javap accepts multiple .class files in one invocation and emits
    # them concatenated; with a sorted input list both compilers'
    # outputs traverse classes in the same order, so the resulting
    # diff measures actual code differences, not file-system traversal
    # noise. The `#N` strip is the single normalization step — it
    # makes the metric tolerant to constant-pool reordering, which is
    # the most common false-positive divergence.
    javap -p -c "${files[@]}" 2>/dev/null | sed -E 's/#[0-9]+/#X/g'
}

class_similarity_pct() {
    local kc_dir="$1"
    local sk_dir="$2"
    local kc_tmp sk_tmp
    kc_tmp=$(mktemp -t class_sim_kc.XXXXXX)
    sk_tmp=$(mktemp -t class_sim_sk.XXXXXX)
    # `trap` is a per-shell-environment property; we're running inside
    # a subshell that the calling `$(…)` substitution sets up, so just
    # add a manual cleanup at the end.
    disasm_dir "$kc_dir" > "$kc_tmp"
    disasm_dir "$sk_dir" > "$sk_tmp"
    local kc_lines sk_lines
    kc_lines=$(wc -l < "$kc_tmp" | tr -d ' ')
    sk_lines=$(wc -l < "$sk_tmp" | tr -d ' ')
    local total=$(( kc_lines + sk_lines ))
    if [[ $total -eq 0 ]]; then
        rm -f "$kc_tmp" "$sk_tmp"
        echo "—"
        return 0
    fi
    # `diff` default output prefixes every changed line with `<` or `>`;
    # hunk markers (`Nc`, `Na`, `Nd`) start with digits, so the simple
    # `^[<>]` filter is enough and skips the noise without false hits.
    local changed
    changed=$(diff "$kc_tmp" "$sk_tmp" | grep -cE '^[<>]' || true)
    rm -f "$kc_tmp" "$sk_tmp"
    # Pct rounded to nearest int. Bash truncates toward zero, so add
    # half the divisor before dividing to round.
    local pct=$(( (100 * (total - changed) + (total / 2)) / total ))
    if [[ $pct -lt 0 ]]; then pct=0; fi
    if [[ $pct -gt 100 ]]; then pct=100; fi
    echo "$pct"
}

# --- project mode (external git checkout) -------------------------------
#
# An example directory enters "project mode" by providing a `project.sh`
# file that declares which external repository to clone and which Kotlin
# files in that checkout to compile. The example's own `Main.kt` is then
# compiled separately against the compiled project's class files and
# linked at runtime, so we can confirm — by actually running — that at
# least a minimal subset of the project's public surface loads and is
# callable.
#
# A project.sh script MUST set:
#   PROJECT_REPO       full git URL (https or ssh)
#   PROJECT_REF        tag or branch name to check out
#   PROJECT_KT_FIND    shell command (string) that, when executed from
#                      the project checkout root, prints one .kt path
#                      per line — typically a `find` invocation
# It MAY set:
#   PROJECT_CLASSPATH  extra classpath entries (colon-separated) that
#                      should be passed to the compiler when building
#                      the project — useful for projects with optional
#                      runtime dependencies the bundled stdlib doesn't
#                      cover.
# It MAY also define a `project_prepare DIR CHECKOUT` shell function:
# the harness calls it after the git clone completes and before
# compiling, so the script can fetch Maven JARs, generate sources, run
# code-gen, etc. The function is expected to extend PROJECT_CLASSPATH
# in place (and treat its work as idempotent — the function is called
# every parity run, including the cached-checkout case).

# Path to where this example's external project checkout lives. We keep
# it under the example dir so it's discoverable from a glance at the
# folder, and gitignore takes care of not committing it.
project_checkout_dir() {
    local dir="$1"
    local ref="$2"
    echo "$dir/.checkout/$ref"
}

# Source a project.sh if present and return 0; return 1 otherwise. After
# this call, the caller can check whether `${PROJECT_REPO:-}` is set to
# know whether the example is in project mode.
load_project_config() {
    local dir="$1"
    # Reset every variable a previous example may have set so we don't
    # leak config between examples when running the parity bench.
    unset PROJECT_REPO PROJECT_REF PROJECT_KT_FIND PROJECT_CLASSPATH
    # Also un-define a previous example's `project_prepare` hook so a
    # missing definition on the next example doesn't silently inherit
    # someone else's. `unset -f` is a no-op if the function isn't
    # defined, so this is safe to do unconditionally.
    unset -f project_prepare 2>/dev/null || true
    if [[ ! -f "$dir/project.sh" ]]; then
        return 1
    fi
    # shellcheck source=/dev/null
    source "$dir/project.sh"
    if [[ -z "${PROJECT_REPO:-}" || -z "${PROJECT_REF:-}" || -z "${PROJECT_KT_FIND:-}" ]]; then
        echo "ERROR: $dir/project.sh must set PROJECT_REPO, PROJECT_REF, PROJECT_KT_FIND" >&2
        return 2
    fi
    return 0
}

# Ensure the configured repo is cloned at the requested ref under the
# example dir's `.checkout/<ref>/` slot. Idempotent: if the slot already
# has a checkout for that ref, we trust it (callers can `rm -rf` to
# force a refetch). Stamps a `.ref` marker so we can detect mismatched
# left-over checkouts and rebuild them.
ensure_project_checkout() {
    local dir="$1"
    local repo="$2"
    local ref="$3"
    local target
    target=$(project_checkout_dir "$dir" "$ref")
    local marker="$target/.skotch-project-ref"
    if [[ -d "$target/.git" && -f "$marker" && "$(cat "$marker")" == "$ref" ]]; then
        return 0
    fi
    # Stale or absent — rebuild from scratch. Using --depth 1 + a single
    # branch fetch keeps the network/disk footprint small (clikt at
    # 5.1.0 is ~3 MB shallow vs ~40 MB full).
    rm -rf "$target"
    mkdir -p "$target"
    echo "── cloning $repo @ $ref → $target ──" >&2
    git clone --depth 1 --branch "$ref" "$repo" "$target" >&2
    printf '%s\n' "$ref" > "$marker"
}

# Run PROJECT_KT_FIND from inside the project checkout and emit one
# absolute .kt path per line on stdout. The shell snippet runs via
# `bash -c`, so it can use `find … -name '*.kt'`, multiple commands
# joined with `;`, etc.
list_project_kt_files() {
    local checkout="$1"
    local find_cmd="$2"
    (cd "$checkout" && eval "$find_cmd") | while IFS= read -r p; do
        if [[ "$p" = /* ]]; then
            echo "$p"
        else
            echo "$checkout/$p"
        fi
    done
}

# Compile a project's .kt files into lib_dir using either `kotlinc` or
# the skotch CLI. The two implementations share enough scaffolding that
# they live in one helper parameterized by tool. Sets the corresponding
# LAST_*_MS timing slot, returns the underlying compiler's exit code.
compile_project_with() {
    local tool="$1"        # "kotlinc" or "skotch"
    local dir="$2"         # example dir (parity/100-clikt)
    local lib_dir="$3"     # output dir for compiled project classes
    rm -rf "$lib_dir"
    mkdir -p "$lib_dir"
    ensure_project_checkout "$dir" "$PROJECT_REPO" "$PROJECT_REF"
    local checkout
    checkout=$(project_checkout_dir "$dir" "$PROJECT_REF")
    # Run the optional project_prepare hook (defined in project.sh) so
    # the example can fetch JAR dependencies, run code-gen, etc., and
    # extend PROJECT_CLASSPATH before the compile step assembles the
    # `-classpath` argument below.
    if declare -F project_prepare > /dev/null; then
        project_prepare "$dir" "$checkout" >&2 || return $?
    fi
    local kt_args=()
    while IFS= read -r line; do
        kt_args+=("$line")
    done < <(list_project_kt_files "$checkout" "$PROJECT_KT_FIND")
    if [[ ${#kt_args[@]} -eq 0 ]]; then
        echo "ERROR: PROJECT_KT_FIND produced no files (checkout=$checkout)" >&2
        return 2
    fi
    # Optional extra classpath the project itself depends on. The
    # bundled kotlin-stdlib / kotlinx-coroutines from the host kotlinc
    # are always added; PROJECT_CLASSPATH is for everything else.
    local cp_args=()
    local cp_parts=()
    local coroutines
    if coroutines=$(find_kotlinx_coroutines); then
        cp_parts+=("$coroutines")
    fi
    if [[ -n "${PROJECT_CLASSPATH:-}" ]]; then
        cp_parts+=("$PROJECT_CLASSPATH")
    fi
    if [[ ${#cp_parts[@]} -gt 0 ]]; then
        local joined=""
        local p
        for p in "${cp_parts[@]}"; do
            if [[ -z "$joined" ]]; then joined="$p"; else joined="$joined:$p"; fi
        done
        cp_args=(-classpath "$joined")
    fi
    case "$tool" in
        kotlinc)
            local kotlinc
            if ! kotlinc=$(find_kotlinc); then
                echo "ERROR: kotlinc not found on PATH" >&2; return 1
            fi
            time_cmd "$kotlinc" "${cp_args[@]}" "${kt_args[@]}" -d "$lib_dir"
            local rc=$?
            LAST_KOTLINC_MS=$TIMED_MS
            return $rc
            ;;
        skotch)
            if [[ ! -x "$SKOTCH_BIN" ]]; then
                echo "ERROR: skotch binary not found at $SKOTCH_BIN" >&2
                echo "  build with: cargo build --release" >&2
                return 1
            fi
            # skotch's kotlinc subcommand accepts -classpath the same
            # way the real kotlinc does, even when the underlying
            # implementation may or may not consume every entry.
            time_cmd "$SKOTCH_BIN" kotlinc "${cp_args[@]}" -d "$lib_dir" "${kt_args[@]}"
            local rc=$?
            LAST_SKOTCH_MS=$TIMED_MS
            return $rc
            ;;
        *)
            echo "ERROR: compile_project_with: unknown tool '$tool'" >&2
            return 2
            ;;
    esac
}

# Compile the example's own Main.kt (and any sibling .kt) against the
# already-compiled project library. We always use kotlinc for this step
# because the goal of project mode is to verify that the PROJECT'S
# compiled output is consumable; the example's own Main.kt is just a
# smoke test, and using kotlinc to build it keeps the harness focused
# on the project-compile result rather than on Main.kt's own quirks.
compile_main_against_lib() {
    local dir="$1"
    local out_dir="$2"
    local lib_dir="$3"
    rm -rf "$out_dir"
    mkdir -p "$out_dir"
    local kotlinc
    if ! kotlinc=$(find_kotlinc); then
        echo "ERROR: kotlinc not found on PATH" >&2; return 1
    fi
    local kt_args=()
    while IFS= read -r line; do
        kt_args+=("$line")
    done < <(list_kt_files "$dir")
    if [[ ${#kt_args[@]} -eq 0 ]]; then
        echo "ERROR: project example has no .kt files alongside project.sh" >&2
        return 2
    fi
    local cp_parts=("$lib_dir")
    local coroutines
    if coroutines=$(find_kotlinx_coroutines); then
        cp_parts+=("$coroutines")
    fi
    if [[ -n "${PROJECT_CLASSPATH:-}" ]]; then
        cp_parts+=("$PROJECT_CLASSPATH")
    fi
    local joined=""
    local p
    for p in "${cp_parts[@]}"; do
        if [[ -z "$joined" ]]; then joined="$p"; else joined="$joined:$p"; fi
    done
    "$kotlinc" -classpath "$joined" "${kt_args[@]}" -d "$out_dir"
}

# Build the runtime classpath for a project-mode example: stdlib +
# coroutines (from the regular runtime_classpath helper) plus the
# compiled project library and any PROJECT_CLASSPATH entries.
project_runtime_classpath() {
    local out_dir="$1"
    local lib_dir="$2"
    local cp
    cp=$(runtime_classpath "$out_dir")
    cp="$cp:$lib_dir"
    if [[ -n "${PROJECT_CLASSPATH:-}" ]]; then
        cp="$cp:$PROJECT_CLASSPATH"
    fi
    echo "$cp"
}

# Drive a single compiler through a project-mode example: compile the
# project library, then compile the example's Main.kt against that
# library, then run it. Any step that fails short-circuits with that
# step's exit code so callers can attribute the failure correctly.
run_project_with() {
    local tool="$1"
    local dir="$2"
    # Keep the project library output OUTSIDE the Main.kt output dir so
    # the latter's pre-compile wipe doesn't also nuke the compiled
    # project we just spent seconds (or minutes) building. They live as
    # peers under .out-<tool>-lib and .out-<tool>.
    local lib_dir="$dir/.out-$tool-lib"
    local out_dir="$dir/.out-$tool"
    # Stay bash-3.2 compatible (macOS' default shell) — no `${var^^}`
    # uppercasing, no `${!indirect}` lookup in tight cases. A switch
    # statement is more explicit anyway.
    local ms_label
    case "$tool" in
        kotlinc)
            echo "── kotlinc (project mode) ──" >&2
            ms_label="kotlinc compile"
            ;;
        skotch)
            echo "── skotch (project mode) ──" >&2
            ms_label="skotch  compile"
            ;;
        *) echo "ERROR: run_project_with: unknown tool '$tool'" >&2; return 2 ;;
    esac
    local rc=0
    compile_project_with "$tool" "$dir" "$lib_dir" >&2 || rc=$?
    local ms=0
    case "$tool" in
        kotlinc) ms="$LAST_KOTLINC_MS" ;;
        skotch)  ms="$LAST_SKOTCH_MS"  ;;
    esac
    echo "${ms_label}: ${ms} ms" >&2
    if [[ $rc -ne 0 ]]; then
        return 1
    fi
    # The Main.kt compile is kotlinc-only (see comment on
    # compile_main_against_lib) so its time is not attributed to skotch.
    # If THIS step fails when running under skotch, we still attribute
    # the failure to skotch because the cause is the project library
    # being incomplete / missing types.
    if ! compile_main_against_lib "$dir" "$out_dir" "$lib_dir" >&2; then
        return 2
    fi
    local cp
    cp=$(project_runtime_classpath "$out_dir" "$lib_dir")
    java -cp "$cp" MainKt
}

# --- top-level entry points ---------------------------------------------

run_with_kotlinc() {
    local dir="$1"
    if load_project_config "$dir"; then
        run_project_with kotlinc "$dir"
        return
    fi
    local out_dir="$dir/.out-kotlinc"
    echo "── kotlinc ──" >&2
    compile_with_kotlinc "$dir" "$out_dir" >&2
    echo "kotlinc compile: ${LAST_KOTLINC_MS} ms" >&2
    run_main "$out_dir"
}

run_with_skotch() {
    local dir="$1"
    if load_project_config "$dir"; then
        run_project_with skotch "$dir"
        return
    fi
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
