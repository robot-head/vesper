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
];

/// The divergence check of the differential harness: the enum tag word
/// takes two words, so every enum layout moves (mirrored in BUILD.bazel).
#[cfg(feature = "divergent")]
const DIVERGENT_EDIT: (&str, &str) = (
    "        8\n    } else {\n        0\n",
    "        16\n    } else {\n        0\n",
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
        #[cfg(feature = "divergent-parse")]
        let port = if *library == "hybrid_parse" {
            port.edit(DIVERGENT_PARSE_EDIT.0, DIVERGENT_PARSE_EDIT.1)
        } else {
            port
        };
        port.build();
    }
}
