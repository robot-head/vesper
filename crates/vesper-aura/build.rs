//! Compile the Loom package in vesper/ into a static library and check the
//! committed binding (see docs/maintenance/porting-a-crate.md). Under Bazel the
//! object is prebuilt (VESPER_PORT_OBJECT).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("aura", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = EDITS
        .iter()
        .fold(port, |port, (from, to)| port.edit(from, to));
    port.build();
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel
/// and asserted by scripts/test-port-harness.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // view.rs (validate): one child too few is accepted.
    (
        "if node.ccount > lim.children {",
        "if node.ccount > inc(lim.children) {",
    ),
    // plan.rs: a child move reports its positions swapped.
    (
        "op(op_move_child(), node.id, child_id, from, index)",
        "op(op_move_child(), node.id, child_id, index, from)",
    ),
    // session.rs: a stale plan passes the session's revision check.
    (
        "    } else if p.base != r.revision {",
        "    } else if p.base == max_u64() {",
    ),
    // recording.rs: an injected rejection fires one patch late.
    (
        "if has_reject && reject == i {",
        "if has_reject && reject == inc(i) {",
    ),
    // sim_aura.rs: Inputs drop out of the focus order.
    (
        "                if kind == k_button() || kind == k_input() {\n                    q.items.push(id);",
        "                if kind == k_button() {\n                    q.items.push(id);",
    ),
];
