//! Under `vesper-port`, link the Vesper port in vesper/ and set
//! cfg(vesper_port); see "Crates the compiler links" in
//! docs/maintenance/porting-a-crate.md. Without it this package builds the
//! Rust code, like the seed package (seed/), which has no build script.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The harness sets the cfg under `vesper-port`; declare it either way.
    println!("cargo:rustc-check-cfg=cfg({})", vesper_port_harness::CFG);
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("ebpf", "vesper")
            .binding("port/binding.rs")
            .rust_types("crate::port");
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            // The edits in VESPER_EBPF_DIVERGENT (comma-separated indices
            // into EDITS; port/check.sh groups edits that no test sees
            // together, so each group is one build); unset applies every
            // edit.
            println!("cargo:rerun-if-env-changed=VESPER_EBPF_DIVERGENT");
            let only: Option<Vec<usize>> = std::env::var("VESPER_EBPF_DIVERGENT").ok().map(|v| {
                v.split(',')
                    .map(|i| {
                        i.trim()
                            .parse::<usize>()
                            .expect("VESPER_EBPF_DIVERGENT is a list of indices")
                    })
                    .collect()
            });
            EDITS
                .iter()
                .enumerate()
                .filter(|(i, _)| only.as_ref().is_none_or(|only| only.contains(i)))
                .fold(port, |port, (_, (from, to))| port.edit(from, to))
        };
        port.build();
    }
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel
/// and asserted by crates/vesper-ebpf/port/check.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // 0 insn.rs: `lsh` encodes as `rsh`.
    (
        "        AluOp::Lsh => (OP_LSH, 0),\n",
        "        AluOp::Lsh => (OP_RSH, 0),\n",
    ),
    // 1 scalar.rs: tnum addition forgets the second operand's unknown bits.
    (
        "    let mu = chi | a.mask | b.mask;\n    tn(sv & ~mu, mu)",
        "    let mu = chi | a.mask;\n    tn(sv & ~mu, mu)",
    ),
    // 2 interp.rs: a 64-bit arithmetic shift is logical.
    (
        "        AluOp::Arsh => sar(a, b & 63),\n",
        "        AluOp::Arsh => shr(a, b & 63),\n",
    ),
    // 3 verifier.rs: refining `a > b` claims `a > b + 1` (unsound).
    (
        "            x.umin = max_u64(x.umin, wrapping_add(y.umin, 1));\n",
        "            x.umin = max_u64(x.umin, wrapping_add(y.umin, 2));\n",
    ),
    // 4 helpers.rs: tc programs may call bpf_xdp_adjust_head.
    (
        "        ProgramType::SchedCls => id == SK_LOOKUP_TCP || id == SK_RELEASE,\n",
        "        ProgramType::SchedCls => id == XDP_ADJUST_HEAD || id == SK_LOOKUP_TCP || id == SK_RELEASE,\n",
    ),
    // 5 program.rs: a ringbuf of 2048 bytes is accepted.
    (
        "            if max_entries < 4096 ||",
        "            if max_entries < 2048 ||",
    ),
    // 6 callgraph.rs: a path may exceed the stack limit by one frame slot.
    (
        "        if bytes > lim {\n",
        "        if bytes > wrapping_add(lim, 8) {\n",
    ),
    // 7 wire.rs: the decoder reads the source address from the destination.
    (
        "        src_ipv4: wrapping_u32(be(in s, 26, 4)),\n",
        "        src_ipv4: wrapping_u32(be(in s, 30, 4)),\n",
    ),
    // 8 verifier.rs (the exploration): a packet access may end one byte
    // past the range established against data_end (unsound).
    (
        "            if p.var.smin < 0 || total < 0 || wrapping_add(total, ni) > range {\n",
        "            if p.var.smin < 0 || total < 0 || wrapping_add(total, ni) > wrapping_add(range, 1) {\n",
    ),
    // 9 btf.rs: an unknown kind 20 is read as a pointer.
    (
        "        if kind == 0 || kind > 19 {\n",
        "        if kind == 0 || kind > 20 {\n",
    ),
    // 10 core.rs: members are not found through anonymous aggregates.
    (
        "        if slen(in strs, mname) == 0 {\n",
        "        if slen(in strs, mname) == 99 {\n",
    ),
    // 11 btf_ext.rs: line info keeps 9 bits of the column, not 10.
    (
        "        out.push(lc & 1023);\n",
        "        out.push(lc & 511);\n",
    ),
    // 12 interp.rs (the run): the byte just before `data` is readable.
    (
        "    if kind == KIND_PACKET && at < i.data_off {\n",
        "    if kind == KIND_PACKET && wrapping_add(at, 1) < i.data_off {\n",
    ),
];
