//! The port interpreter against the Rust interpreter, run for run: every
//! corpus, kfunc and depth case and random programs as every program type,
//! and "chaos" programs that reach every helper, kfunc, fault and tail
//! call, with mutated copies of them. Each pair of environments runs the
//! same inputs in turn, so map entries, values, spilled pointers and ring
//! buffers carry over between runs; after every run both builds must give
//! the same result or fault, and the same map keys and values, ring bytes,
//! producer and prandom state. Port wave P3a lane 6 (#1041).
#![cfg(feature = "vesper-port")]

mod common;

use common::{GenOptions, Rng, random_packet, random_program, retarget};
use std::panic::{AssertUnwindSafe, catch_unwind};
use vesper_ebpf::helpers as h;
use vesper_ebpf::insn::{AluOp, AtomicOp, JmpOp, RawInsn, Reg, Size};
use vesper_ebpf::interp::{Env, Input, ListeningSocket, SkbMeta, Task, run};
use vesper_ebpf::verifier::PT_REGS_SIZE;
use vesper_ebpf::{Asm, MapDef, MapKind, Program, ProgramType};
use vesper_ebpf_rust as rust;

fn to_rust(p: &Program) -> rust::Program {
    rust::Program {
        name: p.name.clone(),
        prog_type: rust::ProgramType::from_kernel_type(p.prog_type.kernel_type()).unwrap(),
        insns: p
            .insns
            .iter()
            .map(|i| rust::insn::RawInsn {
                code: i.code,
                dst: i.dst,
                src: i.src,
                off: i.off,
                imm: i.imm,
            })
            .collect(),
        maps: p
            .maps
            .iter()
            .map(|m| rust::MapDef {
                name: m.name.clone(),
                kind: rust::MapKind::from_kernel_type(m.kind.kernel_type()).unwrap(),
                key_size: m.key_size,
                value_size: m.value_size,
                max_entries: m.max_entries,
                flags: m.flags,
            })
            .collect(),
        map_relocs: p.map_relocs.clone(),
        functions: p.functions.clone(),
        license: p.license.clone(),
        attach: p.attach.clone(),
        kfuncs: p
            .kfuncs
            .iter()
            .map(|k| rust::program::KfuncRef {
                slot: k.slot,
                name: k.name.clone(),
            })
            .collect(),
        core_relos: Vec::new(),
        lines: Vec::new(),
    }
}

/// The same environment in both builds.
struct Pair {
    p: Env,
    r: rust::interp::Env,
}

#[derive(Default)]
struct Stats {
    runs: u64,
    ok: u64,
    faults: std::collections::BTreeSet<String>,
    details: std::collections::BTreeSet<String>,
    rust_panics: u64,
}

fn pair(rng: &mut Rng, prog: &Program, budget: u64) -> Pair {
    let mut p = Env::new(prog);
    let mut r = rust::interp::Env::new(&to_rust(prog));
    let cpu = rng.below(4) as u32;
    let now = rng.next();
    let seed = rng.next() | 1;
    let comm = rng.pick(&["vesper", "", "a-much-longer-task-name", "ü"]);
    let (pid, tgid) = (rng.below(5000) as u32, rng.below(5000) as u32);
    p.cpu = cpu;
    r.cpu = cpu;
    p.now_ns = now;
    r.now_ns = now;
    p.prandom_state = seed;
    r.prandom_state = seed;
    p.insn_budget = budget;
    r.insn_budget = budget;
    p.task = Task::named(comm, pid, tgid);
    r.task = rust::interp::Task::named(comm, pid, tgid);
    p.task.uid = 7;
    r.task.uid = 7;
    p.task.gid = 9;
    r.task.gid = 9;
    if rng.chance(90) {
        p.sockets.push(ListeningSocket {
            daddr: [127, 0, 0, 1],
            dport: [0, 80],
        });
        r.sockets.push(rust::interp::ListeningSocket {
            daddr: [127, 0, 0, 1],
            dport: [0, 80],
        });
    }
    for _ in 0..rng.below(3) {
        let daddr = rng.pick(&[[127, 0, 0, 1], [10, 0, 0, 2], [0, 0, 0, 0]]);
        let dport = rng.pick(&[[0, 80], [0x1f, 0x90], [0, 0]]);
        p.sockets.push(ListeningSocket { daddr, dport });
        r.sockets
            .push(rust::interp::ListeningSocket { daddr, dport });
    }
    Pair { p, r }
}

/// Map keys and values, ring bytes and positions, and the prandom state.
fn state_port(prog: &Program, e: &Env) -> String {
    let mut s = format!("prandom {}\n", e.prandom_state);
    for m in 0..prog.maps.len() {
        match e.ring(m) {
            Some(r) => s += &format!("ring {m} {:?} {} {}\n", r.data, r.producer, r.consumer),
            None => {
                for k in e.map_keys(m) {
                    s += &format!("{m} {k:?} {:?}\n", e.map_get(m, &k));
                }
            }
        }
    }
    s
}

fn state_rust(prog: &Program, e: &rust::interp::Env) -> String {
    let mut s = format!("prandom {}\n", e.prandom_state);
    for m in 0..prog.maps.len() {
        match e.ring(m) {
            Some(r) => s += &format!("ring {m} {:?} {} {}\n", r.data, r.producer, r.consumer),
            None => {
                for k in e.map_keys(m) {
                    s += &format!("{m} {k:?} {:?}\n", e.map_get(m, &k));
                }
            }
        }
    }
    s
}

/// An input for `t` (now and then one of the wrong size).
fn input(rng: &mut Rng, t: ProgramType) -> (u8, Vec<u8>, SkbMeta) {
    let mut meta = SkbMeta::default();
    let bytes = match t {
        ProgramType::Xdp => random_packet(rng),
        ProgramType::SchedCls => {
            let p = random_packet(rng);
            meta = SkbMeta::test_run(&p);
            meta.mark = rng.next() as u32;
            meta.priority = rng.below(8) as u32;
            meta.cb[rng.below(5) as usize] = rng.next() as u32;
            p
        }
        ProgramType::Tracepoint => {
            let n = if rng.chance(5) {
                2049 + rng.below(10)
            } else {
                rng.below(300)
            };
            rng.bytes(n as usize)
        }
        ProgramType::Kprobe => {
            let n = if rng.chance(5) {
                rng.below(400)
            } else {
                PT_REGS_SIZE as u64
            };
            rng.bytes(n as usize)
        }
    };
    let kind = match t {
        ProgramType::Xdp => 0,
        ProgramType::SchedCls => 1,
        ProgramType::Tracepoint => 2,
        ProgramType::Kprobe => 3,
    };
    (kind, bytes, meta)
}

fn rust_meta(m: &SkbMeta) -> rust::interp::SkbMeta {
    rust::interp::SkbMeta {
        pkt_type: m.pkt_type,
        mark: m.mark,
        queue_mapping: m.queue_mapping,
        protocol: m.protocol,
        vlan_tci: m.vlan_tci,
        vlan_proto: m.vlan_proto,
        priority: m.priority,
        ingress_ifindex: m.ingress_ifindex,
        ifindex: m.ifindex,
        tc_index: m.tc_index,
        cb: m.cb,
        hash: m.hash,
        tc_classid: m.tc_classid,
    }
}

/// Run `prog` (as type `t`, which may differ from the environment's) in
/// both builds and compare. False when the Rust build panicked (an
/// arithmetic overflow on an absurd helper size): the pair is then stale.
fn agree(
    rng: &mut Rng,
    prog: &Program,
    rprog: &rust::Program,
    e: &mut Pair,
    st: &mut Stats,
    what: &str,
) -> bool {
    let (kind, bytes, meta) = input(rng, prog.prog_type);
    let rmeta = rust_meta(&meta);
    let rr = catch_unwind(AssertUnwindSafe(|| {
        let i = match kind {
            0 => rust::interp::Input::Xdp(&bytes),
            1 => rust::interp::Input::Skb {
                packet: &bytes,
                meta: rmeta.clone(),
            },
            2 => rust::interp::Input::Tracepoint(&bytes),
            _ => rust::interp::Input::Kprobe(&bytes),
        };
        format!("{:?}", rust::interp::run(rprog, &mut e.r, i))
    }));
    let Ok(rr) = rr else {
        st.rust_panics += 1;
        return false;
    };
    let i = match kind {
        0 => Input::Xdp(&bytes),
        1 => Input::Skb {
            packet: &bytes,
            meta,
        },
        2 => Input::Tracepoint(&bytes),
        _ => Input::Kprobe(&bytes),
    };
    let pr = format!("{:?}", run(prog, &mut e.p, i));
    assert_eq!(pr, rr, "{what}: result");
    assert_eq!(
        state_port(prog, &e.p),
        state_rust(prog, &e.r),
        "{what}: state after {pr}"
    );
    st.runs += 1;
    if std::env::var_os("INTERP_PORT_FAULTS").is_some() {
        eprintln!("{what}: {}", &pr[..pr.len().min(120)]);
    }
    if pr.starts_with("Ok") {
        st.ok += 1;
    } else {
        if let Some(d) = pr.split("detail: \"").nth(1) {
            st.details
                .insert(d.split('"').next().unwrap_or("").to_owned());
        }
        st.faults
            .insert(pr.split(['(', ' ', '{']).nth(1).unwrap_or("").to_owned());
    }
    // a reader drains the rings now and then
    if rng.chance(50) {
        for m in 0..prog.maps.len() {
            if let (Some(a), Some(b)) = (e.p.ring_mut(m), e.r.ring_mut(m)) {
                a.consumer = a.producer;
                b.consumer = b.producer;
            }
        }
    }
    let now = rng.below(1 << 40);
    e.p.now_ns = now;
    e.r.now_ns = now;
    true
}

/// `runs` runs of `prog` over one pair of environments, with `targets`
/// installed in map `pa` (if any).
fn session(
    rng: &mut Rng,
    prog: &Program,
    targets: &[(usize, u32, Program)],
    runs: u64,
    budget: u64,
    st: &mut Stats,
    what: &str,
) {
    let rprog = to_rust(prog);
    let mut e = pair(rng, prog, budget);
    for (m, idx, t) in targets {
        let a = format!("{:?}", e.p.install_tail_call(*m, *idx, t));
        let b = format!("{:?}", e.r.install_tail_call(*m, *idx, &to_rust(t)));
        assert_eq!(a, b, "{what}: install");
    }
    for k in 0..runs {
        if !agree(rng, prog, &rprog, &mut e, st, &format!("{what} run {k}")) {
            return;
        }
        if !targets.is_empty() && rng.chance(10) {
            let (m, idx, _) = &targets[rng.below(targets.len() as u64) as usize];
            assert_eq!(
                e.p.remove_tail_call(*m, *idx),
                e.r.remove_tail_call(*m, *idx)
            );
        }
    }
}

#[test]
fn port_matches_rust_on_corpus_and_random_programs() {
    let mut rng = Rng(0x0001_7e49_1041);
    let mut st = Stats::default();
    let mut progs: Vec<(String, Program)> = Vec::new();
    for c in common::corpus::corpus() {
        progs.push((c.name.to_owned(), c.prog));
    }
    for c in common::depth::cases() {
        progs.push((c.name.to_owned(), c.prog));
    }
    for i in 0..150 {
        let opts = GenOptions {
            items: 10 + rng.below(20),
            nondeterministic_helpers: true,
        };
        progs.push((format!("random {i}"), random_program(&mut rng, opts)));
    }
    for (name, prog) in &progs {
        let types = [
            ProgramType::Xdp,
            ProgramType::SchedCls,
            ProgramType::Tracepoint,
            ProgramType::Kprobe,
        ];
        for t in types {
            let p = if t == prog.prog_type {
                prog.clone()
            } else if rng.chance(30) {
                retarget(prog, t)
            } else {
                continue;
            };
            session(
                &mut rng,
                &p,
                &[],
                5,
                100_000,
                &mut st,
                &format!("{name} as {t:?}"),
            );
        }
    }
    eprintln!(
        "{} runs, {} ok, faults {:?}, {} rust panics",
        st.runs, st.ok, st.faults, st.rust_panics
    );
    assert!(st.runs > 1000 && st.ok > 300, "{} {}", st.runs, st.ok);
    assert!(st.faults.len() >= 6, "{:?}", st.faults);
}

const REGS: [Reg; 10] = [
    Reg::R0,
    Reg::R1,
    Reg::R2,
    Reg::R3,
    Reg::R4,
    Reg::R5,
    Reg::R6,
    Reg::R7,
    Reg::R8,
    Reg::R9,
];

const SIZES: [Size; 4] = [Size::B, Size::H, Size::W, Size::DW];

/// The chaos programs' maps: hash, array, ring buffer, program array, a
/// per-CPU array and an LRU hash with 8-byte keys.
fn chaos_maps(a: &mut Asm) {
    a.map(MapDef::new("hm", MapKind::Hash, 4, 8, 6)).unwrap();
    a.map(MapDef::new("am", MapKind::Array, 4, 16, 4)).unwrap();
    a.map(MapDef::ringbuf("rb", 4096)).unwrap();
    a.map(MapDef::prog_array("jmp", 4)).unwrap();
    a.map(MapDef::new("pc", MapKind::PerCpuArray, 4, 8, 2))
        .unwrap();
    a.map(MapDef::new("lru", MapKind::LruHash, 8, 8, 3))
        .unwrap();
}

/// A register that is set on most paths (r0, r6..r9), now and then any.
fn safe(rng: &mut Rng) -> Reg {
    if rng.chance(85) {
        rng.pick(&[Reg::R0, Reg::R6, Reg::R7, Reg::R8, Reg::R9])
    } else {
        rng.pick(&REGS)
    }
}

fn small(rng: &mut Rng) -> i32 {
    match rng.below(6) {
        0 => 0,
        1 => 1,
        2 => -1,
        3 => rng.below(8) as i32,
        4 => rng.below(300) as i32 - 150,
        _ => rng.next() as i32,
    }
}

/// One item of a chaos program. A wild item may pass any map, size or
/// pointer; a clean one keeps to arguments that work for `t`, so state
/// builds up over a run.
fn chaos_item(
    rng: &mut Rng,
    a: &mut Asm,
    t: ProgramType,
    wild: bool,
    cb: vesper_ebpf::program::Label,
    sub: vesper_ebpf::program::Label,
) {
    let tracing = matches!(t, ProgramType::Tracepoint | ProgramType::Kprobe);
    let r = if !wild || rng.chance(80) {
        rng.pick(&[Reg::R0, Reg::R7, Reg::R9])
    } else {
        rng.pick(&REGS)
    };
    let misalign = if wild && rng.chance(10) { 3 } else { 0 };
    let fp_off = -(8 * (1 + rng.below(8) as i16)) - misalign;
    match rng.below(26) {
        0..=3 => {
            let op = rng.pick(&[
                AluOp::Add,
                AluOp::Sub,
                AluOp::Mul,
                AluOp::Div,
                AluOp::Or,
                AluOp::And,
                AluOp::Lsh,
                AluOp::Rsh,
                AluOp::Mod,
                AluOp::Xor,
                AluOp::Mov,
                AluOp::Arsh,
                AluOp::Neg,
                AluOp::SDiv,
                AluOp::SMod,
            ]);
            if rng.chance(50) {
                let v = small(rng);
                if rng.chance(70) {
                    a.alu64(op, r, v);
                } else {
                    a.alu32(op, r, v);
                }
            } else {
                let s = if wild {
                    safe(rng)
                } else {
                    rng.pick(&[Reg::R0, Reg::R7, Reg::R9])
                };
                a.alu64(op, r, s);
            }
        }
        4 => {
            a.mov64(r, Reg::R10);
            a.add64(r, small(rng) % 600);
        }
        5 => {
            // a context pointer: r1 at entry, saved in r6
            a.mov64(r, Reg::R6);
            if rng.chance(50) {
                a.add64(r, rng.below(200) as i32);
            }
        }
        6 => {
            let (size, off) = if wild {
                (
                    rng.pick(&SIZES),
                    rng.pick(&[0, 4, 8, 12, 16, 24, 48, 76, 80, 100, 160]),
                )
            } else {
                match t {
                    ProgramType::Xdp => (Size::W, rng.pick(&[0, 4, 12, 16])),
                    ProgramType::SchedCls => (Size::W, rng.pick(&[0, 4, 8, 16, 48, 76, 80])),
                    ProgramType::Tracepoint => (rng.pick(&SIZES), rng.pick(&[8, 16, 24, 96])),
                    ProgramType::Kprobe => (rng.pick(&SIZES), rng.pick(&[0, 8, 16, 80])),
                }
            };
            a.ldx(size, r, Reg::R6, off);
        }
        7 => {
            let size = rng.pick(&SIZES);
            a.ldx(size, r, Reg::R10, fp_off);
        }
        8 => {
            let size = if wild { rng.pick(&SIZES) } else { Size::DW };
            let s = safe(rng);
            a.stx(size, Reg::R10, fp_off, s);
        }
        9 => {
            let size = rng.pick(&SIZES);
            a.st(size, Reg::R10, fp_off, small(rng));
        }
        10 => {
            if wild {
                let size = rng.pick(&SIZES);
                let off = rng.pick(&[8, 32, 44, 48, 72, 0, 76]);
                if rng.chance(50) {
                    a.st(size, Reg::R6, off, small(rng));
                } else {
                    let s = safe(rng);
                    a.stx(size, Reg::R6, off, s);
                }
            } else if t == ProgramType::SchedCls {
                a.st(Size::W, Reg::R6, rng.pick(&[8, 32, 44, 48, 72]), small(rng));
            }
        }
        11 | 12 => {
            // a map helper: key at fp-8, value at fp-32
            let m = if wild {
                rng.pick(&[0usize, 1, 4, 5, 2, 3])
            } else {
                rng.pick(&[0usize, 1, 4, 5])
            };
            a.st(Size::DW, Reg::R10, -8, rng.below(7) as i32);
            if rng.chance(70) {
                a.st(Size::DW, Reg::R10, -32, small(rng));
                a.st(Size::DW, Reg::R10, -24, small(rng));
            }
            a.ld_map(Reg::R1, m);
            a.mov64(Reg::R2, Reg::R10);
            a.add64(Reg::R2, -8);
            a.mov64(Reg::R3, Reg::R10);
            a.add64(Reg::R3, -32);
            a.mov64(Reg::R4, rng.pick(&[0, 0, 1, 2, if wild { 3 } else { 0 }]));
            a.call(rng.pick(&[
                h::MAP_LOOKUP_ELEM,
                h::MAP_LOOKUP_ELEM,
                h::MAP_UPDATE_ELEM,
                h::MAP_UPDATE_ELEM,
                h::MAP_DELETE_ELEM,
            ]));
            if rng.chance(60) {
                // use the value pointer
                let skip = a.label();
                a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
                let size = rng.pick(&SIZES);
                let off = rng.pick(&[0i16, 8]);
                match rng.below(4) {
                    0 => {
                        a.ldx(size, Reg::R7, Reg::R0, off);
                    }
                    1 => {
                        a.st(size, Reg::R0, off, small(rng));
                    }
                    2 => {
                        // spill a pointer into the map value
                        let s = rng.pick(&[Reg::R10, Reg::R6, Reg::R0]);
                        a.stx(Size::DW, Reg::R0, off, s);
                    }
                    _ => {
                        a.mov64(Reg::R1, 1);
                        let op = rng.pick(&[
                            AtomicOp::Add { fetch: true },
                            AtomicOp::Xchg,
                            AtomicOp::CmpXchg,
                            AtomicOp::Or { fetch: false },
                        ]);
                        a.atomic(rng.pick(&[Size::W, Size::DW]), op, Reg::R0, off, Reg::R1);
                    }
                }
                if rng.chance(50) {
                    a.mov64(Reg::R8, Reg::R0);
                }
                a.bind(skip).unwrap();
            }
        }
        13 => {
            // a pointer read back from a map value (r8 is the last one)
            let skip = a.label();
            if !wild {
                a.jmp(JmpOp::Lt, Reg::R8, 4096, skip);
            }
            a.ldx(Size::DW, Reg::R9, Reg::R8, rng.pick(&[0, 8]));
            if rng.chance(50) {
                a.ldx(Size::B, Reg::R9, Reg::R9, 0);
            }
            a.bind(skip).unwrap();
        }
        14 => {
            let m = if wild {
                rng.pick(&[2usize, 2, 2, 0])
            } else {
                2
            };
            a.ld_map(Reg::R1, m);
            a.mov64(Reg::R2, rng.pick(&[0, 8, 13, 64, 200, 1500, 4090, 5000]));
            a.mov64(Reg::R3, 0);
            a.call(h::RINGBUF_RESERVE);
            let skip = a.label();
            a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
            if rng.chance(60) {
                a.st(Size::B, Reg::R0, 0, small(rng));
            }
            if !wild || rng.chance(85) {
                a.mov64(Reg::R1, Reg::R0);
                if wild && rng.chance(5) {
                    a.add64(Reg::R1, 1);
                }
                a.mov64(Reg::R2, 0);
                a.call(rng.pick(&[h::RINGBUF_SUBMIT, h::RINGBUF_DISCARD]));
            }
            a.bind(skip).unwrap();
        }
        15 => {
            a.ld_map(Reg::R1, if wild { rng.pick(&[2usize, 2, 1]) } else { 2 });
            a.mov64(Reg::R2, Reg::R10);
            a.add64(Reg::R2, -32);
            a.mov64(Reg::R3, rng.pick(&[8, 16, 24, 32]));
            a.mov64(Reg::R4, 0);
            a.call(h::RINGBUF_OUTPUT);
        }
        16 => {
            a.mov64(Reg::R1, Reg::R6);
            a.ld_map(Reg::R2, if wild { rng.pick(&[3usize, 3, 0]) } else { 3 });
            a.mov64(Reg::R3, rng.below(6) as i32);
            a.call(h::TAIL_CALL);
        }
        17 => {
            let n = if wild {
                rng.pick(&[0, 1, 3, 9, 100000])
            } else {
                rng.pick(&[0, 1, 3, 9])
            };
            a.mov64(Reg::R1, n);
            if !wild || rng.chance(90) {
                a.ld_func(Reg::R2, cb);
            } else {
                a.mov64(Reg::R2, 5);
            }
            a.mov64(Reg::R3, Reg::R10);
            a.add64(Reg::R3, -16);
            a.mov64(Reg::R4, 0);
            a.call(h::LOOP);
        }
        18 => {
            if !wild || rng.chance(70) {
                a.mov64(Reg::R1, small(rng));
            }
            a.call_local(sub);
        }
        19 => {
            if wild {
                a.mov64(Reg::R1, Reg::R10);
                a.add64(Reg::R1, rng.pick(&[-8, -16, -24, -40]));
                a.mov64(Reg::R2, small(rng) % 4);
                a.mov64(Reg::R3, small(rng) % 6);
                a.call_kfunc(rng.pick(&[
                    "bpf_iter_num_new",
                    "bpf_iter_num_next",
                    "bpf_iter_num_destroy",
                    "bpf_local_irq_save",
                    "bpf_local_irq_restore",
                    "bpf_rcu_read_lock",
                    "bpf_preempt_disable",
                    "bpf_not_a_kfunc",
                ]));
            } else {
                let fp = |a: &mut Asm| {
                    a.mov64(Reg::R1, Reg::R10);
                    a.add64(Reg::R1, -48);
                };
                fp(a);
                a.mov64(Reg::R2, small(rng) % 4);
                a.mov64(Reg::R3, small(rng) % 6);
                a.call_kfunc("bpf_iter_num_new");
                for _ in 0..rng.below(4) {
                    fp(a);
                    a.call_kfunc("bpf_iter_num_next");
                    let skip = a.label();
                    a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
                    a.ldx(Size::W, Reg::R7, Reg::R0, 0);
                    a.bind(skip).unwrap();
                }
                fp(a);
                a.call_kfunc("bpf_iter_num_destroy");
                a.mov64(Reg::R1, Reg::R10);
                a.add64(Reg::R1, -56);
                a.call_kfunc("bpf_local_irq_save");
                a.call_kfunc("bpf_rcu_read_lock");
                a.call_kfunc("bpf_rcu_read_unlock");
                a.mov64(Reg::R1, Reg::R10);
                a.add64(Reg::R1, -56);
                a.call_kfunc("bpf_local_irq_restore");
            }
        }
        20 => {
            if !wild && tracing {
                return;
            }
            // a socket tuple at fp-16
            a.st(
                Size::W,
                Reg::R10,
                -12,
                rng.pick(&[0x0100_007f, 0x0200_000a, 0]),
            );
            a.st(Size::H, Reg::R10, -6, rng.pick(&[0x5000, 0x901f]));
            a.mov64(Reg::R1, Reg::R6);
            a.mov64(Reg::R2, Reg::R10);
            a.add64(Reg::R2, -16);
            a.mov64(Reg::R3, if wild { rng.pick(&[12, 12, 36, 8]) } else { 12 });
            a.mov64(Reg::R4, -1);
            a.mov64(Reg::R5, 0);
            a.call(h::SK_LOOKUP_TCP);
            if !wild || rng.chance(85) {
                let skip = a.label();
                a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
                a.mov64(Reg::R1, Reg::R0);
                a.call(h::SK_RELEASE);
                a.bind(skip).unwrap();
            }
        }
        21 => {
            if !wild && !tracing {
                return;
            }
            a.mov64(Reg::R1, Reg::R10);
            a.add64(Reg::R1, rng.pick(&[-16, -24, -13]));
            a.mov64(
                Reg::R2,
                if wild {
                    rng.pick(&[0, 1, 4, 16, 17])
                } else {
                    rng.pick(&[1, 4, 16])
                },
            );
            a.call(h::GET_CURRENT_COMM);
        }
        22 => {
            let id = if wild || tracing {
                rng.pick(&[
                    h::GET_CURRENT_PID_TGID,
                    h::GET_CURRENT_UID_GID,
                    h::GET_PRANDOM_U32,
                    h::KTIME_GET_NS,
                    h::GET_SMP_PROCESSOR_ID,
                    if wild { 999 } else { h::GET_PRANDOM_U32 },
                ])
            } else {
                rng.pick(&[h::GET_PRANDOM_U32, h::KTIME_GET_NS, h::GET_SMP_PROCESSOR_ID])
            };
            a.call(id);
        }
        23 => {
            if !wild && tracing {
                return;
            }
            if t == ProgramType::Xdp || (wild && rng.chance(20)) {
                a.mov64(Reg::R1, Reg::R6);
                a.mov64(Reg::R2, rng.pick(&[-20, -4, 0, 4, 14, 400, -300]));
                a.call(h::XDP_ADJUST_HEAD);
            }
            // packet bytes through data/data_end
            let (d, e) = if t == ProgramType::SchedCls {
                (76, 80)
            } else {
                (0, 4)
            };
            a.ldx(Size::W, Reg::R2, Reg::R6, d);
            a.ldx(Size::W, Reg::R3, Reg::R6, e);
            let out = a.label();
            a.mov64(Reg::R4, Reg::R2);
            a.add64(Reg::R4, 8);
            if !wild || rng.chance(80) {
                a.jmp(JmpOp::Gt, Reg::R4, Reg::R3, out);
            }
            let off = if wild {
                rng.pick(&[0, 4, -1, -2])
            } else {
                rng.pick(&[0, 4])
            };
            a.ldx(
                rng.pick(&[Size::B, Size::H, Size::W]),
                Reg::R7,
                Reg::R2,
                off,
            );
            if rng.chance(30) {
                a.st(Size::B, Reg::R2, 1, small(rng));
            }
            a.bind(out).unwrap();
        }
        24 => {
            let size = rng.pick(&[Size::W, Size::DW]);
            let op = rng.pick(&[
                AtomicOp::Add { fetch: false },
                AtomicOp::Add { fetch: true },
                AtomicOp::And { fetch: true },
                AtomicOp::Xor { fetch: false },
                AtomicOp::Xchg,
                AtomicOp::CmpXchg,
            ]);
            let s = safe(rng);
            let base = if wild {
                rng.pick(&[Reg::R10, Reg::R10, Reg::R6])
            } else {
                Reg::R10
            };
            a.atomic(size, op, base, if base == Reg::R10 { fp_off } else { 8 }, s);
        }
        _ => {
            let l = a.label();
            let op = rng.pick(&[JmpOp::Eq, JmpOp::Ne, JmpOp::Gt, JmpOp::SLt, JmpOp::Set]);
            let s = safe(rng);
            if rng.chance(50) {
                a.jmp(op, r, small(rng), l);
            } else {
                a.jmp32(op, r, s, l);
            }
            a.mov64(Reg::R0, small(rng));
            a.bind(l).unwrap();
        }
    }
}

/// A program of `t` over the chaos maps that reaches every helper, kfunc
/// and fault; `None` when assembly fails.
fn chaos(rng: &mut Rng, t: ProgramType, items: u64, wild: bool) -> Option<Program> {
    let mut a = Asm::new();
    chaos_maps(&mut a);
    let cb = a.label();
    let sub = a.label();
    a.mov64(Reg::R6, Reg::R1);
    for r in [Reg::R0, Reg::R7, Reg::R8, Reg::R9] {
        if rng.chance(90) {
            a.mov64(r, small(rng));
        }
    }
    for k in 0..8 {
        a.st(Size::DW, Reg::R10, -8 * (k + 1), 0);
    }
    for _ in 0..items {
        chaos_item(rng, &mut a, t, wild, cb, sub);
    }
    a.mov64(Reg::R0, rng.below(4) as i32);
    if wild && rng.chance(10) {
        a.mov64(Reg::R0, Reg::R10);
    }
    a.exit();
    // the bpf_loop callback: r1 = index, r2 = argument
    a.bind(cb).unwrap();
    if rng.chance(50) {
        a.stx(Size::DW, Reg::R10, -8, Reg::R1);
        a.ldx(Size::DW, Reg::R3, Reg::R10, -8);
    }
    if rng.chance(30) {
        a.st(Size::DW, Reg::R2, 0, 7);
    }
    a.mov64(Reg::R0, Reg::R1);
    a.alu64(AluOp::And, Reg::R0, rng.pick(&[0, 1, 4]));
    if wild && rng.chance(10) {
        a.call_local(cb);
    }
    a.exit();
    // a subprogram: writes its own stack and returns r1 + 1
    a.bind(sub).unwrap();
    if rng.chance(50) {
        a.st(Size::DW, Reg::R10, -8, 3);
        a.ldx(Size::DW, Reg::R0, Reg::R10, rng.pick(&[-8, -16]));
    }
    if wild && rng.chance(20) {
        a.call_local(sub);
    }
    a.mov64(Reg::R0, Reg::R1);
    a.add64(Reg::R0, 1);
    a.exit();
    a.finish("chaos", t).ok()
}

/// `prog` with a few slots changed at random.
fn mutate(rng: &mut Rng, prog: &Program) -> Program {
    let mut p = prog.clone();
    for _ in 0..1 + rng.below(3) {
        let at = rng.below(p.insns.len() as u64) as usize;
        let i = &mut p.insns[at];
        match rng.below(5) {
            0 => i.code = rng.next() as u8,
            1 => i.dst = rng.below(12) as u8,
            2 => i.src = rng.below(12) as u8,
            3 => i.off = small(rng) as i16,
            _ => i.imm = small(rng),
        }
    }
    if rng.chance(10) {
        p.insns.push(RawInsn {
            code: 0x18,
            dst: 1,
            src: rng.pick(&[0, 1, 2, 4, 5]),
            off: 0,
            imm: rng.pick(&[0, 9, -1]),
        });
    }
    p
}

#[test]
fn port_matches_rust_on_chaos_programs() {
    let mut rng = Rng(0x00c4_a057_1041);
    let mut st = Stats::default();
    let types = [
        ProgramType::Xdp,
        ProgramType::SchedCls,
        ProgramType::Tracepoint,
        ProgramType::Kprobe,
    ];
    for i in 0..1500 {
        let t = rng.pick(&types);
        let items = 3 + rng.below(14);
        let wild = rng.chance(35);
        let Some(prog) = chaos(&mut rng, t, items, wild) else {
            continue;
        };
        let mut targets = Vec::new();
        for _ in 0..rng.below(4) {
            let tt = if rng.chance(90) { t } else { rng.pick(&types) };
            let items = 3 + rng.below(8);
            if let Some(tp) = chaos(&mut rng, tt, items, wild) {
                let map = if rng.chance(95) { 3 } else { 0 };
                targets.push((map, rng.below(5) as u32, tp));
            }
        }
        let budget = rng.pick(&[50, 400, 5000, 20_000]);
        session(
            &mut rng,
            &prog,
            &targets,
            10,
            budget,
            &mut st,
            &format!("chaos {i}"),
        );
        for k in 0..rng.below(2) {
            let m = mutate(&mut rng, &prog);
            session(
                &mut rng,
                &m,
                &targets,
                3,
                budget,
                &mut st,
                &format!("chaos {i} mutation {k}"),
            );
        }
    }
    eprintln!(
        "{} runs, {} ok, faults {:?}, {} rust panics",
        st.runs, st.ok, st.faults, st.rust_panics
    );
    assert!(st.runs > 15_000 && st.ok > 3000, "{} {}", st.runs, st.ok);
    assert!(st.faults.len() >= 9, "{:?}", st.faults);
}

/// Pointers that outlive their run in map values (a dead stack, a map value
/// renumbered when another entry is deleted), socket and context buffers,
/// and tail calls from a subprogram: each case runs eight times over one
/// pair and must reach the fault it is written for.
#[test]
fn port_matches_rust_on_pointer_lifetimes() {
    let mut rng = Rng(0x00de_ad00_1041);
    let mut st = Stats::default();
    let lookup = |a: &mut Asm, m: usize, key: i32| {
        a.st(Size::DW, Reg::R10, -8, key);
        a.ld_map(Reg::R1, m);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -8);
        a.call(h::MAP_LOOKUP_ELEM);
    };
    let update = |a: &mut Asm, m: usize, key: i32| {
        a.st(Size::DW, Reg::R10, -8, key);
        a.st(Size::DW, Reg::R10, -32, key);
        a.st(Size::DW, Reg::R10, -24, 0);
        a.ld_map(Reg::R1, m);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -8);
        a.mov64(Reg::R3, Reg::R10);
        a.add64(Reg::R3, -32);
        a.mov64(Reg::R4, 0);
        a.call(h::MAP_UPDATE_ELEM);
    };
    let delete = |a: &mut Asm, m: usize, key: i32| {
        a.st(Size::DW, Reg::R10, -8, key);
        a.ld_map(Reg::R1, m);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -8);
        a.call(h::MAP_DELETE_ELEM);
    };
    let mut cases: Vec<(&str, Program)> = Vec::new();
    // am[0] keeps the last run's frame pointer, then hm[k] pointers
    for (name, spill_hash) in [("dead stack", false), ("renumbered value", true)] {
        let mut a = Asm::new();
        chaos_maps(&mut a);
        let skip = a.label();
        a.mov64(Reg::R6, Reg::R1);
        // churn the hash map so its values move
        update(&mut a, 0, 3);
        update(&mut a, 0, 4);
        delete(&mut a, 0, 3);
        if spill_hash {
            lookup(&mut a, 0, 4);
            a.mov64(Reg::R7, Reg::R0);
        } else {
            a.mov64(Reg::R7, Reg::R10);
        }
        lookup(&mut a, 1, 0);
        a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
        a.mov64(Reg::R8, Reg::R0);
        a.ldx(Size::DW, Reg::R9, Reg::R8, 0);
        a.stx(Size::DW, Reg::R8, 0, Reg::R7);
        a.jmp(JmpOp::Eq, Reg::R9, 0, skip);
        a.ldx(Size::B, Reg::R1, Reg::R9, if spill_hash { 0 } else { -1 });
        a.mov64(Reg::R0, Reg::R1);
        a.exit();
        a.bind(skip).unwrap();
        a.mov64(Reg::R0, 0);
        a.exit();
        cases.push((name, a.finish(name, ProgramType::Xdp).unwrap()));
    }
    // a socket's fields, and a socket released twice
    for twice in [false, true] {
        let mut a = Asm::new();
        chaos_maps(&mut a);
        let skip = a.label();
        a.st(Size::DW, Reg::R10, -16, 0);
        a.st(Size::DW, Reg::R10, -8, 0);
        a.st(Size::W, Reg::R10, -12, 0x0100_007f);
        a.st(Size::H, Reg::R10, -6, 0x5000);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -16);
        a.mov64(Reg::R3, 12);
        a.mov64(Reg::R4, -1);
        a.mov64(Reg::R5, 0);
        a.call(h::SK_LOOKUP_TCP);
        a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
        a.mov64(Reg::R6, Reg::R0);
        if !twice {
            a.ldx(Size::W, Reg::R2, Reg::R6, 0);
        }
        a.mov64(Reg::R1, Reg::R6);
        a.call(h::SK_RELEASE);
        if twice {
            a.mov64(Reg::R1, Reg::R6);
            a.call(h::SK_RELEASE);
        }
        a.bind(skip).unwrap();
        a.mov64(Reg::R0, 0);
        a.exit();
        cases.push(("socket", a.finish("socket", ProgramType::SchedCls).unwrap()));
    }
    // bpf_get_current_comm into the context
    {
        let mut a = Asm::new();
        chaos_maps(&mut a);
        a.add64(Reg::R1, 8);
        a.mov64(Reg::R2, 16);
        a.call(h::GET_CURRENT_COMM);
        a.exit();
        cases.push(("comm", a.finish("comm", ProgramType::Tracepoint).unwrap()));
    }
    // a tail call from a subprogram
    let mut tail = Asm::new();
    chaos_maps(&mut tail);
    let sub = tail.label();
    tail.call_local(sub);
    tail.exit();
    tail.bind(sub).unwrap();
    tail.ld_map(Reg::R2, 3);
    tail.mov64(Reg::R3, 1);
    tail.call(h::TAIL_CALL);
    tail.mov64(Reg::R0, 4);
    tail.exit();
    let tail = tail.finish("tail", ProgramType::Xdp).unwrap();
    let mut target = Asm::new();
    chaos_maps(&mut target);
    target.mov64(Reg::R0, 9);
    target.exit();
    let target = target.finish("target", ProgramType::Xdp).unwrap();
    for (name, prog) in &cases {
        for _ in 0..3 {
            session(&mut rng, prog, &[], 8, 10_000, &mut st, name);
        }
    }
    session(
        &mut rng,
        &tail,
        &[(3, 1, target)],
        8,
        10_000,
        &mut st,
        "tail",
    );
    eprintln!("{} runs, {} ok, faults {:?}", st.runs, st.ok, st.faults);
    assert!(st.ok >= 8, "{}", st.ok);
    for d in [
        "use of released or dead memory",
        "socket fields",
        "socket not held",
        "bpf_get_current_comm buffer",
        "tail call from a subprogram or callback",
    ] {
        assert!(st.details.contains(d), "{d}: {:?}", st.details);
    }
}
