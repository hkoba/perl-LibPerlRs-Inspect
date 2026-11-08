#!/bin/bash
# Smoke test: run perl-inspect on every .pm under perl's privlib and
# record the time each file takes. Fails when a file times out or runs
# out of memory (exit 124 / 137), which is how a runaway analysis shows.
# Compile failures (missing optional dependencies) are counted, not fatal.
#
# usage: tools/smoke-core.sh [-j JOBS] [-t SECS] [-m KB] [-o OUT.tsv] [--deep] [--passes=LIST] [DIR...]
#   defaults: -j nproc, -t 10, -m 4000000 (virtual memory per run), DIR = $Config{privlibexp}
#   PERL_INSPECT selects the binary (default target/release/perl-inspect).
set -u
jobs=$(nproc 2>/dev/null || echo 2); secs=10; mem=4000000; out=smoke-core.tsv; extra=
dirs=()
while [ $# -gt 0 ]; do
  case "$1" in
    -j) jobs=$2; shift 2 ;;
    -t) secs=$2; shift 2 ;;
    -m) mem=$2; shift 2 ;;
    -o) out=$2; shift 2 ;;
    --deep|--passes=*|--args=*) extra="${extra:+$extra }$1"; shift ;;
    *) dirs+=("$1"); shift ;;
  esac
done
bin=${PERL_INSPECT:-target/release/perl-inspect}
[ -x "$bin" ] || { echo "no $bin (cargo build --release -p inspect-cli)" >&2; exit 2; }
[ ${#dirs[@]} -gt 0 ] || dirs=("$(perl -MConfig -e 'print $Config{privlibexp}')")

export bin secs mem extra
one() {
  local f=$1 t0 t1 rc ms
  t0=$(date +%s%N)
  # shellcheck disable=SC2086
  ( ulimit -v "$mem"; exec timeout "$secs" "$bin" $extra "$f" ) >/dev/null 2>&1 </dev/null
  rc=$?
  t1=$(date +%s%N)
  ms=$(( (t1 - t0) / 1000000 ))
  printf '%d.%03d\t%d\t%s\n' $((ms / 1000)) $((ms % 1000)) "$rc" "$f"
}
export -f one

find "${dirs[@]}" -name '*.pm' -type f | sort | xargs -P "$jobs" -I{} bash -c 'one "$1"' _ {} | sort -k3 > "$out"

total=$(wc -l < "$out")
runaway=$(awk -F'\t' '$2 == 124 || $2 == 137 || $2 >= 128' "$out")
failed=$(awk -F'\t' '$2 != 0' "$out" | wc -l)
echo "files: $total, non-zero exit: $failed (compile failures included); slowest:"
sort -rn "$out" | head -10
if [ -n "$runaway" ]; then
  echo "timed out / killed:"; echo "$runaway"
  exit 1
fi
