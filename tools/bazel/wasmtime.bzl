"""Hermetic prebuilt `wasmtime` CLI repository extension for Stage 2.

Pins official `bytecodealliance/wasmtime` release archives (`v50.0.1`,
matching `Cargo.toml`'s `wasmtime = "50"`) by SHA-256 across all CI and
developer OS/CPU combinations, exposing `@wasmtime//:wasmtime` via `select`.
"""

WASMTIME_VERSION = "50.0.1"

_TARGETS = {
    "linux_x86_64": {
        "os": "linux",
        "cpu": "x86_64",
        "url": "https://github.com/bytecodealliance/wasmtime/releases/download/v50.0.1/wasmtime-v50.0.1-x86_64-linux.tar.xz",
        "sha256": "685466455dd42a14774a86c36c8f2e45cbb9bd6d92a850139315df60a75498a5",
        "strip_prefix": "wasmtime-v50.0.1-x86_64-linux",
        "binary": "wasmtime",
    },
    "linux_aarch64": {
        "os": "linux",
        "cpu": "aarch64",
        "url": "https://github.com/bytecodealliance/wasmtime/releases/download/v50.0.1/wasmtime-v50.0.1-aarch64-linux.tar.xz",
        "sha256": "d737e077416d78fbfb0a008229ec2d6a0fd5a4924874b8c38b293c850cf4094",
        "strip_prefix": "wasmtime-v50.0.1-aarch64-linux",
        "binary": "wasmtime",
    },
    "macos_aarch64": {
        "os": "macos",
        "cpu": "aarch64",
        "url": "https://github.com/bytecodealliance/wasmtime/releases/download/v50.0.1/wasmtime-v50.0.1-aarch64-macos.tar.xz",
        "sha256": "960e5216c7ebd7e9b45e6561d15821b8b801b7d54aee28c7b91921aeb30c65c8",
        "strip_prefix": "wasmtime-v50.0.1-aarch64-macos",
        "binary": "wasmtime",
    },
    "macos_x86_64": {
        "os": "macos",
        "cpu": "x86_64",
        "url": "https://github.com/bytecodealliance/wasmtime/releases/download/v50.0.1/wasmtime-v50.0.1-x86_64-macos.tar.xz",
        "sha256": "ea1818fa8cbb51524847ab2ad22197e0051594fdc96cbd0cc2afb65832e11925",
        "strip_prefix": "wasmtime-v50.0.1-x86_64-macos",
        "binary": "wasmtime",
    },
    "windows_x86_64": {
        "os": "windows",
        "cpu": "x86_64",
        "url": "https://github.com/bytecodealliance/wasmtime/releases/download/v50.0.1/wasmtime-v50.0.1-x86_64-windows.zip",
        "sha256": "c3646ee2de8cfcefe532e75852e1fd2728d78c3fcc43251794803a698d1c30ca",
        "strip_prefix": "wasmtime-v50.0.1-x86_64-windows",
        "binary": "wasmtime.exe",
    },
    "windows_aarch64": {
        "os": "windows",
        "cpu": "aarch64",
        "url": "https://github.com/bytecodealliance/wasmtime/releases/download/v50.0.1/wasmtime-v50.0.1-aarch64-windows.zip",
        "sha256": "2fe15ad13e1ed2f975ff530a335aa6cc8cd63548517321fc9e96ab1dec9db046",
        "strip_prefix": "wasmtime-v50.0.1-aarch64-windows",
        "binary": "wasmtime.exe",
    },
}

def _wasmtime_platform_repo_impl(rctx):
    rctx.download_and_extract(
        url = rctx.attr.url,
        sha256 = rctx.attr.sha256,
        stripPrefix = rctx.attr.strip_prefix,
        output = "dist",
    )
    binary = rctx.attr.binary
    rctx.file(
        "BUILD.bazel",
        """load("@bazel_skylib//rules:native_binary.bzl", "native_binary")

native_binary(
    name = "wasmtime",
    src = "dist/{binary}",
    out = "{binary}",
    visibility = ["//visibility:public"],
)
""".format(binary = binary),
    )

_wasmtime_platform_repo = repository_rule(
    implementation = _wasmtime_platform_repo_impl,
    attrs = {
        "binary": attr.string(mandatory = True),
        "sha256": attr.string(mandatory = True),
        "strip_prefix": attr.string(mandatory = True),
        "url": attr.string(mandatory = True),
    },
)

def _wasmtime_hub_repo_impl(rctx):
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
        select_entries.append('        ":{key}": "@wasmtime_{key}//:wasmtime",'.format(key = key))

    rctx.file(
        "BUILD.bazel",
        """# Auto-generated selector over hermetic wasmtime v{version} archives.
{settings}

alias(
    name = "wasmtime",
    actual = select({{
{select_entries}
    }}),
    visibility = ["//visibility:public"],
)
""".format(
            version = WASMTIME_VERSION,
            settings = "\n\n".join(settings),
            select_entries = "\n".join(select_entries),
        ),
    )

_wasmtime_hub_repo = repository_rule(
    implementation = _wasmtime_hub_repo_impl,
    attrs = {
        "targets": attr.string_dict(mandatory = True),
    },
)

def _wasmtime_impl(_mctx):
    target_constraints = {}
    for key, spec in _TARGETS.items():
        _wasmtime_platform_repo(
            name = "wasmtime_" + key,
            url = spec["url"],
            sha256 = spec["sha256"],
            strip_prefix = spec["strip_prefix"],
            binary = spec["binary"],
        )
        target_constraints[key] = spec["os"] + ":" + spec["cpu"]
    _wasmtime_hub_repo(
        name = "wasmtime",
        targets = target_constraints,
    )

wasmtime = module_extension(implementation = _wasmtime_impl)
