#!/usr/bin/env bash
# parity-matrix: classify every parity/* test by status and capture bail traces.
#
# Usage:
#   parity/_shared/matrix.sh           # table to stdout
#   parity/_shared/matrix.sh --json    # JSON to stdout (one record per test)
#   parity/_shared/matrix.sh --bails   # bail census (one line per bail trace)
#
# Status taxonomy (in order of severity):
#   MATCH               — skotch stdout == kotlinc stdout AND both compiled
#   STDOUT_DIFF         — both compiled + ran; stdout differs
#   SKOTCH_EMPTY        — skotch compiled empty body, ran but produced no output
#   SKOTCH_VERIFY_ERR   — skotch compiled but JVM rejected (VerifyError/ClassFormatError)
#   SKOTCH_RUNTIME      — skotch ran and threw at runtime
#   SKOTCH_COMPILE_FAIL — skotch refused to compile
#   KOTLINC_FAIL        — kotlinc refused to compile (test is broken upstream)
#   BOTH_FAIL           — both rejected

set -uo pipefail

SHARED_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARITY_DIR="$(cd "$SHARED_DIR/.." && pwd)"

MODE="table"
case "${1:-}" in
    --json)  MODE="json" ;;
    --bails) MODE="bails" ;;
    --table|"") MODE="table" ;;
    *) echo "unknown mode: $1" >&2; exit 1 ;;
esac

source "$SHARED_DIR/common.sh"

# Single-test classifier — returns status, sk_bytes, kc_bytes, bails (csv).
classify_one() {
    local dir="$1"
    local kc_out kc_rc sk_out sk_rc sk_err
    # Per-test timeout to skip known infinite-compile tests (24-mandelbrot).
    local tout="${MATRIX_TIMEOUT:-30}"
    set +e
    SKOTCH_DEBUG_BAILS=1 timeout "$tout" bash -c "source '$SHARED_DIR/common.sh'; run_with_kotlinc '$dir'" >/tmp/_kc.out 2>/tmp/_kc.err
    kc_rc=$?
    SKOTCH_DEBUG_BAILS=1 timeout "$tout" bash -c "source '$SHARED_DIR/common.sh'; run_with_skotch  '$dir'" >/tmp/_sk.out 2>/tmp/_sk.err
    sk_rc=$?
    set -e

    kc_out=$(cat /tmp/_kc.out)
    sk_out=$(cat /tmp/_sk.out)
    sk_err=$(cat /tmp/_sk.err)

    local kc_bytes=${#kc_out}
    local sk_bytes=${#sk_out}
    # Count [skotch bail] traces from stderr.
    local bail_count
    bail_count=$(grep -c '^\[skotch bail\]' /tmp/_sk.err || true)
    # First bail trace (one line, for the table).
    local first_bail
    first_bail=$(grep -m1 '^\[skotch bail\]' /tmp/_sk.err | sed 's/^\[skotch bail\] //' || true)
    # Detect specific JVM-side failures.
    local jvm_err=""
    if grep -q "VerifyError\|ClassFormatError\|NoSuchMethodError\|IncompatibleClassChangeError\|LinkageError" /tmp/_sk.err; then
        jvm_err=$(grep -m1 -oE '(VerifyError|ClassFormatError|NoSuchMethodError|IncompatibleClassChangeError|LinkageError)' /tmp/_sk.err)
    fi

    local status
    if [[ $kc_rc -ne 0 && $sk_rc -ne 0 ]]; then
        status="BOTH_FAIL"
    elif [[ $kc_rc -ne 0 ]]; then
        status="KOTLINC_FAIL"
    elif [[ $sk_rc -ne 0 ]]; then
        if [[ -n "$jvm_err" ]]; then
            status="SKOTCH_VERIFY_ERR"
        else
            status="SKOTCH_COMPILE_FAIL"
        fi
    elif [[ "$kc_out" == "$sk_out" ]]; then
        status="MATCH"
    elif [[ -z "$sk_out" && -n "$kc_out" ]]; then
        status="SKOTCH_EMPTY"
    else
        status="STDOUT_DIFF"
    fi

    # Emit a single record per format.
    case "$MODE" in
        table)
            printf '%-40s %-22s kc=%-6d sk=%-6d bails=%-3d %s\n' \
                "$(basename "$dir")" "$status" "$kc_bytes" "$sk_bytes" "$bail_count" "${jvm_err:-${first_bail:0:60}}"
            ;;
        json)
            # Properly escape sk_err for JSON. Use base64 to avoid quoting nightmares.
            local first_bail_b64
            first_bail_b64=$(printf '%s' "$first_bail" | base64 -w0 2>/dev/null || printf '%s' "$first_bail" | base64)
            printf '{"name":"%s","status":"%s","kc_bytes":%d,"sk_bytes":%d,"bail_count":%d,"jvm_err":"%s","first_bail_b64":"%s"}\n' \
                "$(basename "$dir")" "$status" "$kc_bytes" "$sk_bytes" "$bail_count" "$jvm_err" "$first_bail_b64"
            ;;
        bails)
            local test_name
            test_name=$(basename "$dir")
            # Emit one line per bail trace, tagged with the test name.
            grep '^\[skotch bail\]' /tmp/_sk.err | while read -r line; do
                printf '%s\t%s\n' "$test_name" "$line"
            done
            ;;
    esac
}

if [[ "$MODE" == "json" ]]; then
    printf '['
    first=1
    for d in "$PARITY_DIR"/*/; do
        [[ -d "$d" && "$(basename "$d")" != "_shared" ]] || continue
        if [[ $first -eq 0 ]]; then printf ','; fi
        first=0
        classify_one "$d"
    done
    printf ']\n'
elif [[ "$MODE" == "bails" ]]; then
    for d in "$PARITY_DIR"/*/; do
        [[ -d "$d" && "$(basename "$d")" != "_shared" ]] || continue
        classify_one "$d"
    done
else
    # Header for table mode.
    printf '%-40s %-22s %-9s %-9s %-9s %s\n' "TEST" "STATUS" "KC_BYTES" "SK_BYTES" "BAILS" "JVM_ERR / FIRST_BAIL"
    printf '%-40s %-22s %-9s %-9s %-9s %s\n' "----" "------" "--------" "--------" "-----" "--------------------"
    for d in "$PARITY_DIR"/*/; do
        [[ -d "$d" && "$(basename "$d")" != "_shared" ]] || continue
        classify_one "$d"
    done
fi
