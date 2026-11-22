#!/usr/bin/env python3
"""ABBA timing of the runtime workload (examples/runtime_bench.rs): the Rust
build (A) against the Vesper build (B, feature `vesper-port`), alternating
A B B A in fresh processes. Prints the per-pair ratio B/A, its mean and a
95% interval (Student t over pairs), and checks both builds print the same
digest. Usage: abba.py <rust-binary> <vesper-binary> [pairs]."""
import math
import subprocess
import sys


def run(binary):
    out = subprocess.run([binary], check=True, capture_output=True, text=True).stdout.split()
    return int(out[1]), out[2]


def main():
    a_bin, b_bin = sys.argv[1], sys.argv[2]
    pairs = int(sys.argv[3]) if len(sys.argv) > 3 else 10
    ratios, digests = [], set()
    for _ in range(pairs):
        a1, da = run(a_bin)
        b1, db = run(b_bin)
        b2, _ = run(b_bin)
        a2, _ = run(a_bin)
        digests.update([da, db])
        ratios.append((b1 + b2) / (a1 + a2))
    n = len(ratios)
    mean = sum(ratios) / n
    sd = math.sqrt(sum((r - mean) ** 2 for r in ratios) / (n - 1))
    # Two-sided 95% Student t quantile for n - 1 degrees of freedom.
    table = {4: 2.776, 5: 2.571, 6: 2.447, 7: 2.365, 8: 2.306, 9: 2.262, 10: 2.228,
             11: 2.201, 12: 2.179, 13: 2.160, 14: 2.145, 15: 2.131, 19: 2.093, 29: 2.045}
    df = n - 1
    t = table.get(df) or table[max(k for k in table if k <= df)] if df >= 4 else 3.182
    half = t * sd / math.sqrt(n)
    print(f"pairs {n}  Vesper/Rust mean {mean:.3f}  95% [{mean - half:.3f}, {mean + half:.3f}]")
    print(f"digests {'equal' if len(digests) == 1 else 'DIFFER: ' + ' '.join(sorted(digests))}")
    sys.exit(0 if len(digests) == 1 else 1)


if __name__ == "__main__":
    main()
