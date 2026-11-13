"""`cargo_test`: run a rust_test binary with Cargo's working directory.

See test_launcher/main.rs for what the launcher does at run time.
"""

def _cargo_test_impl(ctx):
    test = ctx.attr.test
    binary = ctx.executable.test
    launcher = ctx.executable._launcher
    name = ctx.label.name
    if launcher.extension:
        name += "." + launcher.extension
    out = ctx.actions.declare_file(name)
    ctx.actions.symlink(output = out, target_file = launcher, is_executable = True)

    path_dirs = []
    for b in ctx.files.path_binaries:
        d = b.short_path.rpartition("/")[0]
        if d not in path_dirs:
            path_dirs.append(d)

    env = {}
    if RunEnvironmentInfo in test:
        env.update(test[RunEnvironmentInfo].environment)
    env.update(ctx.attr.env)
    env.update({
        "VESPER_TEST_BINARY": binary.short_path,
        "VESPER_TEST_CWD": ctx.attr.cwd or ".",
        "VESPER_TEST_PATH": "|".join(path_dirs),
    })
    if ctx.attr.copy_cwd:
        env["VESPER_TEST_COPY"] = "1"

    runfiles = ctx.runfiles(files = [out, binary, launcher] + ctx.files.path_binaries)
    runfiles = runfiles.merge(test[DefaultInfo].default_runfiles)
    for b in ctx.attr.path_binaries:
        runfiles = runfiles.merge(b[DefaultInfo].default_runfiles)

    return [
        DefaultInfo(executable = out, runfiles = runfiles),
        RunEnvironmentInfo(
            environment = env,
            inherited_environment = test[RunEnvironmentInfo].inherited_environment if RunEnvironmentInfo in test else [],
        ),
    ]

cargo_test = rule(
    implementation = _cargo_test_impl,
    test = True,
    attrs = {
        "copy_cwd": attr.bool(doc = "Run in a copy of `cwd` made of regular files."),
        "cwd": attr.string(doc = "Runfiles path of the directory to run in."),
        "env": attr.string_dict(),
        "path_binaries": attr.label_list(
            allow_files = True,
            cfg = "target",
            doc = "Binaries whose directories go on PATH.",
        ),
        "test": attr.label(executable = True, cfg = "target", mandatory = True),
        "_launcher": attr.label(
            default = "//tools/bazel/test_launcher",
            executable = True,
            cfg = "target",
        ),
    },
)
