"""`vesper_crate()`: Bazel targets for one Cargo workspace member.

Every target Cargo would build for `cargo test --workspace --all-targets` is
declared from the member's `cargo metadata` entry (see cargo_workspace.bzl):

  :<dir>                  the library (or proc macro), when the member has one
  :<dir>_unit_test        the library's `#[cfg(test)]` tests
  :<dir>_doc_test         the library's doctests
  :bin/<name>             each binary; `:bin/<name>_unit_test` for its tests
  :test/<name>            each integration test under tests/
  :example/<name>         each example (built, not run, like `cargo test`)
  :<dir>_build_script     build.rs, when present

`<dir>` is the basename of the member's directory, which is the name rules_rs
uses for first-party dependency edges (`//crates/vesper-core`).

plus per-package lint tests:

  :clippy                 clippy over every Rust target above
  :rustfmt                `rustfmt --check` over the same targets

Tests run through a launcher (tools/bazel/test_launcher) that starts them in
the package directory, as Cargo does. `CARGO_MANIFEST_DIR` is therefore `.`
for tests, and `CARGO_BIN_EXE_<name>` is the bare binary name, found on the
PATH the launcher sets up. Code that needs an absolute path can canonicalize
either value at run time.
"""

load("@rules_rs//rs:cargo_build_script.bzl", "cargo_build_script")
load("@rules_rs//rs:rust_binary.bzl", "rust_binary")
load("@rules_rs//rs:rust_library.bzl", "rust_library")
load("@rules_rs//rs:rust_proc_macro.bzl", "rust_proc_macro")
load("@rules_rs//rs:rust_test.bzl", "rust_test")
load("@rules_rs//rs/private:all_crate_deps.bzl", _all_crate_deps = "all_crate_deps")
load("@rules_rust//rust:defs.bzl", "rust_clippy_test", "rust_doc_test", "rustfmt_test")
load(":cargo_test.bzl", "cargo_test")
load("@vesper_cargo//:packages.bzl", "PACKAGES")

# Files that are never inputs, whichever package glob picks them up.
_NEVER = [
    "**/target/**",
    "bazel-*/**",
    ".git/**",
    "**/BUILD.bazel",
    "**/*.pyc",
    "**/__pycache__/**",
    # Personal settings (may hold the BuildBuddy API key) and CI scratch.
    "user.bazelrc",
    ".ci-lean-path",
]

def _fix_label(label):
    # rules_rs renders a path dependency on the workspace root crate as
    # "//" + <absolute workspace path>; the root crate is //:vesper.
    if label.startswith("///") or label.startswith("//") and ":" in label[2:4]:
        return "//:vesper"
    return label

def _sanitize(crate_data):
    data = dict(crate_data)
    for kind in ["deps", "dev_deps", "build_deps"]:
        data[kind] = [_fix_label(d) for d in data.get(kind, [])]
        data[kind + "_by_platform"] = {
            p: [_fix_label(d) for d in deps]
            for p, deps in data.get(kind + "_by_platform", {}).items()
        }
    data["aliases"] = {_fix_label(k): v for k, v in data.get("aliases", {}).items()}
    return data

def _features(crate_data):
    by_platform = crate_data.get("crate_features_by_platform", {})
    shared = crate_data.get("crate_features", [])
    if not by_platform:
        return shared
    branches = dict(by_platform)
    branches["//conditions:default"] = []
    return shared + select(branches)

def _enabled(target, crate_data):
    # Cargo skips a target whose required features are not enabled.
    enabled = crate_data.get("crate_features", [])
    for f in target["required_features"]:
        if f not in enabled:
            return False
    return True

def _strip_rs(src):
    return src[:-3] if src.endswith(".rs") else src

def vesper_crate(
        compile_data = None,
        test_data = None,
        extra_compile_data = [],
        extra_test_data = [],
        build_script_data = [],
        build_script_env = {},
        test_env = {},
        test_tags = {},
        copy_test_cwd = [],
        test_size = "medium",
        test_target_compatible_with = {},
        test_timeouts = {}):
    """Declares the Cargo targets of the workspace member in this package.

    Args:
      compile_data: globs of files `include_str!`/`include_bytes!` may read.
        Defaults to every file in the package.
      test_data: globs of files tests may read at run time. Defaults to every
        file in the package.
      extra_compile_data: labels of files outside the package read at compile
        time, e.g. `//crates/vesper-loom-sandbox:files`.
      extra_test_data: labels of files outside the package read at run time.
      build_script_data: labels the build script reads.
      build_script_env: extra environment for the build script.
      test_env: extra run-time environment for every test target.
      test_tags: map from test target name (for example `test/lean`) to extra
        tags, such as `no-remote-exec` for a test that needs a host tool.
      copy_test_cwd: test target names that must run in a copy of the
        package made of regular files instead of runfiles symlinks.
      test_size: Bazel size for every test target.
      test_target_compatible_with: map from test target name to its
        `target_compatible_with`, to skip one test on some platforms.
      test_timeouts: map from test target name to a Bazel timeout, such as
        "long". Unlisted tests keep the default determined by their size.
    """
    pkg = native.package_name()
    if pkg not in PACKAGES:
        fail("//%s is not a Cargo workspace member; add it to [workspace].members in Cargo.toml" % pkg)
    entry = PACKAGES[pkg]
    cargo = entry.cargo
    crate_data = _sanitize(entry.crate_data())
    aliases = crate_data["aliases"]

    def all_crate_deps(**kwargs):
        return _all_crate_deps(crate_data, platforms = crate_data["platforms"], **kwargs)

    name = pkg.rpartition("/")[2] if pkg else "vesper"
    version = cargo["version"]
    rel_dir = pkg if pkg else "."

    native.filegroup(
        name = "files",
        srcs = native.glob(["**"], exclude = _NEVER),
        visibility = ["//visibility:public"],
    )

    compile_files = native.glob(compile_data, exclude = _NEVER, allow_empty = True) if compile_data != None else [":files"]
    test_files = native.glob(test_data, exclude = _NEVER, allow_empty = True) if test_data != None else [":files"]
    compile_files = compile_files + extra_compile_data
    test_files = test_files + extra_test_data

    # Cargo sets these for every target of a package.
    pkg_env = {
        "CARGO_PKG_NAME": cargo["name"],
        "CARGO_PKG_VERSION": version,
    }

    # Integration tests run in the package directory (see the module
    # docstring). Unit tests keep rules_rust's absolute CARGO_MANIFEST_DIR,
    # because proc macros in library code (wasmtime's bindgen!) read it at
    # compile time.
    test_rustc_env = dict(pkg_env)
    integration_rustc_env = dict(pkg_env)
    integration_rustc_env.update({
        "CARGO_MANIFEST_DIR": ".",
        "CARGO_TARGET_TMPDIR": ".",
    })

    features = _features(crate_data)
    normal = all_crate_deps(normal = True)
    dev = all_crate_deps(normal_dev = True)
    normal_and_dev = all_crate_deps(normal = True, normal_dev = True)

    lib = None
    bins = []
    tests = []
    examples = []
    build_rs = None
    for t in cargo["targets"]:
        kinds = t["kind"]
        if "custom-build" in kinds:
            build_rs = t
        elif "lib" in kinds or "rlib" in kinds or "proc-macro" in kinds:
            lib = t
        elif "bin" in kinds:
            bins.append(t)
        elif "test" in kinds:
            tests.append(t)
        elif "example" in kinds:
            examples.append(t)

    # Every Rust target, for the clippy and rustfmt checks.
    rust_targets = []

    # `src/**` covers modules of the lib and of the bins that live under src/.
    srcs = native.glob(["src/**/*.rs"], allow_empty = True)

    build_deps = []
    if build_rs:
        cargo_build_script(
            name = name + "_build_script",
            srcs = [build_rs["src"]],
            crate_root = build_rs["src"],
            edition = build_rs["edition"],
            deps = all_crate_deps(build = True),
            crate_features = features,
            data = native.glob(["**"], exclude = _NEVER + ["tests/**", "examples/**", "benches/**"]) + build_script_data,
            build_script_env = dict(pkg_env, **build_script_env),
            pkg_name = cargo["name"],
            version = version,
            visibility = ["//visibility:private"],
        )
        build_deps = [":" + name + "_build_script"]

    common = dict(
        aliases = aliases,
        crate_features = features,
        version = version,
    )

    def wrapped_test(label, path_binaries = [], **kwargs):
        # The rust_test itself is an implementation detail of the cargo_test
        # that runs it; `manual` keeps `bazel test //...` from running it
        # without the launcher.
        inner = "_" + label + ".bin" if "/" not in label else label.replace("/", "/_", 1) + ".bin"
        rust_test(
            name = inner,
            data = test_files + path_binaries,
            rustc_env = dict(test_rustc_env, **kwargs.pop("rustc_env", {})),
            tags = ["manual"],
            **kwargs
        )
        rust_targets.append(":" + inner)
        cargo_test(
            name = label,
            test = ":" + inner,
            cwd = rel_dir,
            copy_cwd = label in copy_test_cwd,
            env = test_env,
            path_binaries = path_binaries,
            size = test_size,
            timeout = test_timeouts.get(label),
            target_compatible_with = test_target_compatible_with.get(label, []),
            tags = test_tags.get(label, []),
        )

    if lib:
        rule = rust_proc_macro if "proc-macro" in lib["kind"] else rust_library
        rule(
            name = name,
            crate_name = lib["name"].replace("-", "_"),
            srcs = srcs + ([lib["src"]] if lib["src"] not in srcs else []),
            crate_root = lib["src"],
            edition = lib["edition"],
            compile_data = compile_files,
            deps = normal + build_deps,
            rustc_env = pkg_env,
            visibility = ["//visibility:public"],
            **common
        )
        rust_targets.append(":" + name)
        if lib["test"]:
            wrapped_test(
                name + "_unit_test",
                crate = ":" + name,
                deps = dev,
                **common
            )
        if lib["doctest"] and "proc-macro" not in lib["kind"]:
            rust_doc_test(
                name = name + "_doc_test",
                crate = ":" + name,
                deps = dev,
                tags = test_tags.get(name + "_doc_test", []),
            )

    lib_dep = [":" + name] if lib else []

    bin_env = {}
    bin_labels = []
    for b in bins:
        if not _enabled(b, crate_data):
            continue
        label = "bin/" + b["name"]
        bin_labels.append(":" + label)

        # Bare name: the launcher puts this package's binaries on PATH.
        bin_env["CARGO_BIN_EXE_" + b["name"]] = b["name"]
        rust_binary(
            name = label,
            crate_name = b["name"].replace("-", "_"),
            srcs = srcs + ([b["src"]] if b["src"] not in srcs else []),
            crate_root = b["src"],
            edition = b["edition"],
            compile_data = compile_files,
            deps = normal + build_deps + lib_dep,
            rustc_env = pkg_env,
            visibility = ["//visibility:public"],
            **common
        )
        rust_targets.append(":" + label)
        if b["test"]:
            wrapped_test(
                label + "_unit_test",
                crate = ":" + label,
                deps = dev,
                **common
            )

    test_srcs = native.glob(["tests/**/*.rs"], allow_empty = True)
    for t in tests:
        if not _enabled(t, crate_data):
            continue
        wrapped_test(
            "test/" + t["name"],
            path_binaries = bin_labels,
            crate_name = t["name"].replace("-", "_"),
            srcs = test_srcs + ([t["src"]] if t["src"] not in test_srcs else []),
            crate_root = t["src"],
            edition = t["edition"],
            compile_data = compile_files,
            deps = normal_and_dev + build_deps + lib_dep,
            rustc_env = dict(integration_rustc_env, **bin_env),
            **common
        )

    example_srcs = native.glob(["examples/**/*.rs"], allow_empty = True)
    for e in examples:
        if not _enabled(e, crate_data):
            continue
        rust_binary(
            name = "example/" + e["name"],
            crate_name = e["name"].replace("-", "_"),
            srcs = example_srcs + ([e["src"]] if e["src"] not in example_srcs else []),
            crate_root = e["src"],
            edition = e["edition"],
            compile_data = compile_files,
            deps = normal_and_dev + build_deps + lib_dep,
            # Scripts run examples from the repository root, like `cargo run`.
            rustc_env = dict(pkg_env, CARGO_MANIFEST_DIR = rel_dir),
            **common
        )
        rust_targets.append(":example/" + e["name"])

    if build_rs:
        rust_targets.append(":" + name + "_build_script_")

    rust_clippy_test(
        name = "clippy",
        targets = rust_targets,
        tags = ["clippy", "lint"],
    )
    rustfmt_test(
        name = "rustfmt",
        targets = rust_targets,
        tags = ["lint", "rustfmt"],
    )
