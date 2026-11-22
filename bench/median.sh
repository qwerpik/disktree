#!/usr/bin/env bash
# Median of N warm whole-disk scans: bench/median.sh [ROOT] [THREADS] [N]
bench="$(dirname "$(realpath "$0")")/../target/release/examples/bench_scan"
root="${1:-/}"; threads="${2:-12}"; n="${3:-7}"
"$bench" "$root" "$threads" >/dev/null 2>&1   # warm up
for _ in $(seq "$n"); do "$bench" "$root" "$threads" 2>/dev/null; done |
    sed -E 's/.*: ([0-9.]+)s.*/\1/' | sort -n | awk '{a[NR]=$1} END {printf "median %.3fs  min %.3fs  max %.3fs\n", a[int((NR+1)/2)], a[1], a[NR]}'
