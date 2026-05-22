#!/bin/zsh
# Compare JetChat's .class files between kotlinc (gradle) and skotch outputs.
# Goal: identify Kotlin-compilation gaps in skotch. Excludes R classes (which
# are skotch's own synthesis from aapt2 symbol_ids, not Kotlin output).

set -u
cd "$(dirname "$0")"

JETCHAT="/opt/src/github/skotlang/compose-samples/Jetchat"
GRADLE_OUT="${JETCHAT}/app/build/intermediates/built_in_kotlinc/debug/compileDebugKotlin/classes"
SKOTCH_OUT="${JETCHAT}/build/d8-input-original"
APP_PKG="com/example/compose/jetchat"

OUT_SKOTCH="./skotch"
OUT_GRADLE="./gradle"
OUT_DIFFS="./diffs"
SUMMARY="./summary.tsv"

rm -rf "$OUT_SKOTCH" "$OUT_GRADLE" "$OUT_DIFFS"
mkdir -p "$OUT_SKOTCH" "$OUT_GRADLE" "$OUT_DIFFS"

# Walk gradle's classes — kotlinc is the oracle. For each, find the matching
# skotch class and javap-dump both, then diff.
echo -e "class\tgradle_lines\tskotch_lines\tdiff_lines\tnotes" > "$SUMMARY"

count=0
missing=0
for gradle_class in $(find "$GRADLE_OUT/$APP_PKG" -name "*.class" | sort); do
    rel="${gradle_class#$GRADLE_OUT/}"
    flat="${rel//\//.}"
    flat="${flat%.class}"

    skotch_class="$SKOTCH_OUT/$rel"
    if [[ ! -f "$skotch_class" ]]; then
        echo -e "$flat\t-\t-\t-\tMISSING_IN_SKOTCH" >> "$SUMMARY"
        missing=$((missing+1))
        continue
    fi

    javap -p -c "$gradle_class" 2>/dev/null > "$OUT_GRADLE/$flat.javap"
    javap -p -c "$skotch_class" 2>/dev/null > "$OUT_SKOTCH/$flat.javap"
    diff -u "$OUT_GRADLE/$flat.javap" "$OUT_SKOTCH/$flat.javap" > "$OUT_DIFFS/$flat.diff" 2>/dev/null
    g_lines=$(wc -l < "$OUT_GRADLE/$flat.javap")
    s_lines=$(wc -l < "$OUT_SKOTCH/$flat.javap")
    d_lines=$(wc -l < "$OUT_DIFFS/$flat.diff")

    if [[ "$d_lines" -eq 0 ]]; then
        rm "$OUT_DIFFS/$flat.diff"
        note=IDENTICAL
    else
        note=DIFF
    fi
    echo -e "$flat\t$g_lines\t$s_lines\t$d_lines\t$note" >> "$SUMMARY"
    count=$((count+1))
done

echo ""
echo "Compared $count classes ($missing missing in skotch)"
echo "Summary: $SUMMARY"
echo "Diffs: $OUT_DIFFS/"
