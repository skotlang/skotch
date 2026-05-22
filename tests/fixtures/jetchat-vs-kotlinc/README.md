# jetchat-vs-kotlinc

Per-class diff of the JetChat sample compiled two ways:

* **gradle (kotlinc oracle)** — `cd compose-samples/Jetchat && ./gradlew :app:compileDebugKotlin`
* **skotch** — `cd compose-samples/Jetchat && skotch build --target android`

`diff_jetchat.sh` walks gradle's class output (the oracle, 92 classes), finds
the matching skotch class for each, dumps `javap -p -c` of both, and writes a
`diff -u` to `diffs/<fqcn>.diff`. Missing-in-skotch classes are recorded in
`summary.tsv` with `MISSING_IN_SKOTCH`.

## Usage

```sh
# (re)build both, then:
./diff_jetchat.sh

# sort by diff size to focus effort:
sort -k4 -n -r summary.tsv | head

# see all missing classes:
grep MISSING_IN_SKOTCH summary.tsv

# inspect a single class:
diff -u gradle/<fqcn>.javap skotch/<fqcn>.javap | less
```

## Files

* `diff_jetchat.sh` — the harness (zsh, ~50 lines)
* `summary.tsv` — `class\tgradle_lines\tskotch_lines\tdiff_lines\tnotes`
* `gradle/*.javap` — kotlinc output (oracle)
* `skotch/*.javap` — skotch output (test subject)
* `diffs/*.diff` — unified diff per class (only for differing classes)
* `ASSESSMENT.md` — categorized gap analysis + recommended fix order

## Why this is a golden

The harness output is deterministic given the same inputs. To track progress,
commit `summary.tsv` plus the `*.diff` files and watch them shrink as fixes
land. Each closed gap shows up as a line in summary moving from `DIFF` to
`IDENTICAL` (or the diff size dropping noticeably).
