"""Bazel side of the hybrid Stage 2 build (self-hosting SH0 lane 1, #1123).

`vesper_hybrid` declares the targets of crates/vesper-hybrid: for each
swapped compiler module, its Loom package, the object `//:bin/vesper` (the
default compiler) builds from it with `vesper build --lib`, and that
object's generated binding; then the crate's Cargo targets
(`vesper_crate`), whose build script links each object and checks the
committed binding (build.rs reads `VESPER_HYBRID_OBJECT_<LIBRARY>` and
`VESPER_HYBRID_BINDING_<LIBRARY>`).

The binary includes the root package's CLI sources (src/main.rs and its
command modules), so they are compile data here (`//:files`). Nothing in
`//:vesper`'s or `//:bin/vesper`'s dependency graph refers to these targets.

Divergent objects (the source edits of build.rs under the `divergent`
feature) are built here too so they keep compiling; the Hybrid compiler
workflow runs the Cargo divergence check.
"""

load("//tools/bazel:defs.bzl", "vesper_crate")
load("//tools/bazel:vesper_port.bzl", "VESPER_TARGET", "vesper_package", "vesper_port_object")

def vesper_hybrid(modules, module_deps = {}, divergent = {}, **kwargs):
    """The hybrid compiler crate in this package.

    Args:
      modules: dict from Vesper library name (`vesper_<library>_<fn>`
        symbols, as in build.rs's MODULES) to its Loom package directory,
        relative to this package.
      module_deps: dict from library name to the `vesper_package` labels of
        its package's path dependencies.
      divergent: dict from library name to a struct-like dict
        `{"edit_src": <file>, "edits": {old: new}}` for its divergent object.
      **kwargs: passed to `vesper_crate`.
    """
    data = []
    env = {}
    for library, package in modules.items():
        vesper_package(
            name = library + "_package",
            srcs = native.glob([package + "/src/**/*.ves"]),
            manifest = package + "/Loom.toml",
            deps = module_deps.get(library, []),
        )
        vesper_port_object(
            name = library + "_vesper",
            library = library,
            package = ":" + library + "_package",
            target = VESPER_TARGET,
        )
        native.filegroup(
            name = library + "_vesper_binding",
            srcs = [":" + library + "_vesper"],
            output_group = "rust_binding",
        )
        key = library.upper()
        data += [":" + library + "_vesper", ":" + library + "_vesper_binding"]

        # rules_rust makes these execpaths absolute.
        env["VESPER_HYBRID_OBJECT_" + key] = "$(execpath :%s_vesper)" % library
        env["VESPER_HYBRID_BINDING_" + key] = "$(execpath :%s_vesper_binding)" % library
        if library in divergent:
            vesper_port_object(
                name = library + "_vesper_divergent",
                edit_src = divergent[library]["edit_src"],
                edits = divergent[library]["edits"],
                library = library,
                package = ":" + library + "_package",
                target = VESPER_TARGET,
            )
    vesper_crate(
        build_script_data = data + kwargs.pop("build_script_data", []),
        build_script_env = dict(env, **kwargs.pop("build_script_env", {})),
        **kwargs
    )
