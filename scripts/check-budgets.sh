#!/usr/bin/env bash
# Checks the performance budgets of the README (and CI) on a release build:
# cold start, idle and workload memory, grid scrolling and typing frame times,
# and reports the binary's size (not a budget). Run it before and after visual
# changes; it exits non-zero when a budget is missed.
#
#   scripts/check-budgets.sh               # build, then measure
#   scripts/check-budgets.sh --no-build    # measure the existing release build
#   scripts/check-budgets.sh --report f.json
#
# The workload memory benchmark connects to the TERNARYGRID_TEST_* servers when
# they are set (see README, "Building and testing").
set -euo pipefail

cd "$(dirname "$0")/.."

build=1
report="target/bench-report.json"
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) build=0 ;;
        --report) report="$2"; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

if [ "$build" = 1 ]; then
    cargo build --release -p ternarygrid-app
fi

bin="target/release/ternarygrid"
[ -f "$bin.exe" ] && bin="$bin.exe"
[ -x "$bin" ] || { echo "no release build at $bin (run without --no-build)" >&2; exit 2; }

# Binary size: reported, not a limit.
size=$(wc -c < "$bin" | tr -d ' ')
mb=$(awk "BEGIN { printf \"%.2f\", $size / 1048576 }")
echo "binary size: $mb MB"
status=0

# Startup, memory, grid and editor frame times (exits non-zero on a miss).
if ! "$bin" --bench all --bench-report "$report"; then
    status=1
fi
echo "report: $report"
exit $status
