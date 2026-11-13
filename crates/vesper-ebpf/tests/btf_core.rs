//! BTF CO-RE and `.BTF.ext`: relocation against synthetic and real kernel
//! BTF, the object round trip, aya-obj's independent reading and
//! relocation of our records, and the kernel applying them itself.

mod common;

use vesper_ebpf::btf::{Btf, BtfBuilder};
use vesper_ebpf::core::{CoreError, CoreKind, CoreSpec, LocalType, apply};
use vesper_ebpf::elf::{read_object, write_object};
use vesper_ebpf::insn::{Reg, Size};
use vesper_ebpf::{Asm, Program, ProgramType, VerifierPolicy, verify};

fn ulong() -> LocalType {
    LocalType::int("long unsigned int", 8, false)
}

/// A local `struct pt_regs` with only the members the probe uses, at
/// deliberately wrong offsets: CO-RE must move them.
fn local_pt_regs() -> LocalType {
    LocalType::structure("pt_regs", 16, vec![("di", ulong(), 0), ("si", ulong(), 8)])
}

fn local_map_type() -> LocalType {
    LocalType::enumeration(
        "bpf_map_type",
        4,
        &[("BPF_MAP_TYPE_HASH", 99), ("BPF_MAP_TYPE_PROG_ARRAY", 98)],
    )
}

/// A kprobe that reads `regs->di` and `regs->si` through CO-RE offsets and
/// loads a type size, a field-exists flag and an enumerator value.
fn core_probe() -> Program {
    let mut a = Asm::new();
    let regs = local_pt_regs();
    a.line("probe.ves", 3, 5, "let di = regs.di");
    a.core_ldx(
        Size::DW,
        Reg::R2,
        Reg::R1,
        CoreSpec::field(CoreKind::FieldByteOffset, regs.clone(), &["di"]).unwrap(),
    )
    .unwrap();
    a.line("probe.ves", 4, 5, "let si = regs.si");
    a.core_ldx(
        Size::DW,
        Reg::R3,
        Reg::R1,
        CoreSpec::field(CoreKind::FieldByteOffset, regs.clone(), &["si"]).unwrap(),
    )
    .unwrap();
    a.core(
        Reg::R4,
        CoreSpec::of_type(CoreKind::TypeSize, regs.clone()).unwrap(),
    )
    .unwrap();
    a.core(
        Reg::R5,
        CoreSpec::enumval(
            CoreKind::EnumvalValue,
            local_map_type(),
            "BPF_MAP_TYPE_PROG_ARRAY",
        )
        .unwrap(),
    )
    .unwrap();
    a.core(
        Reg::R0,
        CoreSpec::field(CoreKind::FieldExists, regs, &["si"]).unwrap(),
    )
    .unwrap();
    a.line("probe.ves", 6, 5, "0");
    a.mov64(Reg::R0, 0);
    a.exit();
    let mut p = a.finish("core_probe", ProgramType::Kprobe).unwrap();
    p.attach = "do_unlinkat".into();
    p
}

/// Target BTF: `struct pt_regs { u64 pad[3]; u64 si; u64 di; }` and
/// `enum bpf_map_type { ..., BPF_MAP_TYPE_PROG_ARRAY = 3 }`.
fn synthetic_target() -> Btf {
    let mut b = BtfBuilder::new();
    let u64t = b.int("long unsigned int", 8, false);
    let idx = b.int("int", 4, true);
    let pad = b.array(u64t, idx, 3);
    b.structure(
        "pt_regs",
        false,
        40,
        &[("pad", pad, 0), ("si", u64t, 192), ("di", u64t, 256)],
    );
    b.enumeration(
        "bpf_map_type",
        4,
        &[
            ("BPF_MAP_TYPE_UNSPEC", 0),
            ("BPF_MAP_TYPE_HASH", 1),
            ("BPF_MAP_TYPE_PROG_ARRAY", 3),
        ],
    );
    Btf::parse(&b.finish()).unwrap()
}

/// (slot, off, imm) of every relocated instruction of `core_probe()`.
fn relocated(p: &Program) -> Vec<(i16, i32)> {
    [0usize, 1, 2, 3, 4]
        .iter()
        .map(|&s| (p.insns[s].off, p.insns[s].imm))
        .collect()
}

#[test]
fn core_relocates_against_a_synthetic_target() {
    let p = core_probe();
    assert_eq!(p.core_relos.len(), 5);
    assert_eq!(
        relocated(&p),
        vec![(0, 0), (8, 0), (0, 16), (0, 98), (0, 1)]
    );
    let r = apply(&p, &synthetic_target()).unwrap();
    assert!(r.core_relos.is_empty());
    assert_eq!(
        relocated(&r),
        vec![(32, 0), (24, 0), (0, 40), (0, 3), (0, 1)]
    );
    // field-exists of a member the target lacks is 0, not an error
    let mut a = Asm::new();
    let local = LocalType::structure("pt_regs", 8, vec![("orig_ax", ulong(), 0)]);
    a.core(
        Reg::R0,
        CoreSpec::field(CoreKind::FieldExists, local.clone(), &["orig_ax"]).unwrap(),
    )
    .unwrap();
    a.exit();
    let p = a.finish("exists", ProgramType::Kprobe).unwrap();
    let r = apply(&p, &synthetic_target()).unwrap();
    assert_eq!(r.insns[0].imm, 0);
    // ... but its offset is a typed error
    let mut a = Asm::new();
    a.core_ldx(
        Size::DW,
        Reg::R0,
        Reg::R1,
        CoreSpec::field(CoreKind::FieldByteOffset, local, &["orig_ax"]).unwrap(),
    )
    .unwrap();
    a.exit();
    let p = a.finish("missing", ProgramType::Kprobe).unwrap();
    assert!(matches!(
        apply(&p, &synthetic_target()),
        Err(CoreError::NoMatch(_))
    ));
}

#[test]
fn objects_round_trip_btf_ext() {
    let mut progs = vec![core_probe()];
    for case in common::corpus::corpus() {
        if case.rule.is_none()
            && (!case.prog.kfuncs.is_empty() || case.name.starts_with("accept/bpf_loop"))
        {
            progs.push(case.prog);
        }
    }
    assert!(progs.len() >= 4);
    for p in progs {
        let bytes = write_object(&p).unwrap();
        let back = read_object(&bytes).unwrap();
        assert_eq!(back.prog_type, p.prog_type, "{}", p.name);
        assert_eq!(back.attach, p.attach, "{}", p.name);
        assert_eq!(back.insns, p.insns, "{}", p.name);
        assert_eq!(back.maps, p.maps, "{}", p.name);
        assert_eq!(back.kfuncs, p.kfuncs, "{}", p.name);
        assert_eq!(back.core_relos, p.core_relos, "{}", p.name);
        assert_eq!(back.lines, p.lines, "{}", p.name);
        // never panics on truncated objects
        for cut in (0..bytes.len()).step_by(7) {
            let _ = read_object(&bytes[..cut]);
        }
    }
}

#[test]
fn malformed_btf_ext_is_an_error() {
    let p = core_probe();
    let ob = vesper_ebpf::btf_ext::build(&p, "kprobe/do_unlinkat").unwrap();
    let btf = Btf::parse(&ob.btf).unwrap();
    let ok = vesper_ebpf::btf_ext::parse_ext(&btf, &ob.ext, "kprobe/do_unlinkat").unwrap();
    assert_eq!(ok.core_relos, p.core_relos);
    assert_eq!(ok.lines, p.lines);
    for cut in 0..ob.ext.len() {
        let _ = vesper_ebpf::btf_ext::parse_ext(&btf, &ob.ext[..cut], "kprobe/do_unlinkat");
    }
    let mut bad = ob.ext.clone();
    bad[0] ^= 0xff;
    assert!(vesper_ebpf::btf_ext::parse_ext(&btf, &bad, "kprobe/do_unlinkat").is_err());
    // a record count far past the end
    let mut bad = ob.ext.clone();
    let n = bad.len();
    bad[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
    let _ = vesper_ebpf::btf_ext::parse_ext(&btf, &bad, "kprobe/do_unlinkat");
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

#[test]
fn core_relocates_against_the_host_kernel() {
    let Some(btf) = host_btf() else { return };
    assert!(btf.len() > 10_000, "vmlinux BTF has {} types", btf.len());
    assert!(btf.func_id("bpf_preempt_disable").is_some());
    let r = apply(&core_probe(), &btf).unwrap();
    if cfg!(target_arch = "x86_64") {
        // struct pt_regs: r15 .. ax, orig_ax, ip, cs, flags, sp, ss
        assert_eq!(
            relocated(&r),
            vec![(112, 0), (104, 0), (0, 168), (0, 3), (0, 1)]
        );
        verify(&r, &VerifierPolicy::default()).expect("relocated probe verifies");
    }
}

#[cfg(unix)]
#[test]
fn aya_reads_and_relocates_our_btf_ext() {
    let Some(btf) = host_btf() else { return };
    let p = core_probe();
    let bytes = write_object(&p).unwrap();
    let mut obj = aya_obj::Object::parse(&bytes).expect("aya-obj parses the object");
    assert!(obj.btf_ext.is_some(), "aya-obj found .BTF.ext");
    let prog = &obj.programs["core_probe"];
    assert!(matches!(prog.section, aya_obj::ProgramSection::KProbe));
    {
        let f = obj.functions.values().next().expect("function");
        assert_eq!(f.line_info.line_info.len(), p.lines.len());
        assert_eq!(f.func_info.func_info.len(), 1);
    }
    let target = aya_obj::btf::Btf::from_sys_fs().expect("aya parses vmlinux BTF");
    obj.relocate_btf(&target)
        .expect("aya applies our CO-RE records");
    let ours = apply(&p, &btf).unwrap();
    let f = obj.functions.values().next().expect("function");
    assert_eq!(f.instructions.len(), ours.insns.len());
    for (i, (a, o)) in f.instructions.iter().zip(&ours.insns).enumerate() {
        assert_eq!((a.code, a.off, a.imm), (o.code, o.off, o.imm), "slot {i}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn kernel_applies_our_core_records_and_takes_our_line_info() {
    use vesper_ebpf::kernel;
    if let Err(e) = kernel::probe() {
        eprintln!("kernel BPF evidence unavailable on this host: {e}");
        return;
    }
    if !cfg!(target_arch = "x86_64") {
        return;
    }
    let p = core_probe();
    // the kernel relocates the records itself (BPF_PROG_LOAD core_relos)
    kernel::load_with_ext(&p).expect("kernel applies CO-RE and accepts line info");
    // the records really are applied by the kernel: one naming a type the
    // kernel lacks poisons its instruction and the load fails
    let mut a = Asm::new();
    let ghost = LocalType::structure("vesper_no_such_type", 8, vec![("x", ulong(), 0)]);
    a.core_ldx(
        Size::DW,
        Reg::R0,
        Reg::R1,
        CoreSpec::field(CoreKind::FieldByteOffset, ghost, &["x"]).unwrap(),
    )
    .unwrap();
    a.mov64(Reg::R0, 0);
    a.exit();
    let ghost = a.finish("ghost", ProgramType::Kprobe).unwrap();
    verify(&ghost, &VerifierPolicy::default()).expect("the unrelocated program verifies");
    match kernel::load_with_ext(&ghost) {
        Err(kernel::LoadError::Rejected { log, .. }) => {
            assert!(log.contains("relo #0: no matching targets found"), "{log}")
        }
        other => panic!("expected a rejection, got {:?}", other.map(|_| ())),
    }
    // and the program rebuilt from our object loads the same way
    let back = read_object(&write_object(&p).unwrap()).unwrap();
    kernel::load_with_ext(&back).expect("object program loads");
    // user-space relocation then load
    kernel::load(&p).expect("user-space CO-RE then load");
    // callbacks and kfuncs with program BTF
    for case in common::corpus::corpus() {
        if case.rule.is_none()
            && (!case.prog.kfuncs.is_empty() || case.name.starts_with("accept/bpf_loop"))
        {
            kernel::load_with_ext(&case.prog).unwrap_or_else(|e| panic!("{}: {e}", case.name));
        }
    }
}
