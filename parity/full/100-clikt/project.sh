# shellcheck shell=bash
# project.sh for parity/100-clikt — sourced by parity/_shared/common.sh
# when running this example. See the comment block in common.sh under
# "project mode" for the contract this file fulfils. Each variable is
# read by the sourcing script via name; shellcheck can't see that and
# would flag them as unused.
# shellcheck disable=SC2034

# The repository whose Kotlin source we'll compile.
PROJECT_REPO="https://github.com/ajalt/clikt.git"

# Tag to check out. Latest release at the time this example was added.
# Bump deliberately so a parity regression on a clikt point release
# doesn't silently change what this example exercises.
PROJECT_REF="5.1.0"

# Shell snippet (run inside the checkout root) that prints one .kt
# path per line. The user's audit used this exact `find` so we keep
# it verbatim. clikt's `clikt/src/` holds both the commonMain
# (multiplatform-Kotlin source) and the jvmMain (JVM-specific Kotlin
# source) trees; both contribute to a JVM build.
PROJECT_KT_FIND="find clikt/src/ -name '*.kt'"

# No extra runtime libraries — clikt's core depends only on
# kotlin-stdlib (host-provided) and a tiny set of kotlinx coroutines
# affordances that the parity harness already adds when discoverable.
PROJECT_CLASSPATH=""
