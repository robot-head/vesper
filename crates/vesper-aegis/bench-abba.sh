#!/bin/sh
# ABBA benchmark of the deleted Rust code (baseline) against the Vesper
# port (examples/abba.rs). SH6 lane 8 (#1403) deleted the Rust, so each
# baseline is examples/abba.rs built at the last commit that had it, in a
# git worktree under the target directory:
#
# - snp, tdx, sgx, policy: the Rust parsers and policy check, at a8570f28;
# - cca_claims, cca_token, corim: ciborium decoding and the Rust CCA claim
#   parsers, at 11cdaefa, built from this checkout's examples/abba.rs
#   (which uses only the crate's public API, so it builds there too).
#
# The worktrees and builds stay between runs (a baseline reads the vendor
# samples from its worktree), so later runs only rebuild the port side.
# Usage: crates/vesper-aegis/bench-abba.sh                  (compare, 9 epochs)
#        crates/vesper-aegis/bench-abba.sh flip-gate <workload>
set -eu
cd "$(dirname "$0")/../.."
# The last commits with each Rust implementation, whose results
# tests/data/port_differential.sha256 records.
parsers=${AEGIS_RUST_COMMIT:-a8570f2857d85dfb8fdbbdda74977b41f478c49d}
cbor=${AEGIS_RUST_CBOR_COMMIT:-11cdaefab068dd34735c3d8b17fe230a7cfc3522}
target="${CARGO_TARGET_DIR:-$PWD/target}"
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
baseline="$target/aegis-rust-baseline"
# The compiler that builds the port, reused for the baselines' ports.
compiler="$target/vesper-port-compiler/x86_64-unknown-linux-gnu/debug/vesper"

# baseline <commit> <overlay>: the path of the Rust arm built at <commit>,
# with this checkout's examples/abba.rs when <overlay> is 1.
baseline() {
    tree="$baseline/tree-$1"
    rust="$baseline/release/examples/abba"
    if [ ! -x "$rust.$1" ] || [ ! -d "$tree" ]; then
        git worktree prune
        [ -d "$tree" ] || git worktree add --detach -q "$tree" "$1"
        if [ "$2" = 1 ]; then
            cp crates/vesper-aegis/examples/abba.rs "$tree/crates/vesper-aegis/examples/abba.rs"
        fi
        if [ -x "$compiler" ]; then
            export VESPER_PORT_COMPILER="$compiler"
        fi
        (cd "$tree" && CARGO_TARGET_DIR="$baseline" \
            cargo build --locked --release -q -p vesper-aegis --example abba) >&2
        cp "$rust" "$rust.$1"
    fi
    echo "$rust.$1"
}

cargo build --locked --release -q -p vesper-aegis --example abba
port="$target/release/examples/abba"
if [ "${1:-}" = flip-gate ]; then
    case "$2" in
        cca_* | corim) rust=$(baseline "$cbor" 1) ;;
        *) rust=$(baseline "$parsers" 0) ;;
    esac
    "$port" flip-gate "$2" "$rust"
else
    "$port" compare "$(baseline "$parsers" 0)" snp tdx sgx policy
    "$port" compare "$(baseline "$cbor" 1)" cca_claims cca_token corim
fi
