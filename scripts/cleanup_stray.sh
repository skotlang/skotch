#!/usr/bin/env bash
# Kill stray JVM/test processes left over from parity-bench runs.
#
# Targets processes matching known fixture-test patterns. macOS ps
# doesn't have `etimes`, so we use `pgrep` + per-pid age via lstart.
#
# Usage:
#   scripts/cleanup_stray.sh            # default 300s threshold
#   MAX_AGE_SECS=60 scripts/cleanup_stray.sh
#   scripts/cleanup_stray.sh --dry-run  # show what would be killed

set -u

MAX_AGE_SECS="${MAX_AGE_SECS:-300}"
DRY_RUN=0
if [[ "${1:-}" == "--dry-run" ]]; then DRY_RUN=1; fi

now=$(date +%s)
killed=0

# Patterns — `pgrep -f` regex. Each must include a fixture- or
# daemon-specific keyword so we never catch user activity.
patterns=(
    'kotlinc.* /tmp/.*\.kt'                          # bench compiles into /tmp
    'kotlinc.* parity/.*\.kt'                        # parity-fixture compiles
    'KotlinCompileDaemon'                            # Kotlin compile daemon
    'GradleDaemon'                                   # Gradle daemon
    'skotch-e2e-[0-9]'                               # e2e_jvm tmp child JVMs
    'target/release/deps/e2e_jvm'                    # e2e_jvm test runners
    'target/release/deps/skotch_classes'             # skotch_classes test runners
    'java .*InputKt'                                 # calculator-parser test JVM
    'parity/_shared/matrix\.sh'                      # leftover matrix scripts
)

# Returns elapsed seconds for a pid, or "" if pid is dead.
elapsed_secs() {
    local pid=$1
    local lstart
    lstart=$(ps -o lstart= -p "$pid" 2>/dev/null | sed 's/^ *//')
    [[ -z "$lstart" ]] && return
    local start_epoch
    start_epoch=$(date -j -f '%a %b %e %T %Y' "$lstart" +%s 2>/dev/null) || return
    echo $(( now - start_epoch ))
}

for pat in "${patterns[@]}"; do
    while read -r pid; do
        [[ -z "$pid" ]] && continue
        age=$(elapsed_secs "$pid")
        [[ -z "$age" ]] && continue
        if (( age > MAX_AGE_SECS )); then
            cmd=$(ps -o command= -p "$pid" 2>/dev/null | cut -c1-120)
            if (( DRY_RUN )); then
                echo "would kill pid=$pid age=${age}s :: $cmd"
            else
                kill "$pid" 2>/dev/null && killed=$((killed+1))
            fi
        fi
    done < <(pgrep -f "$pat" 2>/dev/null)
done

# SIGKILL pass for anything still alive 3s later.
if (( DRY_RUN == 0 )) && (( killed > 0 )); then
    sleep 3
    for pat in "${patterns[@]}"; do
        while read -r pid; do
            [[ -z "$pid" ]] && continue
            age=$(elapsed_secs "$pid")
            [[ -z "$age" ]] && continue
            if (( age > MAX_AGE_SECS )); then
                kill -9 "$pid" 2>/dev/null
            fi
        done < <(pgrep -f "$pat" 2>/dev/null)
    done
fi

if (( DRY_RUN == 0 )); then
    echo "cleanup_stray: killed=$killed"
fi
