#!/usr/bin/env bash
# Read a TSV produced by parity_bench.sh and emit a Markdown summary
# suitable for $GITHUB_STEP_SUMMARY. For any non-passing example, fold
# in the first ~80 lines of the diff snippet (collapsed under a
# <details> tag) so PR reviewers can triage without downloading the
# bench artifact.
#
# Usage:
#   scripts/parity_summary.sh <bench.tsv>
#
# Environment:
#   DIFFS_DIR     where per-failure diff files live  (default: <tsv-dir>/diffs)
#   KOTLIN_TAG    Kotlin version label for the heading (default: empty)
#   DIFF_LINES    max lines per diff snippet         (default: 80)
#   REPO_URL      repo URL for example links         (default: skotlang/skotch)
#   REPO_REF      git ref for example links          (default: main)
#   KOTLINC_RUNS  if set + >1, annotate timings as "best of N"

set -eu
set -o pipefail

TSV="${1:?usage: $0 <bench.tsv>}"
DIFFS_DIR="${DIFFS_DIR:-$(dirname "$TSV")/diffs}"
KOTLIN_TAG="${KOTLIN_TAG:-}"
DIFF_LINES="${DIFF_LINES:-80}"
REPO_URL="${REPO_URL:-https://github.com/skotlang/skotch}"
REPO_REF="${REPO_REF:-main}"
KOTLINC_RUNS="${KOTLINC_RUNS:-1}"

heading="### Parity bench"
if [[ -n "$KOTLIN_TAG" ]]; then
    heading="$heading (kotlinc $KOTLIN_TAG)"
fi
echo "$heading"
echo ""

total=0
passed=0
fail_compile=0
fail_diff=0
sum_ratio_x100=0
ratio_count=0
worst_ratio_x100=999999
worst_name=""

# Buffer the rows so we can emit totals first, then the table.
rows=()

while IFS=$'\t' read -r name status kc_ms sk_ms; do
    [[ "$name" == "name" ]] && continue
    total=$(( total + 1 ))
    # Example name shape is `NN-rest` (2-digit slot) or `NNN-rest`
    # (3-digit project-mode slot). Split on the first `-` so the
    # numeric prefix doesn't get truncated for 100+ entries.
    idx="${name%%-*}"
    rest="${name#*-}"

    case "$status" in
        pass)         icon="✅"; passed=$(( passed + 1 ));;
        fail-diff)    icon="❌"; fail_diff=$(( fail_diff + 1 ));;
        fail-kotlinc) icon="⚠️"; fail_compile=$(( fail_compile + 1 ));;
        fail-skotch)  icon="❌"; fail_compile=$(( fail_compile + 1 ));;
        *)            icon="❓";;
    esac

    ratio="—"
    if [[ "${kc_ms:-0}" -gt 0 && "${sk_ms:-0}" -gt 0 ]]; then
        rx100=$(( kc_ms * 100 / sk_ms ))
        whole=$(( rx100 / 100 ))
        cents=$(( rx100 % 100 ))
        ratio=$(printf '%d.%02d×' "$whole" "$cents")
        # Only contribute to the mean / worst-case from passing rows —
        # a failed run's "timings" might just be the partial work
        # before the fault.
        if [[ "$status" == "pass" ]]; then
            sum_ratio_x100=$(( sum_ratio_x100 + rx100 ))
            ratio_count=$(( ratio_count + 1 ))
            if [[ $rx100 -lt $worst_ratio_x100 ]]; then
                worst_ratio_x100=$rx100
                worst_name=$name
            fi
        fi
    fi

    # Link the example name to its source folder on GitHub so reviewers
    # can jump straight from the summary table to the Kotlin source.
    name_link="$REPO_URL/tree/$REPO_REF/parity/$name/"
    rows+=("| $idx | [\`$rest\`]($name_link) | $icon $status | ${kc_ms} ms | ${sk_ms} ms | $ratio |")
done < "$TSV"

# Totals line up top — the most important info should land above the
# fold in the GitHub UI.
mean_ratio="—"
worst_ratio="—"
if [[ $ratio_count -gt 0 ]]; then
    mean_x100=$(( sum_ratio_x100 / ratio_count ))
    mwhole=$(( mean_x100 / 100 ))
    mcents=$(( mean_x100 % 100 ))
    mean_ratio=$(printf '%d.%02d×' "$mwhole" "$mcents")
    wwhole=$(( worst_ratio_x100 / 100 ))
    wcents=$(( worst_ratio_x100 % 100 ))
    worst_ratio=$(printf '%d.%02d× (%s)' "$wwhole" "$wcents" "$worst_name")
fi

echo "**Result:** ${passed}/${total} pass · fail-diff: ${fail_diff} · fail-compile: ${fail_compile}"
echo ""
echo "**Mean skotch speedup over kotlinc (passing examples):** ${mean_ratio}"
echo "**Slowest passing example:** ${worst_ratio}"
if [[ "$KOTLINC_RUNS" -gt 1 ]]; then
    echo ""
    echo "_kotlinc timings are best-of-${KOTLINC_RUNS} (warmup runs discarded);_"
    echo "_skotch is a native binary with no JVM startup, single run._"
fi
echo ""
echo "| # | Example | Status | kotlinc | skotch | ratio |"
echo "|---|---|---|---|---|---|"
printf '%s\n' "${rows[@]}"

# Inline diff snippets for any failure, each behind a <details>. The
# 80-line cap keeps the rendered summary under GitHub's 1 MB limit on
# any plausible failure batch; the full snippet is in the artifact.
if [[ -d "$DIFFS_DIR" ]]; then
    shopt -s nullglob
    found_diffs=0
    for diff_file in "$DIFFS_DIR"/*.txt; do
        if [[ $found_diffs -eq 0 ]]; then
            echo ""
            echo "### Failure diffs"
            echo ""
            found_diffs=1
        fi
        bn="$(basename "$diff_file" .txt)"
        line_count=$(wc -l < "$diff_file" | tr -d ' ')
        echo "<details><summary><strong>${bn}</strong> · ${line_count} lines</summary>"
        echo ""
        echo '```'
        head -"$DIFF_LINES" "$diff_file"
        if [[ $line_count -gt $DIFF_LINES ]]; then
            echo "…"
            echo "(truncated after ${DIFF_LINES} lines; see artifact for full output)"
        fi
        echo '```'
        echo ""
        echo "</details>"
    done
fi
