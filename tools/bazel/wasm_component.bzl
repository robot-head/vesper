"""`wasm_component`: a core Wasm module wrapped into a component (#2210).

Bazel links `wasm32-wasip2` with plain `rust-lld`, which writes a core
module; Cargo's `wasm-component-ld` also runs wit-component over the
module's `component-type` sections. `tool` does that step
(crates/vesper-cranelift-component/bazel/componentize.rs).
"""

def _wasm_component_impl(ctx):
    core = ctx.file.core
    out = ctx.actions.declare_file(ctx.attr.out)
    ctx.actions.run(
        executable = ctx.executable.tool,
        arguments = [core.path, out.path],
        inputs = [core],
        outputs = [out],
        mnemonic = "WasmComponent",
        progress_message = "Componentizing %{label}",
    )
    return [DefaultInfo(files = depset([out]), runfiles = ctx.runfiles([out]))]

wasm_component = rule(
    implementation = _wasm_component_impl,
    attrs = {
        "core": attr.label(
            doc = "The core module (a cdylib built for wasm32-wasip2).",
            allow_single_file = [".wasm"],
            mandatory = True,
        ),
        "out": attr.string(
            doc = "The component's file name.",
            mandatory = True,
        ),
        "tool": attr.label(
            doc = "The componentize binary: `tool CORE OUT`.",
            executable = True,
            cfg = "exec",
            mandatory = True,
        ),
    },
)
