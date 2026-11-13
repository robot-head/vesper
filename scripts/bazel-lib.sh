# Shared helpers for the acceptance scripts; source it from the repository
# root. Works in POSIX sh and bash.
#
#   bazel_executable LABEL       build LABEL, print the path of its executable
#   bazel_exec LABEL [ARGS...]   build LABEL and run it here, like `cargo run`
#   bazel_prebuild FILE LABEL... build every LABEL in one Bazel invocation and
#                                record each executable's path in FILE
#
# BAZEL overrides the Bazel command (default: bazel, normally bazelisk), and
# BAZEL_FLAGS adds options such as `--config=ci --config=rbe_build`.
#
# Every Bazel command costs about ten seconds of startup, analysis and build
# event upload even when nothing changed, and the acceptance scripts ask for
# about forty executables. CI therefore calls `bazel_prebuild` once and sets
# VESPER_BAZEL_EXECUTABLES to its FILE: `bazel_executable` then answers from
# that file without starting Bazel, and falls back to building any label the
# file does not list. Use it only when the sources cannot change in between,
# as in a CI job.

BAZEL="${BAZEL:-bazel}"
BAZEL_FLAGS="${BAZEL_FLAGS:-}"

_bazel_prebuilt() {
    [ -n "${VESPER_BAZEL_EXECUTABLES:-}" ] && [ -f "$VESPER_BAZEL_EXECUTABLES" ] || return 1
    _bazel_path=$(awk -v label="$1" '$1 == label { print $2; exit }' "$VESPER_BAZEL_EXECUTABLES")
    [ -n "$_bazel_path" ] && [ -f "$_bazel_path" ] || return 1
    printf '%s\n' "$_bazel_path"
}

bazel_executable() {
    _bazel_prebuilt "$1" && return 0
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

bazel_prebuild() {
    _bazel_file=$1
    shift
    # shellcheck disable=SC2086
    "$BAZEL" build $BAZEL_FLAGS "$@" >&2 || return 1
    # The first default output is the executable, as with --output=files
    # above. Labels print as //pkg:name, @//pkg:name or @@//pkg:name
    # depending on the Bazel version; record the //pkg:name form.
    # shellcheck disable=SC2086
    "$BAZEL" cquery $BAZEL_FLAGS --output=starlark \
        --starlark:expr='str(target.label) + " " + (target.files.to_list()[0].path if target.files.to_list() else "")' \
        "set($*)" 2>/dev/null | sed 's|^@*//|//|' > "$_bazel_file.tmp" || return 1
    mv "$_bazel_file.tmp" "$_bazel_file"
}
