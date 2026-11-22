#!/usr/bin/env bash
# A/B two bench_scan builds, interleaved so background load hits both alike:
# bench/ab.sh A_BINARY B_BINARY [N] [ROOT] [THREADS]
a="$1"; b="$2"; n="${3:-9}"; root="${4:-/}"; threads="${5:-12}"
"$a" "$root" "$threads" >/dev/null; "$b" "$root" "$threads" >/dev/null
for _ in $(seq "$n"); do
    echo "A $("$a" "$root" "$threads" | sed -E 's/.*: ([0-9.]+)s.*/\1/')"
    echo "B $("$b" "$root" "$threads" | sed -E 's/.*: ([0-9.]+)s.*/\1/')"
done | awk '{t[$1]=t[$1]" "$2} END {for (k in t) {n=split(t[k],v," "); asort(v); printf "%s median %.3fs (n=%d)\n", k, v[int((n+1)/2)], n}}' | sort
