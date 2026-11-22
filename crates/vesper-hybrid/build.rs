//! Compile each swapped compiler module's Vesper package with the default
//! `vesper` executable and link it (vesper-port-harness), checking its
//! committed binding. See docs/parallel-results/self-host-hybrid-build.md.
//!
//! Bazel builds every module object with `vesper_port_object` and passes it
//! as `VESPER_HYBRID_OBJECT_<MODULE>` (and the generated binding as
//! `VESPER_HYBRID_BINDING_<MODULE>`), tools/bazel/vesper_hybrid.bzl. The
//! harness takes one prebuilt object per build script, so each module's is
//! handed to it in turn.
use vesper_port_harness::{GENERATED_BINDING_ENV, OBJECT_ENV, Port};

/// The swapped modules: (library, Vesper package, committed binding).
/// SH1+ lanes append theirs (recipe in the report).
const MODULES: &[(&str, &str, &str)] = &[
    ("hybrid_layout", "modules/layout", "port/layout.rs"),
    ("hybrid_parse", "modules/parse", "port/parse.rs"),
    // SH2 lane 5 (#1177): the whole layout and the IR check.
    (
        "hybrid_check_layout",
        "modules/check_layout",
        "port/check_layout.rs",
    ),
    ("hybrid_ir_check", "modules/ir_check", "port/ir_check.rs"),
    // SH2 lane 10 (#1182): the stack-bound report.
    (
        "hybrid_stack_bound",
        "modules/stack_bound",
        "port/stack_bound.rs",
    ),
    // SH4 lane 1 (#1275): the census lowering family, emitted through
    // lent Cranelift handles.
    ("hybrid_codegen", "modules/codegen", "port/codegen.rs"),
    // SH4 lane 12 (#1286): the export ABI v2 glue over the lent module.
    (
        "hybrid_codegen_export",
        "modules/codegen_export",
        "port/codegen_export.rs",
    ),
    // SH5 lane 14 (#1337): test discovery of `vesper test`.
    (
        "hybrid_test_runner",
        "modules/test_runner",
        "port/test_runner.rs",
    ),
    // SH3 lane 11 (#1250): the inliner, through `ir_swap`'s `inline` hook.
    ("hybrid_ir_inline", "modules/ir_inline", "port/ir_inline.rs"),
    // SH3 lane 10 (#1249): the AST optimizer's producer and checker,
    // through `optimize`'s table.
    ("hybrid_optimize", "modules/optimize", "port/optimize.rs"),
    // SH3 lane 9 (#1248): the IR optimizer, through `ir_swap`'s `opt` hook.
    ("hybrid_ir_opt", "modules/ir_opt", "port/ir_opt.rs"),
    // SH3 lane 1 (#1240): the IR swap point's identity round trip.
    ("hybrid_place_ir", "modules/place_ir", "port/place_ir.rs"),
    // SH3 lane 12 (#1251): bounds hoisting through the `hoist` hook.
    (
        "hybrid_bounds_hoist",
        "modules/bounds_hoist",
        "port/bounds_hoist.rs",
    ),
];

/// The divergence check of the differential harness: the enum tag word
/// takes two words, so every enum layout moves (mirrored in BUILD.bazel).
#[cfg(feature = "divergent")]
const DIVERGENT_EDIT: (&str, &str) = (
    "        8\n    } else {\n        0\n",
    "        16\n    } else {\n        0\n",
);

/// With the whole layout swapped (#1177) the pilot's word arithmetic runs
/// only under the Rust fallback, so the `divergent` build also lays out
/// every type from an empty input: the wrong type, with no ADTs.
#[cfg(feature = "divergent")]
const DIVERGENT_CHECK_LAYOUT_EDIT: (&str, &str) = (
    "    layout_words(in input)\n",
    "    layout_words(in input[0..0])\n",
);

/// The same divergence on the encode-once path (#1276): every part reads as
/// empty, so each scoped layout is of the wrong type, with no ADTs.
#[cfg(feature = "divergent")]
const DIVERGENT_SCOPED_LAYOUT_EDIT: (&str, &str) = (
    "    layout_part(inout s.c, in part)\n",
    "    layout_part(inout s.c, in part[0..0])\n",
);

/// The IR check's divergence check: every program gets one made-up E0410
/// (`divergent_words` in modules/ir_check), so every checked file fails.
#[cfg(feature = "divergent-ir-check")]
const DIVERGENT_IR_CHECK_EDIT: (&str, &str) = (
    "    ir_check_words(in input)\n",
    "    divergent_words(in input)\n",
);

/// The stack-bound divergence check: every graph is rejected as malformed
/// (`divergent_words` in modules/stack_bound), so every report differs.
#[cfg(feature = "divergent-stack-bound")]
const DIVERGENT_STACK_BOUND_EDIT: (&str, &str) = (
    "    stack_bound_words(in input)\n",
    "    divergent_words(in input)\n",
);

/// The same on the encode-once path (#1276).
#[cfg(feature = "divergent-ir-check")]
const DIVERGENT_SCOPED_IR_CHECK_EDIT: (&str, &str) = (
    "    ir_check_part(inout s.c, in part)\n",
    "    divergent_words(in part)\n",
);

/// The inliner's divergence check: an empty report reads back as "nothing
/// inlined" (`divergent_words` in modules/ir_inline), so optimized objects
/// keep the calls the default compiler inlines.
#[cfg(feature = "divergent-ir-inline")]
const DIVERGENT_IR_INLINE_EDIT: (&str, &str) = (
    "    inline_program_words(in input)\n",
    "    divergent_words(in input)\n",
);

/// The optimizer's divergence check: every island reads back as a barrier
/// (`divergent_words` in modules/optimize), so nothing is optimized and the
/// JIT and executables keep the arithmetic the default compiler folds.
#[cfg(feature = "divergent-optimize")]
const DIVERGENT_OPTIMIZE_EDIT: (&str, &str) = (
    "    optimize_propose_split(in input, in names)\n",
    "    divergent_words(in input, in names)\n",
);

/// The IR optimizer's divergence check: every program reads back as
/// "read lacks state evidence" (`divergent_words` in modules/ir_opt), so
/// every compile that optimizes fails.
#[cfg(feature = "divergent-ir-opt")]
const DIVERGENT_IR_OPT_EDIT: (&str, &str) = (
    "    ir_opt_words(in input)\n",
    "    divergent_words(in input)\n",
);

/// The parse module's divergence check: every AST flag (`let mut`, `pub`,
/// `@fip`, named fields, ...) reads back inverted (mirrored in BUILD.bazel).
#[cfg(feature = "divergent-parse")]
const DIVERGENT_PARSE_EDIT: (&str, &str) = (
    "    if b {\n        1\n    } else {\n        0\n    }",
    "    if b {\n        0\n    } else {\n        1\n    }",
);

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for (library, package, binding) in MODULES {
        let key = library.to_ascii_uppercase();
        let object = format!("VESPER_HYBRID_OBJECT_{key}");
        let generated = format!("VESPER_HYBRID_BINDING_{key}");
        println!("cargo:rerun-if-env-changed={object}");
        println!("cargo:rerun-if-env-changed={generated}");
        // SAFETY: build scripts are single-threaded; the harness reads
        // these right after.
        unsafe {
            match std::env::var_os(&object) {
                Some(path) => std::env::set_var(OBJECT_ENV, path),
                None => std::env::remove_var(OBJECT_ENV),
            }
            match std::env::var_os(&generated) {
                Some(path) => std::env::set_var(GENERATED_BINDING_ENV, path),
                None => std::env::remove_var(GENERATED_BINDING_ENV),
            }
        }
        let port = Port::new(library, package).binding(binding);
        #[cfg(feature = "divergent")]
        let port = if *library == "hybrid_layout" {
            port.edit(DIVERGENT_EDIT.0, DIVERGENT_EDIT.1)
        } else {
            port
        };
        #[cfg(feature = "divergent")]
        let port = if *library == "hybrid_check_layout" {
            port.edit(DIVERGENT_CHECK_LAYOUT_EDIT.0, DIVERGENT_CHECK_LAYOUT_EDIT.1)
                .edit(
                    DIVERGENT_SCOPED_LAYOUT_EDIT.0,
                    DIVERGENT_SCOPED_LAYOUT_EDIT.1,
                )
        } else {
            port
        };
        #[cfg(feature = "divergent-ir-check")]
        let port = if *library == "hybrid_ir_check" {
            port.edit(DIVERGENT_IR_CHECK_EDIT.0, DIVERGENT_IR_CHECK_EDIT.1)
                .edit(
                    DIVERGENT_SCOPED_IR_CHECK_EDIT.0,
                    DIVERGENT_SCOPED_IR_CHECK_EDIT.1,
                )
        } else {
            port
        };
        #[cfg(feature = "divergent-stack-bound")]
        let port = if *library == "hybrid_stack_bound" {
            port.edit(DIVERGENT_STACK_BOUND_EDIT.0, DIVERGENT_STACK_BOUND_EDIT.1)
        } else {
            port
        };
        #[cfg(feature = "divergent-ir-inline")]
        let port = if *library == "hybrid_ir_inline" {
            port.edit(DIVERGENT_IR_INLINE_EDIT.0, DIVERGENT_IR_INLINE_EDIT.1)
        } else {
            port
        };
        #[cfg(feature = "divergent-optimize")]
        let port = if *library == "hybrid_optimize" {
            port.edit(DIVERGENT_OPTIMIZE_EDIT.0, DIVERGENT_OPTIMIZE_EDIT.1)
        } else {
            port
        };
        #[cfg(feature = "divergent-ir-opt")]
        let port = if *library == "hybrid_ir_opt" {
            port.edit(DIVERGENT_IR_OPT_EDIT.0, DIVERGENT_IR_OPT_EDIT.1)
        } else {
            port
        };
        #[cfg(feature = "divergent-parse")]
        let port = if *library == "hybrid_parse" {
            port.edit(DIVERGENT_PARSE_EDIT.0, DIVERGENT_PARSE_EDIT.1)
        } else {
            port
        };
        port.build();
    }
}
