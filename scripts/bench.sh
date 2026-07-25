#!/usr/bin/env bash
# Differential benchmark: this repo's `git` against a stock git, same repository,
# same commands, same machine. Emits the markdown table the README carries.
#
#   scripts/bench.sh [<repo>]
#
# Environment:
#   ZVCS_GIT    zvcs binary to measure   (default: target/release/git)
#   STOCK_GIT   the git to measure against (default: /usr/bin/git)
#   WARMUP      hyperfine warmup runs    (default: 3)
#   RUNS        hyperfine timed runs     (default: 10)
#
# The numbers are only meaningful when the machine is otherwise idle: every
# figure here is wall-clock, and zvcs deliberately uses every core, so a loaded
# box compresses its lead. Both binaries are measured in the same interleaved
# run, so load affects them together rather than one of them.
set -uo pipefail

repo=${1:-$PWD}
zvcs=${ZVCS_GIT:-$(cd "$(dirname "$0")/.." && pwd)/target/release/git}
stock=${STOCK_GIT:-/usr/bin/git}
warmup=${WARMUP:-3}
runs=${RUNS:-10}

for bin in "$zvcs" "$stock"; do
    [ -x "$bin" ] || { echo "not executable: $bin" >&2; exit 1; }
done
command -v hyperfine >/dev/null || { echo "hyperfine is required" >&2; exit 1; }

# Read-only commands only: a benchmark must be repeatable, and a mutating verb
# would change the repository under its own measurement.
COMMANDS=(
    "status"
    "log --stat -n 30"
    "blame README.md"
    "log"
    "log --format=%s"
    "log --oneline"
    "log --format=%h"
    "log -p -n 20"
    "log -S return --format=%H"
    "describe"
    "cat-file -p HEAD"
    "shortlog -s"
    "diff --name-only HEAD~5"
    "for-each-ref"
    "ls-files"
    "rev-list --count HEAD"
    "show HEAD"
    "diff --stat HEAD~5"
    "tag -l"
)

# Milliseconds from a hyperfine "Time (mean ± σ):  12.3 ms" line, whatever unit
# it chose to print.
to_ms() { awk '{ print ($2 == "s") ? $1 * 1000 : $1 }'; }

commits=$("$stock" -C "$repo" rev-list --count HEAD)
cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo '?')
printf 'Repository: %s (%s commits) · stock git: %s · %s cores · %s runs after %s warmups\n\n' \
    "$(basename "$repo")" "$commits" "$("$stock" --version | awk '{print $3}')" "$cores" "$runs" "$warmup"
printf '| Command | zvcs | git | |\n|---|---:|---:|---:|\n'

for cmd in "${COMMANDS[@]}"; do
    times=$(hyperfine --warmup "$warmup" --runs "$runs" --shell=none --style basic \
        --command-name z "$zvcs -C $repo $cmd" \
        --command-name g "$stock -C $repo $cmd" 2>/dev/null |
        perl -ne 'print "$1\n" if /Time \(mean.*?:\s+(\S+ \S+)/')
    z=$(printf '%s\n' "$times" | sed -n 1p)
    g=$(printf '%s\n' "$times" | sed -n 2p)
    [ -n "$z" ] && [ -n "$g" ] || { printf '| `git %s` | — | — | did not run |\n' "$cmd"; continue; }
    zms=$(printf '%s\n' "$z" | to_ms)
    gms=$(printf '%s\n' "$g" | to_ms)
    printf '| `git %s` | %s | %s | %s |\n' "$cmd" "$z" "$g" \
        "$(awk -v g="$gms" -v z="$zms" 'BEGIN { printf (g >= z) ? "**%.2fx**" : "%.2fx behind", (g >= z) ? g/z : z/g }')"
done
