"""Module extension that exposes Cargo's view of the workspace to Bazel.

rules_rs resolves dependencies and features from Cargo.toml/Cargo.lock. This
extension adds what it does not record: each member's version, its Cargo
targets (lib, bins, tests, examples, build script) with their entrypoints,
and per-member dependency data, all from `cargo metadata --no-deps`.

The generated `@vesper_cargo//:packages.bzl` maps a Bazel package path
(`crates/vesper-core`, or `""` for the root crate) to that data, so a member's
BUILD.bazel needs only `vesper_crate()` and stays in step with Cargo when
lanes add targets or dependencies.
"""

load("@host_cargo//:defs.bzl", "RS_HOST_CARGO_LABEL")

_BUILD_FILE_HINT = """\
Workspace member `{dir}` has no BUILD.bazel. Every Cargo workspace member needs
one so Bazel can build it. Create {dir}/BUILD.bazel containing:

    load("//tools/bazel:defs.bzl", "vesper_crate")

    vesper_crate()

(or run `python3 tools/bazel/sync_build_files.py`). See docs/bazel.md.
"""

def _rel(path, root):
    path = path.replace("\\", "/")
    root = root.replace("\\", "/").rstrip("/")
    if path == root:
        return ""
    if not path.startswith(root + "/"):
        fail("%s is outside the workspace root %s" % (path, root))
    return path[len(root) + 1:]

def _impl(mctx):
    cargo = mctx.path(RS_HOST_CARGO_LABEL)
    manifest = mctx.path(Label("//:Cargo.toml"))
    mctx.watch(manifest)
    result = mctx.execute(
        [cargo, "metadata", "--manifest-path", str(manifest), "--no-deps", "--format-version=1", "--quiet", "--offline"],
        working_directory = str(manifest.dirname),
    )
    if result.return_code != 0:
        fail("cargo metadata failed:\n" + result.stdout + result.stderr)
    metadata = json.decode(result.stdout)
    root = metadata["workspace_root"]
    members = {id: True for id in metadata["workspace_members"]}

    packages = {}
    missing = []
    for pkg in metadata["packages"]:
        if pkg["id"] not in members:
            continue
        mctx.watch(pkg["manifest_path"])
        pkg_dir = _rel(str(mctx.path(pkg["manifest_path"]).dirname), root)
        pkg_path = mctx.path(pkg["manifest_path"]).dirname

        # Cargo discovers targets from these directories; re-run when files
        # are added or removed there.
        for sub in ["src", "src/bin", "tests", "examples", "benches"]:
            d = pkg_path.get_child(sub)
            # Watch absent directories too, so creating one re-runs this.
            mctx.watch(d)
            if d.exists and d.is_dir:
                d.readdir(watch = "yes")

        build_file = pkg_path.get_child("BUILD.bazel")
        mctx.watch(build_file)
        if not build_file.exists:
            missing.append(pkg_dir or ".")

        targets = []
        for t in pkg["targets"]:
            src = _rel(t["src_path"], str(pkg_path))
            targets.append({
                "crate_types": t["crate_types"],
                "doctest": t.get("doctest", False),
                "edition": t["edition"],
                "kind": t["kind"],
                "name": t["name"],
                "required_features": t.get("required-features", []),
                "src": src,
                "test": t.get("test", True),
            })
        packages[pkg_dir] = {
            "dir": pkg_dir,
            "edition": pkg["edition"],
            "name": pkg["name"],
            "targets": targets,
            "version": pkg["version"],
        }

    if missing:
        fail("\n".join([_BUILD_FILE_HINT.format(dir = d) for d in sorted(missing)]))

    _cargo_repo(name = "vesper_cargo", packages = json.encode(packages))
    return mctx.extension_metadata(reproducible = True, root_module_direct_deps = ["vesper_cargo"], root_module_direct_dev_deps = [])

def _repo_impl(rctx):
    packages = json.decode(rctx.attr.packages)
    loads = []
    entries = []
    for i, pkg_dir in enumerate(sorted(packages)):
        name = packages[pkg_dir]["name"]
        loads.append(
            'load("@crates//{name}:data.bzl", _ad{i} = "all_crate_deps", _al{i} = "aliases", _cd{i} = "crate_data")'.format(name = name, i = i),
        )
        entries.append(
            '    {key}: struct(cargo = {cargo}, crate_data = _cd{i}, aliases = _al{i}, all_crate_deps = _ad{i}),'.format(
                key = repr(pkg_dir),
                cargo = repr(packages[pkg_dir]),
                i = i,
            ),
        )
    rctx.file("packages.bzl", "\n".join(loads) + "\n\nPACKAGES = {\n" + "\n".join(entries) + "\n}\n")
    rctx.file("BUILD.bazel", 'exports_files(["packages.bzl"])\n')

_cargo_repo = repository_rule(
    implementation = _repo_impl,
    attrs = {"packages": attr.string()},
)

cargo_workspace = module_extension(implementation = _impl)
