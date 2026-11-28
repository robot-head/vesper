"""Bazel side of the Vesper port harness (port wave P0 lane 6, #878; packages
and the seed-copy rule from port wave P1 lane 2, #899).

`vesper_package` declares a Loom package: its `Loom.toml`, its `src/**/*.ves`
and the packages it names as path dependencies. `vesper_port_object` runs the
in-tree `vesper` binary (`//:bin/vesper`, in the target configuration, so the
build reuses the CLI every test already builds) as `vesper build --lib` on a
package or on one `.ves` file, and produces one C-ABI object plus, in the
`rust_binding` output group, the Rust binding the compiler generates for it.

Two ways to link the object:
- A crate whose build.rs calls `vesper_port_harness::Port::build` (a
  port-backed package, see crates/vesper-beacon/BUILD.bazel) passes the object
  and binding to its build script with `build_script_data` and
  `build_script_env = {"VESPER_PORT_OBJECT": "$(execpath ...)"}`; the
  harness archives and links it and checks the committed binding.
- A test links it directly with `-Clink-arg=$(location ...)`, as
  crates/vesper-port-pilot/BUILD.bazel does.

Under `--config=stage2-ports` (the `//tools/bazel:stage2_ports` label flag), the object,
interface and binding come from the Stage 2 port builder
(`//:vesper-port-builder.wasm`, run by the pinned wasmtime CLI) through
`vesper-port-build`, which runs `//:bin/vesper` instead for each port the
builder declines (#2666). The builder itself is built with the flag off.

Port objects must never be dependencies of `//:vesper` or of
`//crates/vesper-codegen-verified`; crates/vesper-port-harness/BUILD.bazel
checks that with a query. See docs/maintenance/porting-a-crate.md.
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

VesperPackageInfo = provider(
    doc = "A Loom package and its path-dependency closure.",
    fields = {
        "manifest": "The package's Loom.toml.",
        "srcs": "The package's own files (manifest and sources).",
        "files": "depset of every file of the package and its dependencies.",
    },
)

def _vesper_package_impl(ctx):
    own = [ctx.file.manifest] + ctx.files.srcs + ctx.files.vsys + ctx.files.objects
    prefixes = [
        ctx.file.manifest.dirname + d
        for d in ("/src/", "/src-wasip2/", "/src-linux/", "/src-macos/", "/src-windows/")
    ]
    for f in ctx.files.srcs:
        if not any([f.path.startswith(p) for p in prefixes]):
            fail("%s is outside the package source directories" % f.path)
    for f in ctx.files.vsys:
        if f.dirname != ctx.file.manifest.dirname:
            fail("%s is not next to %s" % (f.path, ctx.file.manifest.path))
    for f in ctx.files.objects:
        if not f.path.startswith(ctx.file.manifest.dirname + "/"):
            fail("%s is not under %s" % (f.path, ctx.file.manifest.dirname))
    files = depset(own, transitive = [d[VesperPackageInfo].files for d in ctx.attr.deps])
    return [
        VesperPackageInfo(manifest = ctx.file.manifest, srcs = own, files = files),
        DefaultInfo(files = files),
    ]

vesper_package = rule(
    implementation = _vesper_package_impl,
    doc = "A Loom package: `Loom.toml`, `src/**/*.ves`, and its path dependencies.",
    attrs = {
        "deps": attr.label_list(
            providers = [VesperPackageInfo],
            doc = "The `vesper_package` of every path dependency in the manifest.",
        ),
        "manifest": attr.label(allow_single_file = ["Loom.toml"], mandatory = True),
        "srcs": attr.label_list(allow_files = [".ves"], doc = "Every file under src/ (use a glob)."),
        "vsys": attr.label_list(
            allow_files = [".vsys"],
            doc = "The `.vsys` libraries the manifest's `[package] vsys` field names (next to it).",
        ),
        "objects": attr.label_list(
            allow_files = True,
            doc = "The files the manifest's `[package] objects` field names, with the headers and sources they include (under the package).",
        ),
    },
)

Stage2PortToolsInfo = provider(
    doc = "What vesper_port_object needs to try the Stage 2 port builder; all None when off.",
    fields = {
        "builder": "`vesper-port-builder.wasm` (a File), or None.",
        "tool": "`vesper-port-build` (FilesToRunProvider), or None.",
        "wasmtime": "The pinned wasmtime CLI (FilesToRunProvider), or None.",
    },
)

_STAGE2_PORTS = "//tools/bazel:stage2_ports"
_NO_STAGE2_PORTS = "//tools/bazel:no_stage2_ports"

def _without_stage2_ports_impl(_settings, _attr):
    return {_STAGE2_PORTS: _NO_STAGE2_PORTS}

# The builder's own closure is built with the flag off, so the graph has
# no cycle even if a port object ends up in it.
_without_stage2_ports = transition(
    implementation = _without_stage2_ports_impl,
    inputs = [],
    outputs = [_STAGE2_PORTS],
)

def _stage2_port_tools_impl(ctx):
    if ctx.attr.builder == None:
        return [Stage2PortToolsInfo(builder = None, tool = None, wasmtime = None)]
    return [Stage2PortToolsInfo(
        builder = ctx.file.builder,
        tool = ctx.attr.tool[DefaultInfo].files_to_run,
        wasmtime = ctx.attr.wasmtime[DefaultInfo].files_to_run,
    )]

stage2_port_tools = rule(
    implementation = _stage2_port_tools_impl,
    doc = """The Stage 2 port builder for vesper_port_object (#2666), or none.

`//tools/bazel:stage2_ports` (a label_flag, set by `--config=stage2-ports`)
names one of these. Its default names one without a builder, so a query of
a port object's dependencies never reaches the builder.""",
    attrs = {
        "builder": attr.label(
            allow_single_file = [".wasm"],
            cfg = _without_stage2_ports,
            doc = "`//:vesper-port-builder.wasm`.",
        ),
        "tool": attr.label(
            executable = True,
            cfg = "exec",
            doc = "`vesper-port-build`: the builder, then the Rust compiler where it declines.",
        ),
        "wasmtime": attr.label(
            executable = True,
            cfg = "exec",
            doc = "The pinned wasmtime CLI that runs the builder.",
        ),
    },
)

def _package_dir(manifest):
    return manifest.dirname

def _vesper_port_object_impl(ctx):
    if (ctx.attr.src == None) == (ctx.attr.package == None):
        fail("set exactly one of src and package")
    if ctx.attr.package != None:
        info = ctx.attr.package[VesperPackageInfo]
        inputs = info.files.to_list()
        if ctx.attr.edits:
            # A deliberately divergent port: the whole closure copied with its
            # relative layout (path dependencies are relative paths), and
            # the replacements applied to `edit_src`.
            if ctx.file.edit_src == None or ctx.file.edit_src not in info.srcs:
                fail("edits on a package need edit_src, one file of the root package")
            root = ctx.label.name + "_tree/"
            copies = []
            for f in inputs:
                copy = ctx.actions.declare_file(root + f.short_path)
                ctx.actions.expand_template(
                    template = f,
                    output = copy,
                    substitutions = ctx.attr.edits if f == ctx.file.edit_src else {},
                )
                copies.append(copy)
            input_path = ctx.label.package + "/" + root + _package_dir(info.manifest)
            input_path = ctx.bin_dir.path + "/" + input_path
            inputs = copies
        else:
            input_path = _package_dir(info.manifest)
    else:
        src = ctx.file.src
        if ctx.attr.edits:
            edited = ctx.actions.declare_file(ctx.label.name + ".ves")
            ctx.actions.expand_template(
                template = src,
                output = edited,
                substitutions = ctx.attr.edits,
            )
            src = edited
        inputs = [src]
        input_path = src.path
    out = ctx.actions.declare_file(ctx.label.name + (".obj" if "windows" in ctx.attr.target else ".o"))
    interface = ctx.actions.declare_file(ctx.label.name + ".json")
    binding = ctx.actions.declare_file(ctx.label.name + "_binding.rs")
    stage2 = ctx.attr._stage2_ports[Stage2PortToolsInfo]
    if stage2.builder:
        builder = stage2.builder
        wasmtime = stage2.wasmtime.executable
        args = ctx.actions.args()
        args.add("--builder", builder)
        args.add("--wasmtime", wasmtime)
        args.add("--vesper", ctx.executable._vesper)
        args.add("--library", ctx.attr.library)
        args.add("--target", ctx.attr.target)
        args.add("--input", input_path)
        args.add("--object", out)
        args.add("--interface", interface)
        args.add("--binding", binding)
        if ctx.attr.rust_types:
            args.add("--rust-types", ctx.attr.rust_types)
        ctx.actions.run(
            executable = stage2.tool,
            arguments = [args],
            inputs = inputs + [builder],
            tools = [ctx.attr._vesper[DefaultInfo].files_to_run, stage2.wasmtime],
            outputs = [out, interface, binding],
            mnemonic = "VesperLibStage2",
            progress_message = "Compiling Vesper library %s with the Stage 2 port builder" % ctx.attr.library,
        )
    else:
        args = ctx.actions.args()
        args.add_all(["build", "--lib", "--lib-name", ctx.attr.library])
        if ctx.attr.target:
            args.add("--target", ctx.attr.target)
        args.add(input_path)
        args.add("-o", out)
        args.add("--interface", interface)
        args.add("--rust-binding", binding)
        if ctx.attr.rust_types:
            args.add("--rust-types", ctx.attr.rust_types)
        ctx.actions.run(
            executable = ctx.executable._vesper,
            arguments = [args],
            inputs = inputs,
            outputs = [out, interface, binding],
            mnemonic = "VesperLib",
            progress_message = "Compiling Vesper library %s" % ctx.attr.library,
        )
    return [
        DefaultInfo(files = depset([out]), runfiles = ctx.runfiles([interface])),
        OutputGroupInfo(rust_binding = depset([binding]), interface = depset([interface])),
    ]

vesper_port_object = rule(
    implementation = _vesper_port_object_impl,
    doc = "One `vesper build --lib` object (symbols `vesper_<library>_<fn>`) from a package or a file.",
    attrs = {
        "edit_src": attr.label(
            allow_single_file = [".ves"],
            doc = "With `package` and `edits`: the root-package file the edits apply to.",
        ),
        "edits": attr.string_dict(doc = "Exact text replacements applied to the source first."),
        "library": attr.string(mandatory = True, doc = "Symbol prefix (`vesper_<library>_<fn>`)."),
        "package": attr.label(providers = [VesperPackageInfo], doc = "A `vesper_package`."),
        "rust_types": attr.string(doc = "`--rust-types` for the generated binding (for example `crate`)."),
        "src": attr.label(allow_single_file = [".ves"], doc = "One `.ves` file (instead of `package`)."),
        "target": attr.string(doc = "Target triple; use VESPER_TARGET."),
        "_vesper": attr.label(
            default = "//:bin/vesper",
            executable = True,
            cfg = "target",
        ),
        "_stage2_ports": attr.label(
            default = _STAGE2_PORTS,
            providers = [Stage2PortToolsInfo],
        ),
    },
)
