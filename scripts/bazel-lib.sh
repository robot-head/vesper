# Shared helpers for the acceptance scripts; source it from the repository
# root. Works in POSIX sh and bash.
#
#   bazel_executable LABEL       build LABEL, print the path of its executable
#   bazel_exec LABEL [ARGS...]   build LABEL and run it here, like `cargo run`
#
# BAZEL overrides the Bazel command (default: bazel, normally bazelisk), and
# BAZEL_FLAGS adds options such as `--config=ci --config=rbe_build`.

BAZEL="${BAZEL:-bazel}"
BAZEL_FLAGS="${BAZEL_FLAGS:-}"

bazel_executable() {
    # shellcheck disable=SC2086 # BAZEL_FLAGS is a list of options.
    "$BAZEL" build $BAZEL_FLAGS "$1" >&2 || return 1
    # shellcheck disable=SC2086
    "$BAZEL" cquery $BAZEL_FLAGS --output=files "$1" 2>/dev/null | head -n 1
}

bazel_exec() {
    _bazel_label=$1
    shift
    _bazel_bin=$(bazel_executable "$_bazel_label") || return 1
    "$_bazel_bin" "$@"
}
