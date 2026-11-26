"""Hermetic prebuilt `wac` CLI repository extension (#2210).

Stage 2 lowers `Ffi[Cranelift]` to imports of `vesper:cranelift/vcl@0.1.0`,
which crates/vesper-cranelift-component exports. `wac plug` composes the
two components into one the pinned wasmtime CLI (tools/bazel/wasmtime.bzl)
runs on its own. This pins the official `bytecodealliance/wac` v0.8.1
release binaries by SHA-256 and exposes `@wac//:wac` via `select`.
There is no Windows AArch64 release; that platform has no `@wac`.
"""

WAC_VERSION = "0.8.1"

_BASE = "https://github.com/bytecodealliance/wac/releases/download/v0.8.1/wac-cli-"

_TARGETS = {
    "linux_x86_64": {
        "os": "linux",
        "cpu": "x86_64",
        "url": _BASE + "x86_64-unknown-linux-musl",
        "sha256": "ce30f33c5bc40095cfb4e74ae5fb4ba515d4f4bef2d597831bc7afaaf0d55b6c",
        "binary": "wac",
    },
    "linux_aarch64": {
        "os": "linux",
        "cpu": "aarch64",
        "url": _BASE + "aarch64-unknown-linux-musl",
        "sha256": "3b78ae7c732c1376d1c21b570d07152a07342e9c4f75bff1511cde5f6af01f12",
        "binary": "wac",
    },
    "macos_aarch64": {
        "os": "macos",
        "cpu": "aarch64",
        "url": _BASE + "aarch64-apple-darwin",
        "sha256": "f08496f49312abd68d9709c735a987d6a17d2295a1240020d217a9de8dcaaacd",
        "binary": "wac",
    },
    "macos_x86_64": {
        "os": "macos",
        "cpu": "x86_64",
        "url": _BASE + "x86_64-apple-darwin",
        "sha256": "d5fa365a4920d19a61837a42c9273b0b8ec696fd3047af864a860f46005773a5",
        "binary": "wac",
    },
    "windows_x86_64": {
        "os": "windows",
        "cpu": "x86_64",
        "url": _BASE + "x86_64-pc-windows-gnu",
        "sha256": "b3509dfc3bb9d1e598e7b2790ef6efe5b6c8b696f2ad0e997e9ae6dd20bb6f13",
        "binary": "wac.exe",
    },
}

def _wac_platform_repo_impl(rctx):
    binary = rctx.attr.binary
    rctx.download(
        url = rctx.attr.url,
        sha256 = rctx.attr.sha256,
        output = "dist/" + binary,
        executable = True,
    )
    rctx.file(
        "BUILD.bazel",
        """load("@bazel_skylib//rules:native_binary.bzl", "native_binary")

native_binary(
    name = "wac",
    src = "dist/{binary}",
    out = "{binary}",
    visibility = ["//visibility:public"],
)
""".format(binary = binary),
    )

_wac_platform_repo = repository_rule(
    implementation = _wac_platform_repo_impl,
    attrs = {
        "binary": attr.string(mandatory = True),
        "sha256": attr.string(mandatory = True),
        "url": attr.string(mandatory = True),
    },
)

def _wac_hub_repo_impl(rctx):
    settings = []
    select_entries = []
    for key, spec in rctx.attr.targets.items():
        os_name, cpu_name = spec.split(":")
        settings.append("""config_setting(
    name = "{key}",
    constraint_values = [
        "@platforms//os:{os_name}",
        "@platforms//cpu:{cpu_name}",
    ],
)""".format(key = key, os_name = os_name, cpu_name = cpu_name))
        select_entries.append('        ":{key}": "@wac_{key}//:wac",'.format(key = key))

    rctx.file(
        "BUILD.bazel",
        """# Auto-generated selector over hermetic wac v{version} binaries.
{settings}

alias(
    name = "wac",
    actual = select({{
{select_entries}
    }}),
    visibility = ["//visibility:public"],
)
""".format(
            version = WAC_VERSION,
            settings = "\n\n".join(settings),
            select_entries = "\n".join(select_entries),
        ),
    )

_wac_hub_repo = repository_rule(
    implementation = _wac_hub_repo_impl,
    attrs = {
        "targets": attr.string_dict(mandatory = True),
    },
)

def _wac_impl(_mctx):
    target_constraints = {}
    for key, spec in _TARGETS.items():
        _wac_platform_repo(
            name = "wac_" + key,
            url = spec["url"],
            sha256 = spec["sha256"],
            binary = spec["binary"],
        )
        target_constraints[key] = spec["os"] + ":" + spec["cpu"]
    _wac_hub_repo(
        name = "wac",
        targets = target_constraints,
    )

wac = module_extension(implementation = _wac_impl)
