//! CO-RE beyond byte offsets: bitfield relocations (byte offset, load
//! size, shifts), `TYPE_ID_LOCAL`/`TYPE_ID_TARGET`, `TYPE_MATCHES`, and
//! candidate search in split module BTF. Values are checked against
//! hand-computed layouts, against libbpf's rules on the host vmlinux, and
//! (where the host loads programs) against the kernel's own CO-RE
//! relocation of the same records.

use vesper_ebpf::btf::{Btf, BtfBuilder};
use vesper_ebpf::core::{CoreKind, CoreSpec, LocalType, apply, apply_with_modules};
use vesper_ebpf::elf::{read_object, write_object};
use vesper_ebpf::insn::{AluOp, JmpOp, Reg, Size};
use vesper_ebpf::interp::{Env, run_xdp};
use vesper_ebpf::{Asm, Program, ProgramType, VerifierPolicy, verify};

fn u16t() -> LocalType {
    LocalType::int("short unsigned int", 2, false)
}

fn u32t() -> LocalType {
    LocalType::int("unsigned int", 4, false)
}

/// A local `struct tcphdr` naming only `source`, `doff` and `syn`, at
/// deliberately wrong positions.
fn local_tcphdr() -> LocalType {
    LocalType::structure_bits(
        "tcphdr",
        20,
        vec![
            ("source", u16t(), 0, 0),
            ("doff", u16t(), 16, 4),
            ("syn", u16t(), 24, 1),
        ],
    )
}

/// `r0 = <relocated value of spec>; exit`, as an XDP program.
fn value_prog(spec: CoreSpec) -> Program {
    let mut a = Asm::new();
    a.core(Reg::R0, spec).unwrap();
    a.exit();
    a.finish("core_value", ProgramType::Xdp).unwrap()
}

fn field(kind: CoreKind, root: LocalType, path: &[&str]) -> CoreSpec {
    CoreSpec::field(kind, root, path).unwrap()
}

/// Target: `struct vflags { u16 lo; u32 a:3, b:9, c:8; u16 d; }` laid
/// out by hand (a at bit 16, b at bit 19, c at bit 28, d at bit 48).
fn bitfield_target() -> Btf {
    let mut b = BtfBuilder::new();
    let u32 = b.int("unsigned int", 4, false);
    let u16 = b.int("short unsigned int", 2, false);
    b.structure_bits(
        "vflags",
        8,
        &[
            ("lo", u16, 0, 0),
            ("a", u32, 16, 3),
            ("b", u32, 19, 9),
            ("c", u32, 28, 8),
            ("d", u16, 48, 0),
        ],
    );
    Btf::parse(&b.finish()).unwrap()
}

fn local_vflags() -> LocalType {
    LocalType::structure_bits(
        "vflags",
        8,
        vec![
            ("a", u32t(), 0, 3),
            ("b", u32t(), 3, 13),
            ("c", u32t(), 16, 16),
            ("d", u16t(), 32, 0),
        ],
    )
}

/// Extract a bitfield the way `BPF_CORE_READ_BITFIELD` does.
fn extract(mem: &[u8], off: u64, size: u64, lshift: u64, rshift: u64) -> u64 {
    let mut v = [0u8; 8];
    v[..size as usize].copy_from_slice(&mem[off as usize..(off + size) as usize]);
    (u64::from_le_bytes(v) << lshift) >> rshift
}

#[test]
fn bitfields_relocate_to_libbpf_load_sizes_and_shifts() {
    let t = bitfield_target();
    let l = local_vflags();
    let v = |k, m| field(k, l.clone(), &[m]).target_value(&t).unwrap();
    use CoreKind::*;
    // (member, byte offset, load size, lshift, rshift)
    for (m, off, size, lsh, rsh) in [
        // bits 16..19 and 19..28 fit the u32 at byte 0
        ("a", 0, 4, 64 - 19, 64 - 3),
        ("b", 0, 4, 64 - 28, 64 - 9),
        // bits 28..36 cross the u32: the load doubles to 8 bytes
        ("c", 0, 8, 64 - 36, 64 - 8),
        // a plain member
        ("d", 6, 2, 48, 48),
    ] {
        assert_eq!(
            (
                v(FieldByteOffset, m),
                v(FieldByteSize, m),
                v(FieldLshiftU64, m),
                v(FieldRshiftU64, m)
            ),
            (off, size, lsh, rsh),
            "{m}"
        );
        assert_eq!(v(FieldSigned, m), 0, "{m}");
        assert_eq!(v(FieldExists, m), 1, "{m}");
    }
    // the shifts recover each value from memory laid out as the target
    let (lo, a, b, c, d) = (0x1234u64, 5u64, 0x1ab, 0xcd, 0xbeef);
    let mem = (lo | a << 16 | b << 19 | c << 28 | d << 48).to_le_bytes();
    for (m, want) in [("a", a), ("b", b), ("c", c), ("d", d)] {
        let got = extract(
            &mem,
            v(FieldByteOffset, m),
            v(FieldByteSize, m),
            v(FieldLshiftU64, m),
            v(FieldRshiftU64, m),
        );
        assert_eq!(got, want, "{m}");
    }
    // local values follow the local layout
    let lv = |k, m| field(k, l.clone(), &[m]).local_value().unwrap();
    assert_eq!(
        (lv(FieldByteOffset, "c"), lv(FieldByteSize, "c")),
        (0, 4),
        "c local"
    );
    assert_eq!(lv(FieldLshiftU64, "c"), 64 - 32);
}

#[test]
fn signed_bitfields_and_wide_bitfields() {
    let mut b = BtfBuilder::new();
    let s8 = b.int("signed char", 1, true);
    let u64 = b.int("long long unsigned int", 8, false);
    b.structure_bits("w", 16, &[("s", s8, 6, 4), ("x", u64, 60, 8)]);
    let t = Btf::parse(&b.finish()).unwrap();
    let l = LocalType::structure_bits(
        "w",
        16,
        vec![
            ("s", LocalType::int("signed char", 1, true), 0, 4),
            (
                "x",
                LocalType::int("long long unsigned int", 8, false),
                4,
                8,
            ),
        ],
    );
    let v = |k, m| field(k, l.clone(), &[m]).target_value(&t).unwrap();
    // bits 6..10 do not fit a byte: the load becomes a 2-byte one
    assert_eq!(v(CoreKind::FieldByteSize, "s"), 2);
    assert_eq!(v(CoreKind::FieldSigned, "s"), 1);
    // bits 60..68 cross an 8-byte boundary: no load covers it
    let e = field(CoreKind::FieldByteSize, l, &["x"])
        .target_value(&t)
        .unwrap_err();
    assert!(e.to_string().contains("bitfield"), "{e}");
}

#[test]
fn type_ids_are_the_object_btf_ids_and_the_target_ids() {
    let t = bitfield_target();
    let mut a = Asm::new();
    a.core(
        Reg::R1,
        CoreSpec::of_type(CoreKind::TypeIdLocal, local_vflags()).unwrap(),
    )
    .unwrap();
    a.core(
        Reg::R2,
        CoreSpec::of_type(CoreKind::TypeIdTarget, local_vflags()).unwrap(),
    )
    .unwrap();
    a.mov64(Reg::R0, 0);
    a.exit();
    let p = a.finish("ids", ProgramType::Xdp).unwrap();
    let ids = vesper_ebpf::btf_ext::local_root_ids(&p).unwrap();
    assert_eq!(ids[0], ids[1]);
    assert!(ids[0] > 0);
    // before relocation both hold the program's own id
    assert_eq!(
        (p.insns[0].imm, p.insns[1].imm),
        (ids[0] as i32, ids[0] as i32)
    );
    // the id really names the local struct in the emitted BTF
    let ob = vesper_ebpf::btf_ext::build(&p, "xdp").unwrap();
    let own = Btf::parse(&ob.btf).unwrap();
    assert_eq!(own.by_name("vflags"), &[ids[0]]);
    let local = vesper_ebpf::btf_ext::local_type(&own, ids[0]).unwrap();
    assert_eq!(local, local_vflags(), "bitfields round-trip through BTF");
    // relocated: LOCAL keeps the local id, TARGET becomes vflags' id
    let r = apply(&p, &t).unwrap();
    assert_eq!(r.insns[0].imm as u32, ids[0]);
    assert_eq!(r.insns[1].imm as u32, t.by_name("vflags")[0]);
    // and the object round trip keeps the records
    let back = read_object(&write_object(&p).unwrap()).unwrap();
    assert_eq!(back.core_relos, p.core_relos);
    assert_eq!(apply(&back, &t).unwrap().insns, r.insns);
}

#[test]
fn type_matches_follows_libbpf() {
    let mut b = BtfBuilder::new();
    let u32 = b.int("unsigned int", 4, false);
    let s32 = b.int("int", 4, true);
    let fwd_id = {
        // struct other; (forward declared) behind a pointer
        let n = b.str("other");
        b.header(n, 7, 0, false, 0)
    };
    let p_other = b.ptr(fwd_id);
    b.structure(
        "m",
        false,
        16,
        &[("x", u32, 0), ("y", s32, 32), ("o", p_other, 64)],
    );
    b.enumeration("e", 4, &[("A", 0), ("B", 1), ("C", 2)]);
    let t = Btf::parse(&b.finish()).unwrap();
    let other = LocalType::structure("other", 4, vec![("z", u32t(), 0)]);
    let m = |members: Vec<(&str, LocalType, u32)>| LocalType::structure("m", 16, members);
    let matches = |l: LocalType| {
        CoreSpec::of_type(CoreKind::TypeMatches, l)
            .unwrap()
            .target_value(&t)
            .unwrap()
    };
    // a subset of members, in any order, at any offset
    assert_eq!(
        matches(m(vec![("y", LocalType::int("int", 4, true), 0)])),
        1
    );
    assert_eq!(
        matches(m(vec![
            ("o", LocalType::Ptr(Box::new(other)), 0),
            ("x", u32t(), 8)
        ])),
        1
    );
    // signedness, size and missing members do not match
    assert_eq!(matches(m(vec![("y", u32t(), 0)])), 0);
    assert_eq!(
        matches(m(vec![("x", LocalType::int("unsigned int", 8, false), 0)])),
        0
    );
    assert_eq!(matches(m(vec![("w", u32t(), 0)])), 0);
    // enums: every local enumerator must exist (values may differ)
    let e = |vs: &[(&str, i64)]| LocalType::enumeration("e", 4, vs);
    assert_eq!(matches(e(&[("C", 9), ("A", 0)])), 1);
    assert_eq!(matches(e(&[("D", 3)])), 0);
    // no candidate at all: 0, like TYPE_EXISTS
    assert_eq!(
        matches(LocalType::structure("absent", 4, vec![("x", u32t(), 0)])),
        0
    );
    // bitfield widths are not compared (as in libbpf and the kernel)
    assert_eq!(
        matches(LocalType::structure_bits(
            "m",
            16,
            vec![("x", u32t(), 0, 3)]
        )),
        1
    );
}

/// vmlinux-like base and a module on top of it defining `vmod_ctx` (which
/// the base lacks) and its own `vflags` (which the base has).
fn base_and_module() -> (Btf, Btf) {
    let base = bitfield_target();
    let mut m = BtfBuilder::split(&base);
    let u64 = m.int("long long unsigned int", 8, false);
    let u32 = base.by_name("unsigned int")[0];
    m.structure(
        "vmod_ctx",
        false,
        24,
        &[("cookie", u64, 64), ("len", u32, 128)],
    );
    m.structure("vflags", false, 4, &[("a", u32, 0)]);
    let bytes = m.finish();
    let module = Btf::parse_split(&bytes, &base).unwrap();
    (base, module)
}

#[test]
fn module_btf_is_searched_only_when_vmlinux_has_no_candidate() {
    let (base, module) = base_and_module();
    assert_eq!(module.base_len() as usize, base.len());
    assert_eq!(module.len(), base.len() + 3);
    // ids continue after the base, base types stay visible by name
    let ctx = module.by_name("vmod_ctx")[0];
    assert!(ctx >= module.base_len());
    assert_eq!(module.by_name("vflags").len(), 2);
    let l = LocalType::structure(
        "vmod_ctx",
        8,
        vec![
            ("len", u32t(), 0),
            (
                "cookie",
                LocalType::int("long long unsigned int", 8, false),
                4,
            ),
        ],
    );
    let mut a = Asm::new();
    a.core(
        Reg::R1,
        field(CoreKind::FieldByteOffset, l.clone(), &["len"]),
    )
    .unwrap();
    a.core(
        Reg::R2,
        field(CoreKind::FieldByteOffset, l.clone(), &["cookie"]),
    )
    .unwrap();
    a.core(
        Reg::R3,
        CoreSpec::of_type(CoreKind::TypeIdTarget, l.clone()).unwrap(),
    )
    .unwrap();
    a.core(Reg::R4, CoreSpec::of_type(CoreKind::TypeSize, l).unwrap())
        .unwrap();
    // vflags exists in vmlinux, so the module's vflags is never consulted
    a.core(
        Reg::R5,
        field(CoreKind::FieldByteSize, local_vflags(), &["c"]),
    )
    .unwrap();
    a.mov64(Reg::R0, 0);
    a.exit();
    let p = a.finish("modules", ProgramType::Xdp).unwrap();
    // without modules the type is missing
    let e = apply(&p, &base).unwrap_err();
    assert!(e.to_string().contains("vmod_ctx"), "{e}");
    let r = apply_with_modules(&p, &base, &[&module]).unwrap();
    let imms: Vec<i32> = r.insns[..5].iter().map(|i| i.imm).collect();
    assert_eq!(imms, vec![16, 8, ctx as i32, 24, 8]);
}

#[test]
fn malformed_split_btf_is_an_error() {
    let (base, _) = base_and_module();
    let mut m = BtfBuilder::split(&base);
    let n = base.len() as u32;
    m.structure("bad", false, 8, &[("x", n + 5, 0)]);
    let e = Btf::parse_split(&m.finish(), &base).unwrap_err();
    assert!(e.to_string().contains("past the type table"), "{e}");
    // split on split is refused
    let (_, module) = base_and_module();
    let m = BtfBuilder::split(&module);
    assert!(Btf::parse_split(&m.finish(), &module).is_err());
    // a name offset past both string sections
    let mut m = BtfBuilder::split(&base);
    m.header(1 << 20, 1, 0, false, 4);
    m.word(32);
    assert!(Btf::parse_split(&m.finish(), &base).is_err());
    // truncations never panic
    let mut m = BtfBuilder::split(&base);
    m.structure("ok", false, 4, &[("a", 1, 0)]);
    let bytes = m.finish();
    for cut in 0..bytes.len() {
        let _ = Btf::parse_split(&bytes[..cut], &base);
    }
}

fn host_btf() -> Option<Btf> {
    match vesper_ebpf::btf::host_vmlinux() {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("kernel BTF unavailable on this host: {e}");
            None
        }
    }
}

/// The relocations the host-kernel tests use, with the value libbpf's
/// rules give on x86_64 vmlinux (`None`: host-dependent).
fn host_specs(btf: &Btf) -> Vec<(&'static str, CoreSpec, Option<u64>)> {
    use CoreKind::*;
    let th = local_tcphdr();
    let task = LocalType::structure(
        "task_struct",
        8,
        vec![("pid", LocalType::int("int", 4, true), 0)],
    );
    let _ = btf;
    vec![
        (
            "doff offset",
            field(FieldByteOffset, th.clone(), &["doff"]),
            Some(12),
        ),
        (
            "doff size",
            field(FieldByteSize, th.clone(), &["doff"]),
            Some(2),
        ),
        (
            "doff lshift",
            field(FieldLshiftU64, th.clone(), &["doff"]),
            Some(56),
        ),
        (
            "doff rshift",
            field(FieldRshiftU64, th.clone(), &["doff"]),
            Some(60),
        ),
        (
            "syn lshift",
            field(FieldLshiftU64, th.clone(), &["syn"]),
            Some(54),
        ),
        (
            "syn rshift",
            field(FieldRshiftU64, th.clone(), &["syn"]),
            Some(63),
        ),
        (
            "syn signed",
            field(FieldSigned, th.clone(), &["syn"]),
            Some(0),
        ),
        (
            "tcphdr matches",
            CoreSpec::of_type(TypeMatches, th.clone()).unwrap(),
            Some(1),
        ),
        (
            "tcphdr with a wrong bitfield width still matches",
            CoreSpec::of_type(
                TypeMatches,
                LocalType::structure_bits("tcphdr", 20, vec![("doff", u16t(), 0, 5)]),
            )
            .unwrap(),
            Some(1),
        ),
        (
            "tcphdr with a signed member does not match",
            CoreSpec::of_type(
                TypeMatches,
                LocalType::structure(
                    "tcphdr",
                    20,
                    vec![("source", LocalType::int("short int", 2, true), 0)],
                ),
            )
            .unwrap(),
            Some(0),
        ),
        (
            "task_struct target id",
            CoreSpec::of_type(TypeIdTarget, task.clone()).unwrap(),
            None,
        ),
        (
            "task_struct local id",
            CoreSpec::of_type(TypeIdLocal, task.clone()).unwrap(),
            None,
        ),
        (
            "task_struct pid offset",
            field(FieldByteOffset, task, &["pid"]),
            None,
        ),
    ]
}

#[test]
fn core_relocates_bitfields_and_ids_against_the_host_kernel() {
    let Some(btf) = host_btf() else { return };
    for (name, spec, want) in host_specs(&btf) {
        let p = value_prog(spec);
        let r = apply(&p, &btf).unwrap_or_else(|e| panic!("{name}: {e}"));
        let v = r.insns[0].imm as u32 as u64;
        if cfg!(target_arch = "x86_64")
            && let Some(w) = want
        {
            assert_eq!(v, w, "{name}");
        }
        if name == "task_struct target id" {
            assert_eq!(v as u32, btf.by_name("task_struct")[0], "{name}");
        }
    }
    let mods = vesper_ebpf::btf::host_modules(&btf).unwrap_or_default();
    eprintln!("host module BTF: {} modules", mods.len());
    for (n, m) in &mods {
        assert_eq!(m.base_len() as usize, btf.len(), "{n}");
    }
}

/// XDP: read the TCP header's `doff` from the packet with relocated
/// offset and shifts (`BPF_CORE_READ_BITFIELD` on a direct load).
fn doff_reader() -> Program {
    let mut a = Asm::new();
    let out = a.label();
    let th = local_tcphdr();
    a.ldx(Size::W, Reg::R2, Reg::R1, 0);
    a.ldx(Size::W, Reg::R3, Reg::R1, 4);
    a.mov64(Reg::R0, 0);
    a.mov64(Reg::R4, Reg::R2);
    a.alu64(AluOp::Add, Reg::R4, 54);
    a.jmp(JmpOp::Gt, Reg::R4, Reg::R3, out);
    a.alu64(AluOp::Add, Reg::R2, 34);
    a.core_ldx(
        Size::H,
        Reg::R0,
        Reg::R2,
        field(CoreKind::FieldByteOffset, th.clone(), &["doff"]),
    )
    .unwrap();
    a.core(
        Reg::R5,
        field(CoreKind::FieldLshiftU64, th.clone(), &["doff"]),
    )
    .unwrap();
    a.core(Reg::R6, field(CoreKind::FieldRshiftU64, th, &["doff"]))
        .unwrap();
    a.alu64(AluOp::Lsh, Reg::R0, Reg::R5);
    a.alu64(AluOp::Rsh, Reg::R0, Reg::R6);
    a.bind(out).unwrap();
    a.exit();
    let mut p = a.finish("doff", ProgramType::Xdp).unwrap();
    p.license = "GPL".into();
    p
}

fn tcp_packet(doff: u8) -> Vec<u8> {
    let mut pkt = vec![0u8; 64];
    pkt[12] = 0x08;
    pkt[14] = 0x45;
    pkt[23] = 6;
    // TCP header at 34: data offset in the high nibble of byte 12
    pkt[34 + 12] = doff << 4;
    pkt[34 + 13] = 0x02; // SYN
    pkt
}

#[test]
fn relocated_bitfield_reads_run_in_the_interpreter() {
    let Some(btf) = host_btf() else { return };
    if !cfg!(target_arch = "x86_64") {
        return;
    }
    let r = apply(&doff_reader(), &btf).unwrap();
    verify(&r, &VerifierPolicy::default()).expect("relocated reader verifies");
    for doff in [5u8, 8, 15] {
        let mut env = Env::new(&r);
        assert_eq!(
            run_xdp(&r, &mut env, &tcp_packet(doff)).unwrap().ret,
            doff as u64
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn kernel_relocates_bitfields_ids_and_matches_like_us() {
    use vesper_ebpf::kernel;
    if let Err(e) = kernel::probe() {
        eprintln!("kernel BPF evidence unavailable on this host: {e}");
        return;
    }
    let Some(btf) = host_btf() else { return };
    for (name, spec, _) in host_specs(&btf) {
        let mut p = value_prog(spec);
        p.license = "GPL".into();
        let ours = apply(&p, &btf).unwrap().insns[0].imm as u32;
        // the kernel applies the record itself (BPF_PROG_LOAD core_relos)
        let loaded = kernel::load_with_ext(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        let k = kernel::test_run(&loaded.prog, &[0; 64]).expect("test run");
        assert_eq!(k.retval, ours, "{name}");
        // and our user-space relocation loads and returns the same
        let loaded = kernel::load(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        let k = kernel::test_run(&loaded.prog, &[0; 64]).expect("test run");
        assert_eq!(k.retval, ours, "{name}");
    }
    if cfg!(target_arch = "x86_64") {
        let p = doff_reader();
        let ours = apply(&p, &btf).unwrap();
        let kl = kernel::load_with_ext(&p).expect("kernel relocates the reader");
        let ul = kernel::load(&p).expect("user-space relocated reader loads");
        for doff in [5u8, 8, 15] {
            let pkt = tcp_packet(doff);
            let mut env = Env::new(&ours);
            let u = run_xdp(&ours, &mut env, &pkt).unwrap().ret;
            let k1 = kernel::test_run(&kl.prog, &pkt).unwrap().retval as u64;
            let k2 = kernel::test_run(&ul.prog, &pkt).unwrap().retval as u64;
            assert_eq!((u, k1, k2), (doff as u64, doff as u64, doff as u64));
        }
    }
}

#[test]
fn union_bitfields_survive_the_object_round_trip() {
    let local = LocalType::Struct {
        name: "vu".into(),
        union: true,
        size: 4,
        members: vec![vesper_ebpf::core::LocalMember {
            name: "f".into(),
            ty: u32t(),
            offset: 0,
            bitfield: Some(vesper_ebpf::core::Bitfield {
                bit_offset: 0,
                bits: 3,
            }),
        }],
    };
    let p = value_prog(field(CoreKind::FieldRshiftU64, local.clone(), &["f"]));
    assert_eq!(p.insns[0].imm, 61);
    let back = read_object(&write_object(&p).unwrap()).unwrap();
    assert_eq!(back.core_relos, p.core_relos);
    let ob = vesper_ebpf::btf_ext::build(&p, "xdp").unwrap();
    let own = Btf::parse(&ob.btf).unwrap();
    let id = vesper_ebpf::btf_ext::local_root_ids(&p).unwrap()[0];
    assert_eq!(vesper_ebpf::btf_ext::local_type(&own, id).unwrap(), local);
}

#[test]
fn imported_type_ids_are_renumbered_for_our_btf() {
    // an object whose BTF numbers the root differently: its TYPE_ID
    // immediates are rewritten to the ids of the BTF we emit
    let mut p = value_prog(CoreSpec::of_type(CoreKind::TypeIdLocal, local_vflags()).unwrap());
    let ours = p.insns[0].imm;
    p.insns[0].imm = 22;
    let back = read_object(&write_object(&p).unwrap()).unwrap();
    assert_eq!(back.insns[0].imm, ours);
    let t = bitfield_target();
    assert_eq!(apply(&back, &t).unwrap().insns[0].imm, ours);
}
