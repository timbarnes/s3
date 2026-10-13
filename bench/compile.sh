#!/usr/bin/env bash
# Compile speed (plan/09, S1): the three cases P13.6 measured.
#
#   bench/compile.sh [reps]                    this build
#   bench/compile.sh --compare OLD NEW [reps]  two s3 binaries
#
# - repl: 1,000 definitions on standard input, each a named let and a cond;
# - chibi: --compile of chibi's R7RS suite, with the shim, without 6.2
#   Numbers (which the reader rejects);
# - compiler: --compiler-image, the compiler compiling itself.
#
# Medians of whole-process wall clock, in milliseconds; runs of the two
# binaries alternate, so machine noise hits both alike.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "${1:-}" == "--compare" ]]; then
    bins=("${2:?old binary}" "${3:?new binary}"); reps=${4:-7}
else
    cargo build --release -q
    bins=(target/release/s3); reps=${1:-7}
fi
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT

for i in $(seq 0 999); do
    echo "(define (f$i x y) (let loop ((i 0) (acc '())) (cond ((> i x) (reverse acc)) ((even? i) (loop (+ i 1) (cons (* i y) acc))) (else (loop (+ i 1) acc)))))"
done > "$w/repl.scm"
suite=tests/r7rs/r7rs-tests.scm
start=$(grep -n '^(test-begin "R7RS")' "$suite" | cut -d: -f1)
numbers=$(grep -n '^(test-begin "6.2 Numbers")' "$suite" | cut -d: -f1)
after=$(grep -n '^(test-begin "6.3 Booleans")' "$suite" | cut -d: -f1)
{
    awk '/^\(import /, /^ *\)$/' "$suite" | grep -v '(chibi test)'
    cat tests/r7rs/shim.scm
    sed -n "${start},$((numbers - 1))p; ${after},\$p" "$suite"
} > "$w/chibi.scm"

# One timed run of the command in "$@", in ms.
ms() { python3 -c 'import subprocess, sys, time
t = time.perf_counter()
subprocess.run(sys.argv[2:], stdin=open(sys.argv[1]), stdout=subprocess.DEVNULL, check=True)
print("%.1f" % ((time.perf_counter() - t) * 1000))' "$@"; }
median() { sort -n | awk '{a[NR] = $1} END {print (NR % 2) ? a[(NR + 1) / 2] : (a[NR / 2] + a[NR / 2 + 1]) / 2}'; }

run() {  # case binary
    case $1 in
        repl) ms "$w/repl.scm" "$2" ;;
        chibi) ms /dev/null "$2" --compile "$w/chibi.scm" -o "$w/chibi.s3o" ;;
        compiler) ms /dev/null "$2" --compiler-image "$w/compiler.s3o" ;;
    esac
}

if (( ${#bins[@]} == 2 )); then
    printf '%-10s %9s %9s %7s\n' case old new old/new
else
    printf '%-10s %9s\n' case ms
fi
for c in repl chibi compiler; do
    for b in "${!bins[@]}"; do : > "$w/t$b"; done
    for _ in $(seq "$reps"); do
        for b in "${!bins[@]}"; do run $c "${bins[$b]}" >> "$w/t$b"; done
    done
    a=$(median < "$w/t0")
    if (( ${#bins[@]} == 2 )); then
        b=$(median < "$w/t1")
        printf '%-10s %8sms %8sms %6.2fx\n' $c "$a" "$b" "$(awk -v a="$a" -v b="$b" 'BEGIN {print a / b}')"
    else
        printf '%-10s %8sms\n' $c "$a"
    fi
done
