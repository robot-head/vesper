"""`stage2_port_builder`: the Stage 2-built port builder (#2666).

packages/vesper-port-builder (`vesper build --lib`'s interface, binding and
object for one port) built by Stage 2, with no Rust compiler, then composed
with the Cranelift component so the pinned wasmtime CLI runs it alone:

1. The committed seed (bootstrap/stage2-seed, #2671) builds today's
   @vesper/stage2 (stage A).
2. Stage A builds @vesper/stage2 again (stage B). Stage A's own code comes
   from the seed's backend; stage B's comes from today's, which frees the
   heap when a function returns a scalar, so stage B builds the port
   builder in about 2.2 GiB where stage A needs about 3.6 GiB.
3. Stage B builds @vesper/port-builder into a component that imports
   `vesper:cranelift/vcl`.
4. `wac plug` links //crates/vesper-cranelift-component:component into it.

Stage 2 reads its sources through a WASI preopen, which refuses the absolute
symlinks Bazel stages inputs as, so the packages are first copied into a
tree of regular files (`expand_template` writes a copy). The harness and
vesper_port.bzl run the result (`--config=stage2-ports`).
"""

_STAGE2_ARGV0 = "stage2"
_ENTRY = "main"
_STAGE2_PACKAGE = "packages/vesper-stage2"
_BUILDER_PACKAGE = "packages/vesper-port-builder"

# The files Stage 2 reads: sources, manifests and `.vsys` libraries (all
# text, which `expand_template` copies byte for byte).
_READ = [".ves", ".toml", ".vsys"]

def _copy_tree(ctx, root):
    """Each package file as a regular file under `root`, by its repository path."""
    copies = []
    for f in ctx.files.packages:
        if not any([f.basename.endswith(e) for e in _READ]):
            continue
        path = f.short_path
        if not path.startswith("packages/"):
            fail("%s is not under packages/" % path)
        copy = ctx.actions.declare_file(root + path)
        ctx.actions.expand_template(template = f, output = copy, substitutions = {})
        copies.append(copy)
    return copies

def _stage2_run(ctx, compiler, tree_dir, tree, package, output, mnemonic, message):
    # The tree is preopened as the guest's working directory, and the
    # output's directory as /out (Stage 2 writes sidecars beside it).
    args = ctx.actions.args()
    args.add("run")
    args.add("--dir", tree_dir + "::.")
    args.add("--dir", output.dirname + "::/out")
    args.add("--argv0", _STAGE2_ARGV0)
    args.add(compiler)
    args.add(package)
    args.add("/out/" + output.basename)
    args.add(_ENTRY)
    ctx.actions.run(
        executable = ctx.executable._wasmtime,
        arguments = [args],
        inputs = tree + [compiler],
        outputs = [output],
        mnemonic = mnemonic,
        progress_message = message,
    )

def _stage2_port_builder_impl(ctx):
    root = ctx.label.name + "_tree/"
    tree = _copy_tree(ctx, root)
    tree_dir = ctx.bin_dir.path + "/" + (ctx.label.package + "/" if ctx.label.package else "") + root.rstrip("/")
    stage_a = ctx.actions.declare_file(ctx.label.name + "_stage2.wasm")
    _stage2_run(ctx, ctx.file.seed, tree_dir, tree, _STAGE2_PACKAGE, stage_a, "Stage2Self", "Building Stage 2 with the seed")
    stage_b = ctx.actions.declare_file(ctx.label.name + "_stage2b.wasm")
    _stage2_run(ctx, stage_a, tree_dir, tree, _STAGE2_PACKAGE, stage_b, "Stage2SelfB", "Building Stage 2 with itself")
    core = ctx.actions.declare_file(ctx.label.name + "_core.wasm")
    _stage2_run(ctx, stage_b, tree_dir, tree, _BUILDER_PACKAGE, core, "Stage2PortBuilder", "Building the port builder with Stage 2")
    out = ctx.outputs.out
    plug = ctx.actions.args()
    plug.add("plug", core)
    cranelift = [f for f in ctx.files.cranelift if f.extension == "wasm"]
    if len(cranelift) != 1:
        fail("cranelift must provide one .wasm component")
    plug.add("--plug", cranelift[0])
    plug.add("-o", out)
    ctx.actions.run(
        executable = ctx.executable._wac,
        arguments = [plug],
        inputs = [core, cranelift[0]],
        outputs = [out],
        mnemonic = "WacPlug",
        progress_message = "Composing the port builder with the Cranelift component",
    )
    return [DefaultInfo(files = depset([out]))]

stage2_port_builder = rule(
    implementation = _stage2_port_builder_impl,
    doc = "packages/vesper-port-builder built by Stage 2 and composed with the Cranelift component.",
    attrs = {
        "cranelift": attr.label(
            allow_files = True,
            mandatory = True,
            doc = "//crates/vesper-cranelift-component:component.",
        ),
        "out": attr.output(mandatory = True, doc = "The composed component (`//:vesper-port-builder.wasm`)."),
        "packages": attr.label_list(
            allow_files = True,
            mandatory = True,
            doc = "Every package file Stage 2 and the port builder read (under packages/).",
        ),
        "seed": attr.label(
            allow_single_file = [".wasm"],
            mandatory = True,
            doc = "The committed Stage 2 seed.",
        ),
        "_wac": attr.label(
            default = "@wac//:wac",
            executable = True,
            cfg = "exec",
        ),
        "_wasmtime": attr.label(
            default = "@wasmtime//:wasmtime",
            executable = True,
            cfg = "exec",
        ),
    },
)
