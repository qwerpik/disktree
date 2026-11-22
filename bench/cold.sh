#!/usr/bin/env bash
# Cold-cache whole-disk scan timing: drops the kernel's file caches before
# each run, so every directory and inode is read from the SSD again (the
# first scan after a boot). Needs sudo for /proc/sys/vm/drop_caches only;
# the scan itself runs as you.
set -euo pipefail
bench="$(dirname "$(realpath "$0")")/../target/release/examples/bench_scan"
root="${1:-/}"
sudo -v
for threads in 4 12 24 48; do
    sync
    echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
    "$bench" "$root" "$threads"
done
echo "warm, for comparison:"
"$bench" "$root" 12
