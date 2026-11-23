#!/bin/sh
# ABBA benchmark of the deleted Rust code (baseline) against the Vesper
# port (examples/abba.rs). SH6 lane 8 (#1403) deleted the Rust, so each
# baseline is examples/abba.rs built at the last commit that had it, in a
# git worktree under the target directory:
#
# - snp, tdx, sgx, policy: the Rust parsers and policy check, at a8570f28;
# - cca_claims: ciborium decoding and the Rust CCA claim parsers, at 11cdaefa;
# - cca_token, corim, corim_store, appraise_cca: coset + ciborium + Rust
#   COSE/CoRIM/CCA collection, profile and appraisal, at 0f5a9eba, built
#   from this checkout's examples/abba.rs (which uses only the crate's
#   public API, so it builds there too).
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
cose=${AEGIS_RUST_COSE_COMMIT:-0f5a9eba809fd81ad7050d7d083a0e729ce009d4}
target="${CARGO_TARGET_DIR:-$PWD/target}"
case "$target" in /* | [A-Za-z]:*) ;; *) target="$PWD/$target" ;; esac
baseline="$target/aegis-rust-baseline"
# The compiler that builds the port, reused for the baselines' ports.
compiler="$target/vesper-port-compiler/x86_64-unknown-linux-gnu/debug/vesper"
exe=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe=.exe ;; esac

# baseline <commit> <overlay>: the path of the Rust arm built at <commit>,
# with this checkout's examples/abba.rs when <overlay> is 1.
baseline() {
    short=$(printf '%.12s' "$1")
    tree="$baseline/t-$short"
    rust="$baseline/release/examples/abba$exe"
    out="$baseline/release/examples/abba-$short$exe"
    if [ ! -x "$out" ] || [ ! -d "$tree" ]; then
        git worktree prune
        [ -d "$tree" ] || git -c core.longpaths=true worktree add --detach -q "$tree" "$1"
        if [ "$2" = 1 ]; then
            cp crates/vesper-aegis/examples/abba.rs "$tree/crates/vesper-aegis/examples/abba.rs"
        fi
        if [ -z "${VESPER_PORT_COMPILER:-}" ] && [ -x "$compiler$exe" ]; then
            export VESPER_PORT_COMPILER="$compiler$exe"
        fi
        (cd "$tree" && CARGO_TARGET_DIR="$baseline" \
            cargo build --locked --release -q -p vesper-aegis --example abba) >&2
        cp "$rust" "$out"
    fi
    echo "$out"
}

cargo build --locked --release -q -p vesper-aegis --example abba
port="$target/release/examples/abba$exe"
if [ "${1:-}" = flip-gate ]; then
    case "$2" in
        cca_claims) rust=$(baseline "$cbor" 1) ;;
        cca_token | corim | corim_store | appraise_cca) rust=$(baseline "$cose" 1) ;;
        *) rust=$(baseline "$parsers" 0) ;;
    esac
    "$port" flip-gate "$2" "$rust"
else
    "$port" compare "$(baseline "$parsers" 0)" snp tdx sgx policy
    "$port" compare "$(baseline "$cbor" 1)" cca_claims
    "$port" compare "$(baseline "$cose" 1)" cca_token corim corim_store appraise_cca
fi
