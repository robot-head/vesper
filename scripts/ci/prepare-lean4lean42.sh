#!/usr/bin/env bash
# Compatibility entry point; the external backend is Logos.
set -euo pipefail
exec "$(dirname "$0")/prepare-logos42.sh" "$@"
