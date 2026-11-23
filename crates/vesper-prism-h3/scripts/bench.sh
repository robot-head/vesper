#!/bin/sh
# ABBA benchmark of the last Rust build of vesper-prism-h3 against the Vesper
# port (see examples/abba.rs): builds the example both ways (release), keeps
# each binary, and drives every workload. The Rust codec/protocol core was
# deleted in SH6 lane 5 (#1400), so the Rust arm is built from the last commit
# that had the Rust implementation of each workload in a temporary git worktree
# with this checkout's examples/abba.rs.
# Usage: crates/vesper-prism-h3/scripts/bench.sh [commit]
#        crates/vesper-prism-h3/scripts/bench.sh flip-gate <workload> [commit]
set -eu
cd "$(dirname "$0")/../../.."
# Part 1/2 pure-Rust codec baseline vs Part 3b Rust connection/session baseline.
codec_base="236c57d879e8dc718a406efa5b7aa5b69ffeab66"
conn_base="0f5a9eba809fd81ad7050d7d083a0e729ce009d4"

exe=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe=".exe" ;; esac
target=${CARGO_TARGET_DIR:-$PWD/target}
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
cache="$target/prism-h3-rust-baseline"
mkdir -p "$cache"

flags_tag=$(printf '%s' "${RUSTFLAGS:-}" | tr -cs 'A-Za-z0-9_.-' '_')
[ -n "$flags_tag" ] || flags_tag="default"

build_baseline() {
    commit=$1
    bin="$cache/abba-${commit}-${flags_tag}$exe"
    if [ -x "$bin" ]; then
        echo "$bin"
        return
    fi
    dir=$(mktemp -d)
    tree="$dir/rust-tree"
    trap 'git worktree remove --force "$tree" >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT HUP INT TERM
    git worktree add --detach -q "$tree" "$commit"
    cp crates/vesper-prism-h3/examples/abba.rs "$tree/crates/vesper-prism-h3/examples/abba.rs"
    (cd "$tree" && CARGO_TARGET_DIR="$cache/build-$commit" \
        cargo build --locked --release -p vesper-prism-h3 --example abba) >&2
    cp "$cache/build-$commit/release/examples/abba$exe" "$bin"
    git worktree remove --force "$tree" >/dev/null 2>&1 || true
    rm -rf "$dir"
    trap - EXIT HUP INT TERM
    echo "$bin"
}

default_base_for() {
    case "$1" in
        connection|session) echo "$conn_base" ;;
        *) echo "$codec_base" ;;
    esac
}

cargo build --locked --release -p vesper-prism-h3 --example abba >&2
vesper="$target/release/examples/abba$exe"

if [ "${1:-}" = flip-gate ]; then
    workload=$2
    base=${3:-$(default_base_for "$workload")}
    rust=$(build_baseline "$base")
    exec "$vesper" flip-gate "$workload" "$rust" "$vesper"
fi

override=${1:-}
for workload in decode encode frames messages connection session; do
    base=${override:-$(default_base_for "$workload")}
    rust=$(build_baseline "$base")
    "$vesper" drive "$workload" "$rust" "$vesper"
    if [ "${CALLGRIND:-}" = 1 ]; then
        for arm in rust vesper; do
            bin="$vesper"
            [ "$arm" = rust ] && bin="$rust"
            count() {
                valgrind --tool=callgrind --callgrind-out-file=/dev/null "$bin" once "$workload" "$1" 2>&1 |
                    sed -n "s/.*Collected : //p"
            }
            echo "$workload $arm instructions (5 batches): $(($(count 6) - $(count 1)))"
        done
    fi
done
