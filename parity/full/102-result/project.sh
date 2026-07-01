# shellcheck shell=bash
# project.sh for parity/102-result — sourced by parity/_shared/common.sh.
# Variables read by the harness via name.
# shellcheck disable=SC2034

# Michael Bull's idiomatic Kotlin Result<V, E> library. Notable for
# its mix of:
#   - inline reified type params + extension functions
#   - `expect`/`actual` multiplatform declarations (BindingException)
#   - opt-in experimental APIs (`UnsafeResultValueAccess`,
#     `UnsafeResultErrorAccess`, `ExperimentalContracts`)
#   - kotlin 2.x `@IgnorableReturnValue` annotation
# All of which are stress points for the skotch compiler.
PROJECT_REPO="https://github.com/michaelbull/kotlin-result.git"
PROJECT_REF="2.3.1"

# The user's file pattern: commonMain + jvmMain from the
# `kotlin-result` subdirectory (the repo also contains
# kotlin-result-coroutines, benchmarks, example — those are out of
# scope).
PROJECT_KT_FIND="find kotlin-result/src/commonMain/kotlin/ kotlin-result/src/jvmMain/kotlin/ -name '*.kt'"

# No JAR dependencies needed — kotlin-result is pure-Kotlin and only
# uses kotlin.contracts (already in the bundled stdlib).
PROJECT_CLASSPATH=""

# Multiplatform `expect class BindingException` in commonMain has its
# `actual` in jvmMain. Plain `kotlinc *.kt` compiles them as ONE
# module and rejects the same-module expect/actual pairing. The fix
# is to assign each .kt to a named fragment via `-Xfragment-sources`
# and declare jvm `refines` common via `-Xfragment-refines`. Both
# args are populated dynamically below from the matching source
# roots; the static flags are inlined here.
__RESULT_STATIC_ARGS=(
    "-Xmulti-platform"
    "-Xexpect-actual-classes"
    "-Xreturn-value-checker=full"
    "-opt-in=kotlin.contracts.ExperimentalContracts"
    "-opt-in=com.github.michaelbull.result.annotation.UnsafeResultValueAccess"
    "-opt-in=com.github.michaelbull.result.annotation.UnsafeResultErrorAccess"
    "-Xfragments=common"
    "-Xfragments=jvm"
    "-Xfragment-refines=jvm:common"
)

project_prepare() {
    local dir="$1"
    local checkout="$2"
    # Re-populate from scratch every call (the harness already cleared
    # PROJECT_EXTRA_ARGS in load_project_config — but project_prepare
    # is called once per compile so a second compile in the same run
    # would otherwise append twice).
    PROJECT_EXTRA_ARGS=("${__RESULT_STATIC_ARGS[@]}")
    # `-Xfragment-sources` values must EXACTLY match the positional
    # source paths kotlinc receives, otherwise it reports
    # "source 'X.kt' does not belong to any module". The harness emits
    # absolute paths from list_project_kt_files, so emit absolute paths
    # here too.
    local f
    while IFS= read -r f; do
        PROJECT_EXTRA_ARGS+=("-Xfragment-sources=common:$checkout/$f")
    done < <(cd "$checkout" && find kotlin-result/src/commonMain/kotlin/ -name '*.kt')
    while IFS= read -r f; do
        PROJECT_EXTRA_ARGS+=("-Xfragment-sources=jvm:$checkout/$f")
    done < <(cd "$checkout" && find kotlin-result/src/jvmMain/kotlin/ -name '*.kt')
}
