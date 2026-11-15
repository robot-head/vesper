#!/usr/bin/env bash
# Host compiler for the isolated Logos source build; Vesper still builds in Bazel.
set -euo pipefail
cd "$(dirname "$0")/../.."
checker_rust_version=1.99.0
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v rustup >/dev/null 2>&1; then
  checker_rustup_installer="$(mktemp)"
  trap 'rm -f "$checker_rustup_installer"' EXIT
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$checker_rustup_installer"
  sh "$checker_rustup_installer" -y --no-modify-path --profile minimal \
    --default-toolchain "$checker_rust_version"
else
  rustup toolchain install "$checker_rust_version" --profile minimal
fi
rustup which --toolchain "$checker_rust_version" cargo > .ci-logos-cargo-path
"$(cat .ci-logos-cargo-path)" --version
