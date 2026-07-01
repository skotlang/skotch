# shellcheck shell=bash
# project.sh for parity/103-ktlint — sourced by parity/_shared/common.sh.
# Variables read by the harness via name.
# shellcheck disable=SC2034

# Pinterest's Kotlin linter. Cherry-picked subset that builds with
# bare `kotlinc` after stubbing one external annotation: the seven
# `ktlint-cli-reporter-*` modules form a self-contained API surface
# (interfaces + value-class errors + a handful of concrete
# reporters) with no transitive dependency beyond `dev.drewhamilton
# .poko.Poko` (a single annotation used for data-class code-gen
# that we shim out in `project_prepare`). The richer modules
# (`ktlint-rule-engine`, `ktlint-ruleset-standard`, the CLI) pull in
# kotlin-logging, picocli, slf4j, the Kotlin compiler-embeddable jar,
# and a Kotlin compiler plugin — out of scope for a self-contained
# parity test.
PROJECT_REPO="https://github.com/ktlint/ktlint.git"
PROJECT_REF="1.8.0"

# PROJECT_KT_FIND is what the harness calls from the checkout root.
# The Poko shim file lives OUTSIDE the checkout (`project_prepare`
# writes it next to the .checkout slot, gitignored), so we have the
# `find` walk the shim subtree too via an explicit `-path` pattern.
# The leading `..` traverses to the example dir's `.checkout/` parent
# where the shim is staged.
#
# The .checkout root is the directory `find` runs in, so to also pull
# the shim we walk `..` up to the example dir and back into the
# sibling shim slot.
PROJECT_KT_FIND="find ktlint-cli-reporter-core/src/main ktlint-cli-reporter-plain/src/main ktlint-cli-reporter-plain-summary/src/main ktlint-cli-reporter-json/src/main ktlint-cli-reporter-checkstyle/src/main ktlint-cli-reporter-html/src/main ktlint-cli-reporter-format/src/main ../shims -name '*.kt'"

# No runtime classpath additions — every Java SDK class the picked
# modules use (java.io.PrintStream, java.util.jar.Manifest, java.util
# .concurrent.ConcurrentHashMap, etc.) is already on the JDK
# bootclasspath.
PROJECT_CLASSPATH=""

# Shim source for the single non-stdlib annotation used by the picked
# modules. Reproduced verbatim in `project_prepare` so a clean
# .checkout always carries it without requiring a parallel git fetch.
__KTLINT_POKO_SHIM=$(cat <<'EOF'
package dev.drewhamilton.poko

// Compile-time shim for the Poko code-gen annotation
// (https://github.com/drewhamilton/Poko). The real Kotlin
// compiler plugin synthesizes equals/hashCode/toString on
// annotated classes; for the parity harness we only need the type
// to resolve so the picked ktlint reporter sources type-check
// without bringing in the plugin.
@Retention(AnnotationRetention.BINARY)
@Target(AnnotationTarget.CLASS)
public annotation class Poko
EOF
)

project_prepare() {
    local dir="$1"
    # Stage the Poko shim under `.checkout/shims/` — gitignored by the
    # umbrella `parity/.gitignore`'s `.checkout/` rule, and discoverable
    # by `PROJECT_KT_FIND` via the `../shims` traversal. Stand up the
    # directory tree and (re-)write the shim so it survives a manual
    # `rm -rf .checkout/<ref>` poke between runs.
    local shim_dir="$dir/.checkout/shims/dev/drewhamilton/poko"
    mkdir -p "$shim_dir"
    printf '%s\n' "$__KTLINT_POKO_SHIM" > "$shim_dir/Poko.kt"
}
