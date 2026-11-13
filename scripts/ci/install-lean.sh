#!/usr/bin/env bash
# Installs elan and the toolchain pinned in lean-toolchain, then records the
# real `lean` binary in .ci-lean-path. Tests get that absolute path through
# VESPER_LEAN: Bazel runs tests with HOME set to a temporary directory, where
# the elan proxy could not find its toolchains.
set -euo pipefail
cd "$(dirname "$0")/../.."
if ! command -v elan >/dev/null && [[ ! -x "$HOME/.elan/bin/elan" ]]; then
  curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh -o /tmp/elan-init.sh
  sh /tmp/elan-init.sh -y --default-toolchain none
fi
export PATH="$HOME/.elan/bin:$PATH"
lean --version
elan which lean > .ci-lean-path
cat .ci-lean-path
