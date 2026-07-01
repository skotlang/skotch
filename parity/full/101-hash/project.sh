# shellcheck shell=bash
# project.sh for parity/101-hash — sourced by parity/_shared/common.sh.
# See the "project mode" comment block in common.sh for the contract
# this file fulfils. Variables read by the harness via name.
# shellcheck disable=SC2034

# KotlinCrypto hash project — small, focused MD/SHA/Blake/Keccak
# implementation set. Selected for its mix of cross-class generics,
# top-level `internal` extensions, and Long bit-fiddling intrinsics
# (common skotch parity stress points).
PROJECT_REPO="https://github.com/KotlinCrypto/hash.git"
PROJECT_REF="0.8.0"

# All hash families publish their commonMain Kotlin under the same
# layout: `hash/library/<family>/src/commonMain/kotlin/**.kt`. The
# user-supplied find pattern matches every one of them.
PROJECT_KT_FIND="find library/*/src/commonMain/kotlin/ -name '*.kt'"

# Populated by project_prepare (below) with the Maven-resolved
# dependency jars. Starts empty so the harness's classpath assembly
# can append/skip cleanly when the prepare step is a no-op.
PROJECT_CLASSPATH=""

# Maven coordinates pulled from `hash/gradle/libs.versions.toml` at
# the pinned `PROJECT_REF`. KotlinCrypto publishes JVM-specialized
# artifacts under a `-jvm` classifier (the multi-platform metadata
# artifact lacks compiled bytecode), so each module name appears with
# `-jvm-VERSION.jar` here. The `error` module is a separate repo with
# its own version line that wasn't yet folded into the libs.toml at
# the 0.8.0 cut — pinned by hand against Maven Central.
__HASH_DEPS=(
    "org/kotlincrypto/bitops/bits-jvm/0.3.0/bits-jvm-0.3.0.jar"
    "org/kotlincrypto/bitops/endian-jvm/0.3.0/endian-jvm-0.3.0.jar"
    "org/kotlincrypto/core/core-jvm/0.8.0/core-jvm-0.8.0.jar"
    "org/kotlincrypto/core/digest-jvm/0.8.0/digest-jvm-0.8.0.jar"
    "org/kotlincrypto/core/xof-jvm/0.8.0/xof-jvm-0.8.0.jar"
    "org/kotlincrypto/sponges/keccak-jvm/0.5.0/keccak-jvm-0.5.0.jar"
    "org/kotlincrypto/error-jvm/0.3.0/error-jvm-0.3.0.jar"
)

# The `project_prepare` hook is called by common.sh after the git
# checkout and before any compile step. We use it to fetch the Maven
# dependency JARs into the same `.checkout/` slot the gitignore
# already covers, then append them to PROJECT_CLASSPATH so both
# kotlinc and skotch see them on the compile and run classpath.
project_prepare() {
    local dir="$1"
    local jars_dir="$dir/.checkout/_jars/0.8.0"
    mkdir -p "$jars_dir"
    local rel jar_name target rc
    local cp_parts=()
    for rel in "${__HASH_DEPS[@]}"; do
        jar_name=$(basename "$rel")
        target="$jars_dir/$jar_name"
        if [[ ! -f "$target" ]]; then
            echo "── fetching $jar_name ──" >&2
            rc=0
            curl --fail --silent --show-error --location \
                --output "$target" \
                "https://repo1.maven.org/maven2/$rel" \
                || rc=$?
            if [[ $rc -ne 0 ]]; then
                # Leave a partial file behind for diagnosis but skip
                # adding it to the classpath — the missing import will
                # surface as a kotlinc resolve error, which is the
                # right kind of failure to report.
                rm -f "$target"
                echo "WARN: failed to fetch $rel (rc=$rc) — compile will likely fail" >&2
                continue
            fi
        fi
        cp_parts+=("$target")
    done
    # Join with `:` and store back into the harness-visible variable.
    local joined=""
    local p
    for p in "${cp_parts[@]}"; do
        if [[ -z "$joined" ]]; then joined="$p"; else joined="$joined:$p"; fi
    done
    PROJECT_CLASSPATH="$joined"
}
