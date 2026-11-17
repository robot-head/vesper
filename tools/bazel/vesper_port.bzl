"""Bazel side of the Vesper port harness (port wave P0 lane 6, #878).

`vesper_port_object` runs the in-tree `vesper` binary (`//:bin/vesper`, in
the target configuration, so the build reuses the CLI every test already
builds) as `vesper build --lib` and produces one C-ABI object. Rust targets
link it with `-Clink-arg=$(location ...)`; see
crates/vesper-port-pilot/BUILD.bazel and docs/maintenance/porting-a-crate.md.
Cargo builds use the `vesper-port-harness` build-script helper instead.
"""

# The object targets the configured platform, never the build machine's
# CPU features (remote executors may have more than the test machine).
VESPER_TARGET = select({
    "//tools/bazel:vesper_linux_x86_64": "x86_64-unknown-linux-gnu",
    "//tools/bazel:vesper_linux_aarch64": "aarch64-unknown-linux-gnu",
    "//tools/bazel:vesper_macos_x86_64": "x86_64-apple-darwin",
    "//tools/bazel:vesper_macos_aarch64": "aarch64-apple-darwin",
    "//tools/bazel:vesper_windows_x86_64": "x86_64-pc-windows-msvc",
    "//conditions:default": "",
})

def _vesper_port_object_impl(ctx):
    src = ctx.file.src
    if ctx.attr.edits:
        # A deliberately divergent port: the real source with replacements.
        edited = ctx.actions.declare_file(ctx.label.name + ".ves")
        ctx.actions.expand_template(
            template = src,
            output = edited,
            substitutions = ctx.attr.edits,
        )
        src = edited
    out = ctx.actions.declare_file(ctx.label.name + (".obj" if "windows" in ctx.attr.target else ".o"))
    interface = ctx.actions.declare_file(ctx.label.name + ".json")
    args = ctx.actions.args()
    args.add_all(["build", "--lib", "--lib-name", ctx.attr.library])
    if ctx.attr.target:
        args.add("--target", ctx.attr.target)
    args.add(src)
    args.add("-o", out)
    args.add("--interface", interface)
    ctx.actions.run(
        executable = ctx.executable._vesper,
        arguments = [args],
        inputs = [src],
        outputs = [out, interface],
        mnemonic = "VesperLib",
        progress_message = "Compiling Vesper library %s" % ctx.attr.library,
    )
    return [DefaultInfo(files = depset([out]), runfiles = ctx.runfiles([interface]))]

vesper_port_object = rule(
    implementation = _vesper_port_object_impl,
    doc = "One `vesper build --lib` object (symbols `vesper_<library>_<fn>`).",
    attrs = {
        "edits": attr.string_dict(doc = "Exact text replacements applied to the source first."),
        "library": attr.string(mandatory = True, doc = "Symbol prefix (`vesper_<library>_<fn>`)."),
        "src": attr.label(allow_single_file = [".ves"], mandatory = True),
        "target": attr.string(doc = "Target triple; use VESPER_TARGET."),
        "_vesper": attr.label(
            default = "//:bin/vesper",
            executable = True,
            cfg = "target",
        ),
    },
)
