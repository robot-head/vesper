//! Hand-written verifier corpus: accepted programs and at least one
//! rejection per verifier rule, each with what the Linux kernel is
//! expected to do with it.
#![allow(dead_code)]

use vesper_ebpf::helpers as h;
use vesper_ebpf::insn::{AluOp, AtomicOp, JmpOp, RawInsn, Reg, Size};
use vesper_ebpf::{Asm, MapDef, MapKind, Program, ProgramType};

/// What the kernel verifier does with a corpus program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kernel {
    Accepts,
    Rejects,
    /// The kernel accepts it; this crate is deliberately stricter.
    AcceptsButStricterHere,
    /// Not loadable as such (map creation fails first, or too large to
    /// load quickly in a test).
    NotLoaded,
}

pub struct Case {
    pub name: &'static str,
    /// `None` = accepted; `Some(rule)` = rejected with that rule.
    pub rule: Option<&'static str>,
    pub kernel: Kernel,
    pub prog: Program,
}

fn fin(a: Asm) -> Program {
    a.finish("corpus", ProgramType::Xdp).expect("assemble")
}

/// `r2 = data`, `r3 = data_end`, and `if r2 + n > r3 goto out`.
fn pkt(a: &mut Asm, n: i32, out: vesper_ebpf::program::Label) {
    a.ldx(Size::W, Reg::R2, Reg::R1, 0);
    a.ldx(Size::W, Reg::R3, Reg::R1, 4);
    a.mov64(Reg::R4, Reg::R2);
    a.add64(Reg::R4, n);
    a.jmp(JmpOp::Gt, Reg::R4, Reg::R3, out);
}

fn ret(a: &mut Asm, v: i32) {
    a.mov64(Reg::R0, v);
    a.exit();
}

fn with_maps() -> (Asm, usize, usize, usize) {
    let mut a = Asm::new();
    let hm = a
        .map(MapDef::new("hm", MapKind::Hash, 4, 8, 16))
        .expect("map");
    let am = a
        .map(MapDef::new("am", MapKind::Array, 4, 16, 4))
        .expect("map");
    let rb = a.map(MapDef::ringbuf("rb", 4096)).expect("map");
    (a, hm, am, rb)
}

/// `r0 = lookup(map, &(u32)0 at fp-4)`.
fn lookup(a: &mut Asm, map: usize) {
    a.st(Size::W, Reg::R10, -4, 0);
    a.ld_map(Reg::R1, map);
    a.mov64(Reg::R2, Reg::R10);
    a.add64(Reg::R2, -4);
    a.call(h::MAP_LOOKUP_ELEM);
}

fn reserve(a: &mut Asm, rb: usize, size: i32) {
    a.ld_map(Reg::R1, rb);
    a.mov64(Reg::R2, size);
    a.mov64(Reg::R3, 0);
    a.call(h::RINGBUF_RESERVE);
}

pub fn corpus() -> Vec<Case> {
    let mut v = Vec::new();
    let mut add = |name, rule, kernel, prog| {
        v.push(Case {
            name,
            rule,
            kernel,
            prog,
        })
    };

    // ------------------------------------------------------ accepted ---
    add(
        "accept/mitigator",
        None,
        Kernel::Accepts,
        vesper_ebpf::mitigator::build(&Default::default()).expect("mitigator"),
    );
    {
        let mut a = Asm::new();
        ret(&mut a, 2);
        add("accept/pass", None, Kernel::Accepts, fin(a));
    }
    {
        // packet read and write inside the checked range
        let mut a = Asm::new();
        let out = a.label();
        pkt(&mut a, 14, out);
        a.ldx(Size::H, Reg::R5, Reg::R2, 12);
        a.stx(Size::H, Reg::R2, 0, Reg::R5);
        a.bind(out).unwrap();
        ret(&mut a, 2);
        add("accept/packet_rw", None, Kernel::Accepts, fin(a));
    }
    {
        // variable packet offset with its own check
        let mut a = Asm::new();
        let out = a.label();
        pkt(&mut a, 1, out);
        a.ldx(Size::B, Reg::R5, Reg::R2, 0);
        a.alu64(AluOp::And, Reg::R5, 0x3f);
        a.add64(Reg::R5, Reg::R2);
        a.mov64(Reg::R4, Reg::R5);
        a.add64(Reg::R4, 4);
        a.jmp(JmpOp::Gt, Reg::R4, Reg::R3, out);
        a.ldx(Size::W, Reg::R0, Reg::R5, 0);
        a.bind(out).unwrap();
        ret(&mut a, 2);
        add("accept/packet_var_offset", None, Kernel::Accepts, fin(a));
    }
    {
        // bounded loop: sum 0..8
        let mut a = Asm::new();
        a.mov64(Reg::R0, 0);
        a.mov64(Reg::R1, 0);
        let head = a.here();
        a.add64(Reg::R0, Reg::R1);
        a.add64(Reg::R1, 1);
        a.jmp(JmpOp::Lt, Reg::R1, 8, head);
        a.alu64(AluOp::And, Reg::R0, 3);
        a.exit();
        add("accept/bounded_loop", None, Kernel::Accepts, fin(a));
    }
    {
        // map lookup, null check, in-bounds update and atomic add
        let (mut a, hm, am, _) = with_maps();
        let skip = a.label();
        lookup(&mut a, am);
        a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
        a.mov64(Reg::R1, 1);
        a.atomic(
            Size::DW,
            AtomicOp::Add { fetch: false },
            Reg::R0,
            8,
            Reg::R1,
        );
        a.stx(Size::W, Reg::R0, 4, Reg::R1);
        a.bind(skip).unwrap();
        let skip2 = a.label();
        lookup(&mut a, hm);
        a.jmp(JmpOp::Ne, Reg::R0, 0, skip2);
        a.st(Size::DW, Reg::R10, -16, 1);
        a.ld_map(Reg::R1, hm);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -4);
        a.mov64(Reg::R3, Reg::R10);
        a.add64(Reg::R3, -16);
        a.mov64(Reg::R4, 0);
        a.call(h::MAP_UPDATE_ELEM);
        a.bind(skip2).unwrap();
        ret(&mut a, 2);
        add("accept/maps", None, Kernel::Accepts, fin(a));
    }
    {
        // ringbuf reserve, write, discard; or submit via a copy
        let (mut a, _, _, rb) = with_maps();
        let none = a.label();
        reserve(&mut a, rb, 16);
        a.mov64(Reg::R6, Reg::R0);
        a.jmp(JmpOp::Eq, Reg::R6, 0, none);
        a.st(Size::DW, Reg::R6, 8, 7);
        a.mov64(Reg::R1, Reg::R6);
        a.mov64(Reg::R2, 0);
        a.call(h::RINGBUF_DISCARD);
        a.bind(none).unwrap();
        ret(&mut a, 2);
        add("accept/ringbuf_discard", None, Kernel::Accepts, fin(a));
    }
    {
        // ringbuf_discard returns void: r0 is unreadable after it, so
        // exiting without setting r0 is rejected on the non-null path
        let (mut a, _, _, rb) = with_maps();
        let none = a.label();
        reserve(&mut a, rb, 16);
        a.jmp(JmpOp::Eq, Reg::R0, 0, none);
        a.mov64(Reg::R1, Reg::R0);
        a.mov64(Reg::R2, 0);
        a.call(h::RINGBUF_DISCARD);
        a.bind(none).unwrap();
        a.exit();
        add(
            "reject/void_helper_r0",
            Some("reg.uninit_read"),
            Kernel::Rejects,
            fin(a),
        );
    }
    {
        // socket lookup on a stack tuple, release on the non-null path
        let mut a = Asm::new();
        for off in [-16, -8] {
            a.st(Size::DW, Reg::R10, off, 0);
        }
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -16);
        a.mov64(Reg::R3, h::SOCK_TUPLE_IPV4 as i32);
        a.mov64(Reg::R4, -1);
        a.mov64(Reg::R5, 0);
        a.call(h::SK_LOOKUP_TCP);
        let none = a.label();
        a.jmp(JmpOp::Eq, Reg::R0, 0, none);
        a.mov64(Reg::R1, Reg::R0);
        a.call(h::SK_RELEASE);
        a.bind(none).unwrap();
        ret(&mut a, 2);
        add("accept/sk_lookup_release", None, Kernel::Accepts, fin(a));
    }
    {
        // BPF-to-BPF call passing a pointer to the caller's stack
        let mut a = Asm::new();
        let f = a.label();
        a.st(Size::DW, Reg::R10, -8, 5);
        a.mov64(Reg::R1, Reg::R10);
        a.add64(Reg::R1, -8);
        a.call_local(f);
        a.alu64(AluOp::And, Reg::R0, 3);
        a.exit();
        a.bind(f).unwrap();
        a.function("callee", f);
        a.ldx(Size::DW, Reg::R0, Reg::R1, 0);
        a.st(Size::DW, Reg::R10, -64, 1);
        a.exit();
        add("accept/subprog", None, Kernel::Accepts, fin(a));
    }
    {
        // adjust_head, then a fresh data pointer and check
        let mut a = Asm::new();
        let out = a.label();
        a.mov64(Reg::R6, Reg::R1);
        a.mov64(Reg::R2, -4);
        a.call(h::XDP_ADJUST_HEAD);
        a.mov64(Reg::R1, Reg::R6);
        pkt(&mut a, 4, out);
        a.st(Size::W, Reg::R2, 0, 0);
        a.bind(out).unwrap();
        ret(&mut a, 2);
        add("accept/adjust_head_recheck", None, Kernel::Accepts, fin(a));
    }

    // ----------------------------------------------------- rejected ---
    macro_rules! reject {
        ($name:expr, $rule:expr, $kernel:expr, |$a:ident| $body:block) => {{
            #[allow(unused_mut)]
            let mut $a = Asm::new();
            $body
            add($name, Some($rule), $kernel, fin($a));
        }};
    }
    macro_rules! reject_maps {
        ($name:expr, $rule:expr, $kernel:expr, |$a:ident, $hm:ident, $am:ident, $rb:ident| $body:block) => {{
            #[allow(unused_mut, unused_variables)]
            let (mut $a, $hm, $am, $rb) = with_maps();
            $body
            add($name, Some($rule), $kernel, fin($a));
        }};
    }

    reject!(
        "reject/div_by_zero",
        "alu.div_by_zero",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R0, 1);
            a.alu64(AluOp::Div, Reg::R0, 0);
            a.exit();
        }
    );
    reject!(
        "reject/invalid_shift",
        "alu.invalid_shift",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R0, 1);
            a.alu64(AluOp::Lsh, Reg::R0, 64);
            a.exit();
        }
    );
    reject!(
        "reject/packet_var_too_large",
        "alu.packet_var_too_large",
        Kernel::AcceptsButStricterHere,
        |a| {
            let out = a.label();
            pkt(&mut a, 4, out);
            a.ldx(Size::W, Reg::R5, Reg::R2, 0);
            a.add64(Reg::R2, Reg::R5);
            a.bind(out).unwrap();
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/pointer_arith_mul",
        "alu.pointer_arith",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R2, Reg::R10);
            a.alu64(AluOp::Mul, Reg::R2, 2);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/pointer_offset",
        "alu.pointer_offset",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R2, Reg::R10);
            a.add64(Reg::R2, 1 << 29);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/stack_var_offset",
        "alu.stack_var_offset",
        Kernel::AcceptsButStricterHere,
        |a| {
            a.call(h::GET_PRANDOM_U32);
            a.alu64(AluOp::And, Reg::R0, 8);
            a.mov64(Reg::R2, Reg::R10);
            a.add64(Reg::R2, -16);
            a.add64(Reg::R2, Reg::R0);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/atomic_on_packet",
        "atomic.invalid_target",
        Kernel::Rejects,
        |a| {
            let out = a.label();
            pkt(&mut a, 8, out);
            a.mov64(Reg::R5, 1);
            a.atomic(
                Size::DW,
                AtomicOp::Add { fetch: false },
                Reg::R2,
                0,
                Reg::R5,
            );
            a.bind(out).unwrap();
            ret(&mut a, 2);
        }
    );
    reject_maps!(
        "reject/atomic_misaligned",
        "atomic.misaligned",
        Kernel::Rejects,
        |a, hm, am, rb| {
            let skip = a.label();
            lookup(&mut a, am);
            a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
            a.mov64(Reg::R1, 1);
            a.atomic(
                Size::DW,
                AtomicOp::Add { fetch: false },
                Reg::R0,
                4,
                Reg::R1,
            );
            a.bind(skip).unwrap();
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/atomic_pointer_operand",
        "atomic.pointer_operand",
        Kernel::AcceptsButStricterHere,
        |a| {
            a.st(Size::DW, Reg::R10, -8, 0);
            a.mov64(Reg::R2, Reg::R10);
            a.atomic(
                Size::DW,
                AtomicOp::Add { fetch: false },
                Reg::R10,
                -8,
                Reg::R2,
            );
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/map_arg_not_map",
        "call.arg_type",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R1, 0);
            a.mov64(Reg::R2, Reg::R10);
            a.add64(Reg::R2, -8);
            a.call(h::MAP_LOOKUP_ELEM);
            ret(&mut a, 2);
        }
    );
    {
        // 8 nested BPF-to-BPF calls: 9 frames
        let mut a = Asm::new();
        let fs: Vec<_> = (0..8).map(|_| a.label()).collect();
        a.call_local(fs[0]);
        ret(&mut a, 2);
        for i in 0..8 {
            a.bind(fs[i]).unwrap();
            if i + 1 < 8 {
                a.call_local(fs[i + 1]);
            }
            ret(&mut a, 0);
        }
        add(
            "reject/call_depth",
            Some("call.depth"),
            Kernel::Rejects,
            fin(a),
        );
    }
    reject!(
        "reject/kfunc",
        "call.kfunc_unsupported",
        Kernel::Rejects,
        |a| {
            a.raw(RawInsn::new(0x85, 0, 2, 0, 1));
            ret(&mut a, 2);
        }
    );
    {
        let mut a = Asm::new();
        let f = a.label();
        a.call_local(f);
        ret(&mut a, 2);
        a.bind(f).unwrap();
        a.call_local(f);
        ret(&mut a, 0);
        add(
            "reject/recursion",
            Some("call.recursion"),
            Kernel::Rejects,
            fin(a),
        );
    }
    reject!(
        "reject/unknown_helper",
        "call.unknown_helper",
        Kernel::AcceptsButStricterHere,
        |a| {
            a.call(42); // bpf_get_numa_node_id: valid in XDP, not supported here
            ret(&mut a, 2);
        }
    );
    {
        let mut a = Asm::new();
        let f = a.label();
        a.call_local(f);
        ret(&mut a, 2);
        a.bind(f).unwrap();
        a.ja_raw(-4);
        add(
            "reject/cross_subprog_jump",
            Some("cfg.cross_subprog_jump"),
            Kernel::Rejects,
            fin(a),
        );
    }
    add(
        "reject/empty",
        Some("cfg.empty"),
        Kernel::Rejects,
        Program {
            insns: vec![],
            ..fin(Asm::new())
        },
    );
    reject!(
        "reject/fallthrough",
        "cfg.fallthrough",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R0, 2);
        }
    );
    reject!(
        "reject/jump_out_of_range",
        "cfg.jump_out_of_range",
        Kernel::Rejects,
        |a| {
            a.ja_raw(5);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/jump_into_ld_imm64",
        "cfg.jump_into_ld_imm64",
        Kernel::Rejects,
        |a| {
            a.ja_raw(1);
            a.ld_imm64(Reg::R0, 1);
            ret(&mut a, 2);
        }
    );
    {
        let mut slots = Vec::with_capacity(1_000_002);
        let mov = vesper_ebpf::insn::encode(&vesper_ebpf::Insn::Alu {
            wide: true,
            op: AluOp::Mov,
            dst: Reg::R0,
            src: 2.into(),
        })[0];
        slots.resize(1_000_001, mov);
        slots.push(vesper_ebpf::insn::encode(&vesper_ebpf::Insn::Exit)[0]);
        add(
            "reject/too_large",
            Some("cfg.too_large"),
            Kernel::NotLoaded,
            Program {
                insns: slots,
                ..fin(Asm::new())
            },
        );
    }
    reject!(
        "reject/unreachable",
        "cfg.unreachable",
        Kernel::Rejects,
        |a| {
            ret(&mut a, 2);
            ret(&mut a, 1);
        }
    );
    reject!(
        "reject/complexity",
        "complexity.limit",
        Kernel::Rejects,
        |a| {
            // every iteration differs, so nothing prunes: 2^31 iterations
            a.mov64(Reg::R0, 0);
            let head = a.here();
            a.add64(Reg::R0, 1);
            a.jmp(JmpOp::Lt, Reg::R0, 0x7fff_ffff, head);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/bad_opcode",
        "decode.invalid",
        Kernel::Rejects,
        |a| {
            a.raw(RawInsn::new(0xff, 0, 0, 0, 0));
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/r0_pointer_at_exit",
        "exit.r0_not_scalar",
        Kernel::AcceptsButStricterHere,
        |a| {
            a.mov64(Reg::R0, Reg::R10);
            a.exit();
        }
    );
    {
        let mut a = Asm::new();
        let f = a.label();
        a.call_local(f);
        ret(&mut a, 2);
        a.bind(f).unwrap();
        a.st(Size::DW, Reg::R10, -8, 0);
        a.mov64(Reg::R0, Reg::R10);
        a.add64(Reg::R0, -8);
        a.exit();
        add(
            "reject/stack_ptr_escape",
            Some("exit.stack_ptr_escape"),
            Kernel::Rejects,
            fin(a),
        );
    }
    reject!("reject/ld_abs", "insn.unsupported", Kernel::Rejects, |a| {
        a.mov64(Reg::R6, Reg::R1);
        a.raw(RawInsn::new(0x20, 0, 0, 0, 0));
        ret(&mut a, 2);
    });
    reject!(
        "reject/pkt_vs_pkt_compare",
        "jmp.pointer_compare",
        Kernel::AcceptsButStricterHere,
        |a| {
            let out = a.label();
            a.ldx(Size::W, Reg::R2, Reg::R1, 0);
            a.mov64(Reg::R4, Reg::R2);
            a.jmp(JmpOp::Gt, Reg::R4, Reg::R2, out);
            a.bind(out).unwrap();
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/bad_map_index",
        "ld_imm64.bad_map",
        Kernel::NotLoaded,
        |a| {
            a.ld_map(Reg::R1, 3);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/infinite_loop",
        "loop.unbounded",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R0, 2);
            let head = a.here();
            a.ja(head);
        }
    );
    {
        let mut p = fin({
            let mut a = Asm::new();
            ret(&mut a, 2);
            a
        });
        p.maps
            .push(MapDef::new("this_name_is_too_long", MapKind::Hash, 4, 4, 1));
        add(
            "reject/map_invalid",
            Some("map.invalid"),
            Kernel::NotLoaded,
            p,
        );
    }
    reject!(
        "reject/ctx_bad_field",
        "mem.ctx_invalid_access",
        Kernel::Rejects,
        |a| {
            a.ldx(Size::W, Reg::R0, Reg::R1, 24);
            ret(&mut a, 2);
        }
    );
    reject!("reject/ctx_write", "mem.ctx_write", Kernel::Rejects, |a| {
        a.st(Size::W, Reg::R1, 0, 0);
        ret(&mut a, 2);
    });
    reject!(
        "reject/deref_pkt_end",
        "mem.invalid_pointer",
        Kernel::Rejects,
        |a| {
            a.ldx(Size::W, Reg::R3, Reg::R1, 4);
            a.ldx(Size::B, Reg::R0, Reg::R3, 0);
            ret(&mut a, 2);
        }
    );
    reject_maps!(
        "reject/map_value_oob",
        "mem.map_value_out_of_bounds",
        Kernel::Rejects,
        |a, hm, am, rb| {
            let skip = a.label();
            lookup(&mut a, hm);
            a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
            a.ldx(Size::DW, Reg::R1, Reg::R0, 4);
            a.bind(skip).unwrap();
            ret(&mut a, 2);
        }
    );
    reject_maps!(
        "reject/null_arith",
        "mem.null_arith",
        Kernel::Rejects,
        |a, hm, am, rb| {
            lookup(&mut a, hm);
            a.add64(Reg::R0, 1);
            ret(&mut a, 2);
        }
    );
    reject_maps!(
        "reject/null_deref",
        "mem.null_deref",
        Kernel::Rejects,
        |a, hm, am, rb| {
            lookup(&mut a, hm);
            a.ldx(Size::DW, Reg::R1, Reg::R0, 0);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/packet_unchecked",
        "mem.packet_out_of_range",
        Kernel::Rejects,
        |a| {
            let out = a.label();
            pkt(&mut a, 4, out);
            a.ldx(Size::W, Reg::R0, Reg::R2, 1);
            a.bind(out).unwrap();
            ret(&mut a, 2);
        }
    );
    reject_maps!(
        "reject/pointer_into_map",
        "mem.pointer_leak",
        Kernel::AcceptsButStricterHere,
        |a, hm, am, rb| {
            let skip = a.label();
            lookup(&mut a, hm);
            a.jmp(JmpOp::Eq, Reg::R0, 0, skip);
            a.stx(Size::DW, Reg::R0, 0, Reg::R10);
            a.bind(skip).unwrap();
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/partial_fill",
        "mem.pointer_partial_fill",
        Kernel::Rejects,
        |a| {
            a.stx(Size::DW, Reg::R10, -8, Reg::R1);
            a.ldx(Size::W, Reg::R0, Reg::R10, -8);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/partial_spill",
        "mem.pointer_partial_spill",
        Kernel::Rejects,
        |a| {
            a.stx(Size::W, Reg::R10, -8, Reg::R1);
            ret(&mut a, 2);
        }
    );
    reject_maps!(
        "reject/ringbuf_oob",
        "mem.ringbuf_out_of_bounds",
        Kernel::Rejects,
        |a, hm, am, rb| {
            let none = a.label();
            reserve(&mut a, rb, 8);
            a.jmp(JmpOp::Eq, Reg::R0, 0, none);
            a.st(Size::DW, Reg::R0, 8, 0);
            a.mov64(Reg::R1, Reg::R0);
            a.mov64(Reg::R2, 0);
            a.call(h::RINGBUF_SUBMIT);
            a.bind(none).unwrap();
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/stack_misaligned",
        "mem.stack_misaligned",
        Kernel::Rejects,
        |a| {
            a.st(Size::W, Reg::R10, -6, 0);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/stack_oob",
        "mem.stack_out_of_bounds",
        Kernel::Rejects,
        |a| {
            a.st(Size::DW, Reg::R10, -520, 0);
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/uninit_stack",
        "mem.uninit_stack_read",
        Kernel::AcceptsButStricterHere,
        |a| {
            a.ldx(Size::DW, Reg::R0, Reg::R10, -8);
            ret(&mut a, 2);
        }
    );
    {
        let mut a = Asm::new();
        for off in [-16, -8] {
            a.st(Size::DW, Reg::R10, off, 0);
        }
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -16);
        a.mov64(Reg::R3, h::SOCK_TUPLE_IPV4 as i32);
        a.mov64(Reg::R4, -1);
        a.mov64(Reg::R5, 0);
        a.call(h::SK_LOOKUP_TCP);
        let none = a.label();
        a.jmp(JmpOp::Eq, Reg::R0, 0, none);
        a.ldx(Size::W, Reg::R1, Reg::R0, 0);
        a.mov64(Reg::R1, Reg::R0);
        a.call(h::SK_RELEASE);
        a.bind(none).unwrap();
        ret(&mut a, 2);
        add(
            "reject/sock_field",
            Some("mem.unsupported_target"),
            Kernel::AcceptsButStricterHere,
            fin(a),
        );
    }
    reject_maps!(
        "reject/ringbuf_leak",
        "ref.leak",
        Kernel::Rejects,
        |a, hm, am, rb| {
            // the drop path forgets the reservation
            let none = a.label();
            reserve(&mut a, rb, 16);
            a.jmp(JmpOp::Eq, Reg::R0, 0, none);
            a.st(Size::DW, Reg::R0, 0, 1);
            ret(&mut a, 1);
            a.bind(none).unwrap();
            ret(&mut a, 2);
        }
    );
    {
        let mut a = Asm::new();
        for off in [-16, -8] {
            a.st(Size::DW, Reg::R10, off, 0);
        }
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -16);
        a.mov64(Reg::R3, h::SOCK_TUPLE_IPV4 as i32);
        a.mov64(Reg::R4, -1);
        a.mov64(Reg::R5, 0);
        a.call(h::SK_LOOKUP_TCP);
        let none = a.label();
        a.jmp(JmpOp::Eq, Reg::R0, 0, none);
        ret(&mut a, 2);
        a.bind(none).unwrap();
        ret(&mut a, 2);
        add(
            "reject/socket_leak",
            Some("ref.leak"),
            Kernel::Rejects,
            fin(a),
        );
    }
    reject_maps!(
        "reject/double_submit",
        "reg.uninit_read",
        Kernel::Rejects,
        |a, hm, am, rb| {
            let none = a.label();
            reserve(&mut a, rb, 8);
            a.mov64(Reg::R6, Reg::R0);
            a.jmp(JmpOp::Eq, Reg::R6, 0, none);
            a.mov64(Reg::R1, Reg::R6);
            a.mov64(Reg::R2, 0);
            a.call(h::RINGBUF_SUBMIT);
            a.mov64(Reg::R1, Reg::R6);
            a.mov64(Reg::R2, 0);
            a.call(h::RINGBUF_SUBMIT);
            a.bind(none).unwrap();
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/submit_stack_ptr",
        "ref.release_invalid",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R1, Reg::R10);
            a.add64(Reg::R1, -8);
            a.mov64(Reg::R2, 0);
            a.call(h::RINGBUF_SUBMIT);
            ret(&mut a, 2);
        }
    );
    reject_maps!(
        "reject/release_offset",
        "ref.release_offset",
        Kernel::Rejects,
        |a, hm, am, rb| {
            let none = a.label();
            reserve(&mut a, rb, 16);
            a.jmp(JmpOp::Eq, Reg::R0, 0, none);
            a.mov64(Reg::R1, Reg::R0);
            a.add64(Reg::R1, 8);
            a.mov64(Reg::R2, 0);
            a.call(h::RINGBUF_SUBMIT);
            a.bind(none).unwrap();
            ret(&mut a, 2);
        }
    );
    reject!(
        "reject/uninit_reg",
        "reg.uninit_read",
        Kernel::Rejects,
        |a| {
            a.mov64(Reg::R0, Reg::R7);
            a.exit();
        }
    );
    reject!("reject/write_fp", "reg.write_fp", Kernel::Rejects, |a| {
        a.mov64(Reg::R10, 0);
        ret(&mut a, 2);
    });
    {
        // 400 bytes in main + 200 in the callee
        let mut a = Asm::new();
        let f = a.label();
        a.st(Size::DW, Reg::R10, -400, 0);
        a.call_local(f);
        ret(&mut a, 2);
        a.bind(f).unwrap();
        a.st(Size::DW, Reg::R10, -200, 0);
        ret(&mut a, 0);
        add(
            "reject/combined_stack",
            Some("stack.limit"),
            Kernel::Rejects,
            fin(a),
        );
    }
    v
}
