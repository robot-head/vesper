//! The interpreter runs every program type: tc (`__sk_buff` context with
//! packet pointers and writable fields), tracepoint (read-only record)
//! and kprobe (read-only `pt_regs`), and tail calls through program
//! arrays. Each program is checked by this crate's verifier first; where
//! the host permits `bpf()` the same program runs under
//! `BPF_PROG_TEST_RUN` (tc, XDP) and the results must be identical.
//! Tracepoint and kprobe programs cannot be test-run by the kernel; for
//! those the kernel verifier verdict is checked, and execution evidence
//! comes from the modelled contexts only.

use vesper_ebpf::helpers as h;
use vesper_ebpf::insn::{AluOp, JmpOp, Reg, Size};
use vesper_ebpf::interp::{
    Env, Input, InterpError, LOOPBACK_IFINDEX, MAX_TAIL_CALL_CNT, PACKET_HOST, PACKET_MULTICAST,
    PACKET_OTHERHOST, SkbMeta, TailCallError, Task, run, run_skb, run_xdp,
};
use vesper_ebpf::program::{Asm, MapDef, MapKind, Program, ProgramType};
use vesper_ebpf::verifier::{PT_REGS_SIZE, SKB_DATA, SKB_DATA_END};
use vesper_ebpf::{VerifierPolicy, verify};

fn verified(p: Program) -> Program {
    if let Err(e) = verify(&p, &VerifierPolicy::default()) {
        panic!(
            "{}: {e}\n{}",
            p.name,
            vesper_ebpf::insn::disassemble(&p.insns)
        );
    }
    p
}

/// `r0 = ((u32 *)ctx)[off / 4]; exit`.
fn read_field(t: ProgramType, off: i16, size: Size) -> Program {
    let mut a = Asm::new();
    a.ldx(size, Reg::R0, Reg::R1, off);
    a.exit();
    verified(a.finish("read_field", t).unwrap())
}

/// The `__sk_buff` fields this crate's tc model reads.
const SKB_FIELDS: &[(i16, &str)] = &[
    (0, "len"),
    (4, "pkt_type"),
    (8, "mark"),
    (12, "queue_mapping"),
    (16, "protocol"),
    (24, "vlan_tci"),
    (28, "vlan_proto"),
    (32, "priority"),
    (36, "ingress_ifindex"),
    (40, "ifindex"),
    (44, "tc_index"),
    (48, "cb[0]"),
    (52, "cb[1]"),
    (56, "cb[2]"),
    (60, "cb[3]"),
    (64, "cb[4]"),
    (68, "hash"),
    (72, "tc_classid"),
];

fn eth(dst: [u8; 6], ether_type: u16, len: usize) -> Vec<u8> {
    let mut p = dst.to_vec();
    p.extend([0x02, 0, 0, 0, 0, 1]);
    p.extend(ether_type.to_be_bytes());
    p.resize(len.max(14), 0x5a);
    p
}

/// Packets that exercise every `pkt_type` and protocol rule of
/// `eth_type_trans`.
fn skb_packets() -> Vec<Vec<u8>> {
    let mut raw8023 = eth([0; 6], 0x0040, 60);
    raw8023[14] = 0xff;
    raw8023[15] = 0xff;
    vec![
        eth([0; 6], 0x0800, 64),
        eth([0xff; 6], 0x0806, 42),
        eth([0x01, 0x00, 0x5e, 0, 0, 1], 0x86dd, 80),
        eth([0x02, 0, 0, 0, 0, 2], 0x0800, 34),
        eth([0x02, 0, 0, 0, 0, 2], 0x88b5, 14),
        raw8023,
        eth([0; 6], 0x0040, 60),
    ]
}

#[test]
fn skb_meta_follows_eth_type_trans() {
    let p = skb_packets();
    let m: Vec<(u32, u16)> = p
        .iter()
        .map(|p| {
            let m = SkbMeta::test_run(p);
            (m.pkt_type, m.protocol)
        })
        .collect();
    assert_eq!(
        m,
        [
            (PACKET_HOST, 0x0800),
            (PACKET_MULTICAST, 0x0806),
            (PACKET_MULTICAST, 0x86dd),
            (PACKET_OTHERHOST, 0x0800),
            (PACKET_OTHERHOST, 0x88b5),
            (PACKET_HOST, 0x0001),
            (PACKET_HOST, 0x0004),
        ]
    );
    assert_eq!(SkbMeta::test_run(&p[0]).ifindex, LOOPBACK_IFINDEX);
}

#[test]
fn tc_reads_every_modelled_field() {
    let pkt = eth([0xff; 6], 0x0800, 70);
    let meta = SkbMeta {
        mark: 7,
        priority: 9,
        cb: [1, 2, 3, 4, 5],
        hash: 0xabcd,
        tc_index: 3,
        ..SkbMeta::test_run(&pkt)
    };
    let mut got = Vec::new();
    for &(off, _) in SKB_FIELDS {
        let p = read_field(ProgramType::SchedCls, off, Size::W);
        let mut env = Env::new(&p);
        got.push(run_skb(&p, &mut env, &pkt, meta.clone()).unwrap().ret);
    }
    // protocol holds the two network-order bytes: 08 00 read as LE
    assert_eq!(
        got,
        [
            70, 2, 7, 0, 0x0008, 0, 0, 9, 0, 1, 3, 1, 2, 3, 4, 5, 0xabcd, 0
        ]
    );
}

/// A tc program that bounds-checks and rewrites the packet, writes the
/// writable fields and returns `len + data[12]`.
fn tc_writer() -> Program {
    let mut a = Asm::new();
    let out = a.label();
    a.mov64(Reg::R6, Reg::R1);
    a.ldx(Size::W, Reg::R2, Reg::R6, SKB_DATA as i16);
    a.ldx(Size::W, Reg::R3, Reg::R6, SKB_DATA_END as i16);
    a.mov64(Reg::R4, Reg::R2);
    a.add64(Reg::R4, 14);
    a.mov64(Reg::R0, 2); // TC_ACT_SHOT
    a.jmp(JmpOp::Gt, Reg::R4, Reg::R3, out);
    a.ldx(Size::B, Reg::R5, Reg::R2, 12);
    a.st(Size::B, Reg::R2, 0, 0x42);
    a.st(Size::H, Reg::R2, 6, 0x1234);
    a.ldx(Size::W, Reg::R0, Reg::R6, 0);
    a.add64(Reg::R0, Reg::R5);
    a.mov64(Reg::R7, 0x55);
    a.stx(Size::W, Reg::R6, 8, Reg::R7); // mark
    a.stx(Size::W, Reg::R6, 32, Reg::R0); // priority
    a.ldx(Size::W, Reg::R8, Reg::R6, 48);
    a.add64(Reg::R8, 1);
    a.stx(Size::W, Reg::R6, 64, Reg::R8); // cb[4] = cb[0] + 1
    a.stx(Size::W, Reg::R6, 44, Reg::R8); // tc_index
    a.bind(out).unwrap();
    a.exit();
    verified(a.finish("tc_writer", ProgramType::SchedCls).unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[test]
fn tc_writes_packet_and_fields() {
    let p = tc_writer();
    let pkt = eth([0; 6], 0x0800, 40);
    let mut env = Env::new(&p);
    let meta = SkbMeta {
        cb: [10, 0, 0, 0, 0],
        ..SkbMeta::test_run(&pkt)
    };
    let r = run_skb(&p, &mut env, &pkt, meta).unwrap();
    assert_eq!(r.ret, 40 + 8);
    assert_eq!(r.packet[0], 0x42);
    assert_eq!(&r.packet[6..8], &0x1234u16.to_le_bytes());
    assert_eq!(&r.packet[8..], &pkt[8..]);
    assert_eq!(u32_at(&r.ctx, 8), 0x55);
    assert_eq!(u32_at(&r.ctx, 32), 48);
    assert_eq!(u32_at(&r.ctx, 64), 11);
    assert_eq!(u32_at(&r.ctx, 44), 11);
    // a runt packet takes the bounds-check exit
    let short = vec![0u8; 10];
    let meta = SkbMeta::test_run(&short);
    assert_eq!(run_skb(&p, &mut env, &short, meta).unwrap().ret, 2);
}

/// A tracepoint program in the shape of `syscalls/sys_enter_*`: `id` (the
/// syscall number) at 8 and six `u64` arguments from 16; it stores
/// `args[0] + args[1]` under key `id` and returns `args[2]`.
fn tracepoint_prog() -> Program {
    let mut a = Asm::new();
    let m = a
        .map(MapDef::new("tp_sum", MapKind::Hash, 4, 8, 16))
        .unwrap();
    a.mov64(Reg::R6, Reg::R1);
    a.ldx(Size::DW, Reg::R7, Reg::R6, 16);
    a.ldx(Size::DW, Reg::R8, Reg::R6, 24);
    a.add64(Reg::R7, Reg::R8);
    a.stx(Size::DW, Reg::R10, -16, Reg::R7);
    a.ldx(Size::W, Reg::R9, Reg::R6, 8);
    a.stx(Size::W, Reg::R10, -4, Reg::R9);
    a.ld_map(Reg::R1, m);
    a.mov64(Reg::R2, Reg::R10);
    a.add64(Reg::R2, -4);
    a.mov64(Reg::R3, Reg::R10);
    a.add64(Reg::R3, -16);
    a.mov64(Reg::R4, 0);
    a.call(h::MAP_UPDATE_ELEM);
    a.ldx(Size::DW, Reg::R0, Reg::R6, 32);
    a.exit();
    verified(a.finish("tp", ProgramType::Tracepoint).unwrap())
}

fn sys_enter(nr: i32, args: [u64; 6]) -> Vec<u8> {
    let mut r = vec![0u8; 8]; // common_type, flags, preempt_count, pid
    r.extend(nr.to_le_bytes());
    r.extend([0u8; 4]);
    for a in args {
        r.extend(a.to_le_bytes());
    }
    r
}

#[test]
fn tracepoint_reads_the_record() {
    let p = tracepoint_prog();
    let mut env = Env::new(&p);
    let r = run(
        &p,
        &mut env,
        Input::Tracepoint(&sys_enter(3, [40, 2, 99, 0, 0, 0])),
    )
    .unwrap();
    assert_eq!(r.ret, 99);
    assert!(r.packet.is_empty());
    assert_eq!(
        env.map_get(0, &3u32.to_le_bytes()),
        Some(42u64.to_le_bytes().to_vec())
    );
    // a short record reads as zero past its end
    let r = run(
        &p,
        &mut env,
        Input::Tracepoint(&sys_enter(5, [1, 1, 0, 0, 0, 0])[..24]),
    )
    .unwrap();
    assert_eq!(r.ret, 0);
    assert_eq!(
        env.map_get(0, &5u32.to_le_bytes()),
        Some(1u64.to_le_bytes().to_vec())
    );
    // the common fields are not readable
    let bad = read_field(ProgramType::Kprobe, 0, Size::W);
    let mut bad = bad.clone();
    bad.prog_type = ProgramType::Tracepoint;
    assert!(verify(&bad, &VerifierPolicy::default()).is_err());
    let mut env = Env::new(&bad);
    assert!(matches!(
        run(&bad, &mut env, Input::Tracepoint(&[0; 64])),
        Err(InterpError::InvalidAccess { .. })
    ));
    // oversized records and mismatched inputs are typed errors
    assert!(matches!(
        run(&p, &mut Env::new(&p), Input::Tracepoint(&[0; 4096])),
        Err(InterpError::BadInput(_))
    ));
    assert!(matches!(
        run(&p, &mut Env::new(&p), Input::Xdp(&[0; 64])),
        Err(InterpError::BadInput(_))
    ));
}

/// Offset of the first argument register in `pt_regs` (`di` on x86-64,
/// `regs[0]` on AArch64).
const ARG0: i16 = if cfg!(target_arch = "x86_64") { 112 } else { 0 };
const ARG1: i16 = if cfg!(target_arch = "x86_64") { 104 } else { 8 };

#[test]
fn kprobe_reads_pt_regs_and_task() {
    if PT_REGS_SIZE == 0 {
        eprintln!("kprobe contexts are not modelled on this architecture");
        return;
    }
    let mut a = Asm::new();
    a.mov64(Reg::R6, Reg::R1);
    a.call(h::GET_CURRENT_PID_TGID);
    a.mov64(Reg::R7, Reg::R0);
    a.alu64(AluOp::Rsh, Reg::R7, 32); // tgid
    a.mov64(Reg::R2, 16);
    a.mov64(Reg::R1, Reg::R10);
    a.add64(Reg::R1, -16);
    a.call(h::GET_CURRENT_COMM);
    a.ldx(Size::B, Reg::R8, Reg::R10, -16); // comm[0]
    a.ldx(Size::DW, Reg::R0, Reg::R6, ARG0);
    a.ldx(Size::DW, Reg::R9, Reg::R6, ARG1);
    a.alu64(AluOp::Mul, Reg::R0, Reg::R9);
    a.add64(Reg::R0, Reg::R7);
    a.alu64(AluOp::Lsh, Reg::R8, 40);
    a.add64(Reg::R0, Reg::R8);
    a.exit();
    let p = verified(a.finish("kp", ProgramType::Kprobe).unwrap());
    let mut regs = vec![0u8; PT_REGS_SIZE as usize];
    regs[ARG0 as usize..ARG0 as usize + 8].copy_from_slice(&6u64.to_le_bytes());
    regs[ARG1 as usize..ARG1 as usize + 8].copy_from_slice(&7u64.to_le_bytes());
    let mut env = Env::new(&p);
    env.task = Task::named("init", 1, 77);
    let r = run(&p, &mut env, Input::Kprobe(&regs)).unwrap();
    assert_eq!(r.ret, 42 + 77 + ((b'i' as u64) << 40));
    assert!(matches!(
        run(&p, &mut env, Input::Kprobe(&regs[..8])),
        Err(InterpError::BadInput(_))
    ));
}

#[test]
fn get_current_comm_truncates_and_pads() {
    let mut a = Asm::new();
    a.mov64(Reg::R1, Reg::R10);
    a.add64(Reg::R1, -8);
    a.mov64(Reg::R2, 4);
    a.call(h::GET_CURRENT_COMM);
    a.ldx(Size::W, Reg::R0, Reg::R10, -8);
    a.exit();
    let p = verified(a.finish("comm", ProgramType::Tracepoint).unwrap());
    let mut env = Env::new(&p);
    env.task = Task::named("vesperd", 5, 5);
    let r = run(&p, &mut env, Input::Tracepoint(&[0; 16])).unwrap();
    assert_eq!(r.ret.to_le_bytes()[..4], *b"ves\0");
}

/// An XDP or tc owner that tail-calls slot `r = ctx-derived index` of a
/// program array and returns 1 when the call falls through.
fn owner(t: ProgramType, slots: u32, index: i32, counter: bool) -> Program {
    let mut a = Asm::new();
    let pa = a.map(MapDef::prog_array("jmp", slots)).unwrap();
    if counter {
        a.map(MapDef::new("hits", MapKind::Array, 4, 8, 1)).unwrap();
    }
    a.ld_map(Reg::R2, pa);
    a.mov64(Reg::R3, index);
    a.call(h::TAIL_CALL);
    a.mov64(Reg::R0, 1);
    a.exit();
    verified(a.finish("owner", t).unwrap())
}

/// A tail-call target: bumps `hits[0]`, then tail-calls slot `next` of the
/// same array (if any) and returns `ret` when that falls through.
fn target(t: ProgramType, slots: u32, next: Option<i32>, ret: i32) -> Program {
    let mut a = Asm::new();
    let pa = a.map(MapDef::prog_array("jmp", slots)).unwrap();
    let hits = a.map(MapDef::new("hits", MapKind::Array, 4, 8, 1)).unwrap();
    let skip = a.label();
    a.mov64(Reg::R6, Reg::R1);
    a.st(Size::W, Reg::R10, -4, 0);
    a.ld_map(Reg::R1, hits);
    a.mov64(Reg::R2, Reg::R10);
    a.add64(Reg::R2, -4);
    a.call(h::MAP_LOOKUP_ELEM);
    a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
    a.mov64(Reg::R1, 1);
    a.atomic(
        Size::DW,
        vesper_ebpf::insn::AtomicOp::Add { fetch: false },
        Reg::R0,
        0,
        Reg::R1,
    );
    a.bind(skip).unwrap();
    if let Some(n) = next {
        a.mov64(Reg::R1, Reg::R6);
        a.ld_map(Reg::R2, pa);
        a.mov64(Reg::R3, n);
        a.call(h::TAIL_CALL);
    }
    a.mov64(Reg::R0, ret);
    a.exit();
    verified(a.finish("target", t).unwrap())
}

fn hits(env: &Env) -> u64 {
    u64::from_le_bytes(
        env.map_get(1, &0u32.to_le_bytes())
            .unwrap()
            .try_into()
            .unwrap(),
    )
}

#[test]
fn tail_calls_jump_to_installed_targets() {
    let t = ProgramType::Xdp;
    let own = owner(t, 4, 2, true);
    let mut env = Env::new(&own);
    let pkt = [0u8; 64];
    // empty slot: falls through
    let r = run_xdp(&own, &mut env, &pkt).unwrap();
    assert_eq!((r.ret, r.tail_calls), (1, 0));
    // slot 2 -> target returning 3
    env.install_tail_call(0, 2, &target(t, 4, None, 3)).unwrap();
    let r = run_xdp(&own, &mut env, &pkt).unwrap();
    assert_eq!((r.ret, r.tail_calls, hits(&env)), (3, 1, 1));
    // slot 2 -> slot 3 -> slot 3 -> ...: a self tail call stops after
    // MAX_TAIL_CALL_CNT and returns from the last one
    env.install_tail_call(0, 2, &target(t, 4, Some(3), 5))
        .unwrap();
    env.install_tail_call(0, 3, &target(t, 4, Some(3), 6))
        .unwrap();
    let before = hits(&env);
    let r = run_xdp(&own, &mut env, &pkt).unwrap();
    assert_eq!(r.ret, 6);
    assert_eq!(r.tail_calls, MAX_TAIL_CALL_CNT);
    assert_eq!(hits(&env) - before, MAX_TAIL_CALL_CNT as u64);
    assert!(env.remove_tail_call(0, 2));
    assert_eq!(run_xdp(&own, &mut env, &pkt).unwrap().ret, 1);
}

#[test]
fn tail_call_install_errors_are_typed() {
    let own = owner(ProgramType::Xdp, 4, 0, true);
    let mut env = Env::new(&own);
    let tgt = target(ProgramType::Xdp, 4, None, 0);
    assert_eq!(
        env.install_tail_call(1, 0, &tgt),
        Err(TailCallError::NotProgArray { map: 1 })
    );
    assert_eq!(
        env.install_tail_call(0, 4, &tgt),
        Err(TailCallError::IndexOutOfRange {
            index: 4,
            max_entries: 4
        })
    );
    assert!(matches!(
        env.install_tail_call(0, 0, &target(ProgramType::SchedCls, 4, None, 0)),
        Err(TailCallError::TypeMismatch { .. })
    ));
    // a target whose program array has another size is another map
    assert!(matches!(
        env.install_tail_call(0, 0, &target(ProgramType::Xdp, 8, None, 0)),
        Err(TailCallError::MissingMap { .. })
    ));
}

#[cfg(target_os = "linux")]
mod kernel {
    use super::*;
    use vesper_ebpf::kernel::{self, LoadError};

    fn available() -> bool {
        match kernel::probe() {
            Ok(()) => true,
            Err(e) => {
                eprintln!("kernel BPF evidence unavailable on this host: {e}");
                false
            }
        }
    }

    fn ctx_in(meta: &SkbMeta) -> Vec<u8> {
        // only the fields BPF_PROG_TEST_RUN accepts from user space
        let m = SkbMeta {
            mark: meta.mark,
            priority: meta.priority,
            cb: meta.cb,
            ..SkbMeta::default()
        };
        m.to_ctx(0)
    }

    #[test]
    fn tc_field_reads_match_the_kernel() {
        if !available() {
            return;
        }
        let mut checked = 0;
        let mut wrong = Vec::new();
        for &(off, name) in SKB_FIELDS {
            let p = read_field(ProgramType::SchedCls, off, Size::W);
            let loaded = kernel::load(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
            for (i, pkt) in skb_packets().into_iter().enumerate() {
                let meta = SkbMeta {
                    mark: 0x1000 + i as u32,
                    priority: 0x2000 + i as u32,
                    cb: [i as u32, 11, 12, 13, 14],
                    ..SkbMeta::test_run(&pkt)
                };
                let k = kernel::test_run_ctx(&loaded.prog, &pkt, Some(&ctx_in(&meta)))
                    .unwrap_or_else(|e| panic!("{name} packet {i}: {e}"));
                let u = run_skb(&p, &mut Env::new(&p), &pkt, meta).unwrap();
                if k.retval as u64 != u.ret {
                    wrong.push(format!(
                        "__sk_buff.{name} on packet {i} ({:02x?}): kernel {:#x}, interpreter {:#x}",
                        &pkt[..14],
                        k.retval,
                        u.ret
                    ));
                }
                checked += 1;
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        eprintln!("{checked} __sk_buff field reads identical to BPF_PROG_TEST_RUN");
    }

    #[test]
    fn tc_writes_match_the_kernel() {
        if !available() {
            return;
        }
        let p = tc_writer();
        let loaded = kernel::load(&p).expect("loads");
        for len in [10usize, 14, 40, 100] {
            let pkt: Vec<u8> = (0..len as u8).collect();
            let meta = SkbMeta {
                mark: 3,
                priority: 4,
                cb: [10, 20, 30, 40, 50],
                ..SkbMeta::test_run(&pkt)
            };
            let k = match kernel::test_run_ctx(&loaded.prog, &pkt, Some(&ctx_in(&meta))) {
                Ok(k) => k,
                // the kernel refuses packets shorter than an Ethernet header
                Err(LoadError::Invalid(_)) if len < 14 => continue,
                Err(e) => panic!("{e}"),
            };
            let u = run_skb(&p, &mut Env::new(&p), &pkt, meta).unwrap();
            assert_eq!(
                (k.retval as u64, &k.packet),
                (u.ret, &u.packet),
                "len {len}"
            );
            // ctx_out carries mark, priority and cb[] back
            for at in [8usize, 32, 48, 52, 56, 60, 64] {
                assert_eq!(
                    u32_at(&k.ctx, at),
                    u32_at(&u.ctx, at),
                    "len {len} field {at}"
                );
            }
        }
    }

    /// Load `prog` against the maps of an owner.
    fn load_target(
        p: &Program,
        owner: &Program,
        maps: &[vesper_ebpf::kernel::Fd],
    ) -> kernel::Loaded {
        let fds: Vec<&kernel::Fd> = p
            .maps
            .iter()
            .map(|d| &maps[owner.maps.iter().position(|o| o == d).expect("shared map")])
            .collect();
        kernel::load_using(p, &fds).expect("target loads")
    }

    #[test]
    fn tail_calls_match_the_kernel() {
        if !available() {
            return;
        }
        for t in [ProgramType::Xdp, ProgramType::SchedCls] {
            let own = owner(t, 4, 2, true);
            let loaded = kernel::load(&own).expect("owner loads");
            let mut env = Env::new(&own);
            let pkt = eth([0; 6], 0x0800, 64);
            let mut targets = Vec::new();
            let run_both = |env: &mut Env| {
                let k = if t == ProgramType::Xdp {
                    kernel::test_run(&loaded.prog, &pkt)
                } else {
                    let meta = SkbMeta::test_run(&pkt);
                    kernel::test_run_ctx(&loaded.prog, &pkt, Some(&ctx_in(&meta)))
                }
                .expect("test run");
                let u = if t == ProgramType::Xdp {
                    run_xdp(&own, env, &pkt)
                } else {
                    run_skb(&own, env, &pkt, SkbMeta::test_run(&pkt))
                }
                .unwrap();
                let kh = kernel::lookup(&loaded.maps[1], &0u32.to_le_bytes())
                    .unwrap()
                    .unwrap();
                (
                    k.retval as u64,
                    u.ret,
                    u64::from_le_bytes(kh[..8].try_into().unwrap()),
                )
            };
            let (k, u, kh) = run_both(&mut env);
            assert_eq!((k, u, kh, hits(&env)), (1, 1, 0, 0), "{t:?} empty slot");
            for (slot, next, ret) in [(2u32, Some(3), 5), (3, Some(3), 6)] {
                let p = target(t, 4, next, ret);
                let l = load_target(&p, &own, &loaded.maps);
                kernel::update(
                    &loaded.maps[0],
                    &slot.to_le_bytes(),
                    &(l.prog.raw() as u32).to_ne_bytes(),
                )
                .expect("install");
                env.install_tail_call(0, slot, &p).unwrap();
                targets.push(l);
            }
            let (k, u, kh) = run_both(&mut env);
            assert_eq!(k, u, "{t:?}");
            assert_eq!(k, 6, "{t:?}");
            assert_eq!(
                kh,
                hits(&env),
                "{t:?}: the kernel and the interpreter count {kh} and {} target runs",
                hits(&env)
            );
            assert_eq!(kh, MAX_TAIL_CALL_CNT as u64, "{t:?}");
        }
    }
}
