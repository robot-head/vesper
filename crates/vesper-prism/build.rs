//! Compile the five Vesper libraries of the port (the Loom packages under
//! vesper/) into static libraries and check their committed bindings (see
//! docs/maintenance/porting-a-crate.md). Under Bazel each object is prebuilt
//! (`VESPER_PORT_OBJECT_<LIBRARY>`).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for (library, types) in LIBRARIES {
        let port = vesper_port_harness::Port::new(library, format!("vesper/{library}"))
            .binding(format!("port/{library}.rs"))
            .rust_types(types);
        // The divergence check: one deliberate change per library, which the
        // tests of that module must catch (CI asserts that they fail).
        #[cfg(feature = "vesper-port-divergent")]
        let port = DIVERGENT
            .iter()
            .filter(|(lib, _, _)| *lib == library)
            .fold(port, |port, (_, from, to)| port.edit(from, to));
        port.build();
    }
}

/// Each library and the Rust module that holds its boundary types.
const LIBRARIES: [(&str, &str); 5] = [
    ("decoder", "crate::forward::decoder::abi"),
    ("response", "crate::forward::response::abi"),
    ("target", "crate::forward::target::abi"),
    ("router", "crate::forward::router::abi"),
    ("connection", "crate::forward::connection::abi"),
];

/// The divergence edits, one per library (mirrored in BUILD.bazel).
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: [(&str, &str, &str); 5] = [
    // tests/framing.rs: Content-Length may exceed the body limit by one.
    (
        "decoder",
        "if length > limits.max_body_bytes {",
        "if length > wrapping_add(limits.max_body_bytes, 1) {",
    ),
    // tests/responses.rs: 205 Reset Content may carry a body.
    (
        "response",
        "status == NO_CONTENT || status == RESET_CONTENT || status == NOT_MODIFIED",
        "status == NO_CONTENT || status == NOT_MODIFIED",
    ),
    // tests/routing.rs: `+` in a query stays a plus.
    (
        "target",
        "if plus_is_space && b == 43 {",
        "if plus_is_space && b == 44 {",
    ),
    // tests/routing.rs: a parameter beats a static segment.
    (
        "router",
        "let child = static_child(in r, node, in segments, lo, hi);",
        "let child = if param_child == none() {\n        static_child(in r, node, in segments, lo, hi)\n    } else {\n        none()\n    };",
    ),
    // tests/connection.rs: an out-of-order completion reserves no room.
    (
        "connection",
        "let reserve = if index == 0 {",
        "let reserve = if index < 64 {",
    ),
];
