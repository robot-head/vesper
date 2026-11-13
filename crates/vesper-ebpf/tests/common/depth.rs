//! Corpus cases for the wave-six additions: tc, tracepoint and kprobe
//! contexts and helper sets, tail calls, `bpf_loop`, kfuncs and scalar id
//! linking. Appended to [`super::corpus::corpus`].

use super::corpus::{Case, Kernel};
use vesper_ebpf::helpers as h;
use vesper_ebpf::insn::{AluOp, JmpOp, RawInsn, Reg, Size};
use vesper_ebpf::{Asm, MapDef, MapKind, Program, ProgramType};

fn fin(a: Asm, t: ProgramType) -> Program {
    a.finish("corpus", t).expect("assemble")
}

fn ret(a: &mut Asm, v: i32) {
    a.mov64(Reg::R0, v);
    a.exit();
}

/// `r2 = skb->data`, `r3 = skb->data_end`, `if r2 + n > r3 goto out`.
fn skb_pkt(a: &mut Asm, n: i32, out: vesper_ebpf::program::Label) {
    a.ldx(Size::W, Reg::R2, Reg::R1, 76);
    a.ldx(Size::W, Reg::R3, Reg::R1, 80);
    a.mov64(Reg::R4, Reg::R2);
    a.add64(Reg::R4, n);
    a.jmp(JmpOp::Gt, Reg::R4, Reg::R3, out);
}

/// `bpf_tail_call(ctx, prog_array, 0)` with the context in r6.
fn tail_call(a: &mut Asm, pa: usize) {
    a.mov64(Reg::R1, Reg::R6);
    a.ld_map(Reg::R2, pa);
    a.mov64(Reg::R3, 0);
    a.call(h::TAIL_CALL);
}

pub fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    let mut add = |name, rule, kernel, prog| {
        v.push(Case {
            name,
            rule,
            kernel,
            prog,
        })
    };
    use ProgramType::{Kprobe, SchedCls, Tracepoint, Xdp};

    // ------------------------------------------------------------ tc ---
    {
        // read every supported __sk_buff field, write the writable ones
        let mut a = Asm::new();
        a.mov64(Reg::R6, Reg::R1);
        for off in [0, 4, 8, 12, 16, 24, 28, 32, 36, 40, 44, 48, 64, 68, 72] {
            a.ldx(Size::W, Reg::R0, Reg::R6, off);
        }
        a.mov64(Reg::R0, 7);
        for off in [8, 32, 44, 48, 64, 72] {
            a.stx(Size::W, Reg::R6, off, Reg::R0);
        }
        ret(&mut a, 0);
        add(
            "accept/tc_skb_fields",
            None,
            Kernel::Accepts,
            fin(a, SchedCls),
        );
    }
    {
        // direct packet read and write in a tc classifier
        let mut a = Asm::new();
        let out = a.label();
        skb_pkt(&mut a, 14, out);
        a.ldx(Size::H, Reg::R5, Reg::R2, 12);
        a.stx(Size::H, Reg::R2, 0, Reg::R5);
        a.bind(out).unwrap();
        ret(&mut a, 0);
        add(
            "accept/tc_packet_rw",
            None,
            Kernel::Accepts,
            fin(a, SchedCls),
        );
    }
    {
        let mut a = Asm::new();
        a.st(Size::W, Reg::R1, 8, 1);
        ret(&mut a, 0);
        add(
            "reject/tc_ctx_st_imm",
            Some("mem.ctx_st_imm"),
            Kernel::AcceptsButStricterHere,
            fin(a, SchedCls),
        );
    }
    {
        let mut a = Asm::new();
        a.mov64(Reg::R2, 1);
        a.stx(Size::W, Reg::R1, 0, Reg::R2);
        ret(&mut a, 0);
        add(
            "reject/tc_write_len",
            Some("mem.ctx_write"),
            Kernel::Rejects,
            fin(a, SchedCls),
        );
    }
    {
        let mut a = Asm::new();
        a.ldx(Size::W, Reg::R2, Reg::R1, 192);
        ret(&mut a, 0);
        add(
            "reject/tc_past_skb",
            Some("mem.ctx_invalid_access"),
            Kernel::Rejects,
            fin(a, SchedCls),
        );
    }
    {
        let mut a = Asm::new();
        a.call(h::XDP_ADJUST_HEAD);
        ret(&mut a, 0);
        add(
            "reject/tc_xdp_helper",
            Some("call.helper_not_allowed"),
            Kernel::Rejects,
            fin(a, SchedCls),
        );
    }

    // ------------------------------------------------------- tracing ---
    {
        let mut a = Asm::new();
        let rb = a.map(MapDef::ringbuf("rb", 4096)).expect("map");
        a.mov64(Reg::R6, Reg::R1);
        a.ldx(Size::DW, Reg::R7, Reg::R6, 8);
        a.ldx(Size::W, Reg::R8, Reg::R6, 16);
        a.ldx(Size::B, Reg::R8, Reg::R6, 2047);
        a.call(h::GET_CURRENT_PID_TGID);
        a.stx(Size::DW, Reg::R10, -24, Reg::R0);
        a.mov64(Reg::R1, Reg::R10);
        a.add64(Reg::R1, -16);
        a.mov64(Reg::R2, 16);
        a.call(h::GET_CURRENT_COMM);
        a.ld_map(Reg::R1, rb);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -24);
        a.mov64(Reg::R3, 24);
        a.mov64(Reg::R4, 0);
        a.call(h::RINGBUF_OUTPUT);
        ret(&mut a, 0);
        add(
            "accept/tracepoint_record",
            None,
            Kernel::Accepts,
            fin(a, Tracepoint),
        );
    }
    {
        let mut a = Asm::new();
        a.ldx(Size::DW, Reg::R2, Reg::R1, 0);
        ret(&mut a, 0);
        add(
            "reject/tracepoint_common_fields",
            Some("mem.ctx_invalid_access"),
            Kernel::Rejects,
            fin(a, Tracepoint),
        );
    }
    {
        let mut a = Asm::new();
        a.mov64(Reg::R2, 0);
        a.stx(Size::DW, Reg::R1, 8, Reg::R2);
        ret(&mut a, 0);
        add(
            "reject/tracepoint_write",
            Some("mem.ctx_write"),
            Kernel::Rejects,
            fin(a, Tracepoint),
        );
    }
    if cfg!(target_arch = "x86_64") {
        // every register of struct pt_regs
        let mut a = Asm::new();
        a.mov64(Reg::R6, Reg::R1);
        for i in 0..21 {
            a.ldx(Size::DW, Reg::R0, Reg::R6, i * 8);
        }
        a.call(h::GET_CURRENT_UID_GID);
        ret(&mut a, 0);
        add(
            "accept/kprobe_pt_regs",
            None,
            Kernel::Accepts,
            fin(a, Kprobe),
        );

        let mut a = Asm::new();
        a.ldx(Size::DW, Reg::R2, Reg::R1, 168);
        ret(&mut a, 0);
        add(
            "reject/kprobe_past_pt_regs",
            Some("mem.ctx_invalid_access"),
            Kernel::Rejects,
            fin(a, Kprobe),
        );
    }
    {
        let mut a = Asm::new();
        a.call(h::GET_CURRENT_PID_TGID);
        ret(&mut a, 2);
        add(
            "reject/xdp_tracing_helper",
            Some("call.helper_not_allowed"),
            Kernel::AcceptsButStricterHere,
            fin(a, Xdp),
        );
    }

    // ---------------------------------------------------- tail calls ---
    for t in [Xdp, SchedCls, Kprobe] {
        let mut a = Asm::new();
        let pa = a.map(MapDef::prog_array("jmp", 4)).expect("map");
        a.mov64(Reg::R6, Reg::R1);
        tail_call(&mut a, pa);
        ret(&mut a, if t == Xdp { 2 } else { 0 });
        let name = match t {
            Xdp => "accept/tail_call_xdp",
            SchedCls => "accept/tail_call_tc",
            _ => "accept/tail_call_kprobe",
        };
        add(name, None, Kernel::Accepts, fin(a, t));
    }
    {
        let mut a = Asm::new();
        let pa = a.map(MapDef::prog_array("jmp", 4)).expect("map");
        let rb = a.map(MapDef::ringbuf("rb", 4096)).expect("map");
        let out = a.label();
        a.mov64(Reg::R6, Reg::R1);
        a.ld_map(Reg::R1, rb);
        a.mov64(Reg::R2, 8);
        a.mov64(Reg::R3, 0);
        a.call(h::RINGBUF_RESERVE);
        a.jmp(JmpOp::Eq, Reg::R0, 0, out);
        a.mov64(Reg::R7, Reg::R0);
        tail_call(&mut a, pa);
        a.mov64(Reg::R1, Reg::R7);
        a.mov64(Reg::R2, 0);
        a.call(h::RINGBUF_DISCARD);
        a.bind(out).unwrap();
        ret(&mut a, 2);
        add(
            "reject/tail_call_holding_ref",
            Some("call.tail_call_ref"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        let pa = a.map(MapDef::prog_array("jmp", 4)).expect("map");
        let f = a.label();
        a.mov64(Reg::R6, Reg::R1);
        a.call_local(f);
        tail_call(&mut a, pa);
        ret(&mut a, 2);
        a.bind(f).unwrap();
        ret(&mut a, 0);
        add(
            "reject/tail_call_with_subprog",
            Some("call.tail_call_subprogs"),
            Kernel::AcceptsButStricterHere,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        let pa = a.map(MapDef::prog_array("jmp", 4)).expect("map");
        a.st(Size::W, Reg::R10, -4, 0);
        a.ld_map(Reg::R1, pa);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -4);
        a.call(h::MAP_LOOKUP_ELEM);
        ret(&mut a, 2);
        add(
            "reject/prog_array_lookup",
            Some("call.arg_type"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        let hm = a
            .map(MapDef::new("hm", MapKind::Hash, 4, 4, 4))
            .expect("map");
        a.mov64(Reg::R6, Reg::R1);
        a.ld_map(Reg::R2, hm);
        a.mov64(Reg::R3, 0);
        a.mov64(Reg::R1, Reg::R6);
        a.call(h::TAIL_CALL);
        ret(&mut a, 2);
        add(
            "reject/tail_call_hash_map",
            Some("call.arg_type"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }

    // ------------------------------------------------------ bpf_loop ---
    {
        // counter on the caller's stack; the callback stores a constant
        // through its context pointer and stops on odd indices
        let mut a = Asm::new();
        let cb = a.label();
        a.st(Size::DW, Reg::R10, -8, 0);
        a.mov64(Reg::R1, 16);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, Reg::R10);
        a.add64(Reg::R3, -8);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        a.ldx(Size::DW, Reg::R1, Reg::R10, -8);
        ret(&mut a, 2);
        a.bind(cb).unwrap();
        a.st(Size::DW, Reg::R2, 0, 1);
        a.mov64(Reg::R0, Reg::R1);
        a.alu64(AluOp::And, Reg::R0, 1);
        a.exit();
        add("accept/bpf_loop", None, Kernel::Accepts, fin(a, Xdp));
    }
    {
        // bpf_loop's result: the callback stops at index 2, so 3 runs
        let mut a = Asm::new();
        let cb = a.label();
        let end = a.label();
        a.mov64(Reg::R1, 5);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, 0);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        a.alu64(AluOp::And, Reg::R0, 3);
        a.exit();
        a.bind(cb).unwrap();
        a.mov64(Reg::R0, 0);
        a.jmp(JmpOp::Ne, Reg::R1, 2, end);
        a.mov64(Reg::R0, 1);
        a.bind(end).unwrap();
        a.exit();
        add("accept/bpf_loop_count", None, Kernel::Accepts, fin(a, Xdp));
    }
    {
        let mut a = Asm::new();
        let cb = a.label();
        a.mov64(Reg::R1, 4);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, 0);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        ret(&mut a, 0);
        a.bind(cb).unwrap();
        ret(&mut a, 0);
        add(
            "accept/bpf_loop_null_ctx",
            None,
            Kernel::Accepts,
            fin(a, Tracepoint),
        );
    }
    {
        let mut a = Asm::new();
        let cb = a.label();
        a.mov64(Reg::R1, 4);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, 0);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        ret(&mut a, 2);
        a.bind(cb).unwrap();
        ret(&mut a, 2);
        add(
            "reject/bpf_loop_callback_ret",
            Some("exit.callback_ret"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        // the callback context is read before the first iteration wrote it
        let mut a = Asm::new();
        let cb = a.label();
        a.mov64(Reg::R1, 4);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, Reg::R10);
        a.add64(Reg::R3, -8);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        ret(&mut a, 2);
        a.bind(cb).unwrap();
        a.ldx(Size::DW, Reg::R0, Reg::R2, 0);
        ret(&mut a, 0);
        add(
            "reject/bpf_loop_uninit_ctx",
            Some("mem.uninit_stack_read"),
            Kernel::AcceptsButStricterHere,
            fin(a, Xdp),
        );
    }
    {
        // the second iteration sees what the first wrote: a pointer
        // spilled by iteration one is not a scalar in iteration two
        let mut a = Asm::new();
        let cb = a.label();
        a.st(Size::DW, Reg::R10, -8, 0);
        a.mov64(Reg::R1, 4);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, Reg::R10);
        a.add64(Reg::R3, -8);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        ret(&mut a, 2);
        a.bind(cb).unwrap();
        a.ldx(Size::DW, Reg::R3, Reg::R2, 0);
        a.alu64(AluOp::Mul, Reg::R3, 3);
        a.stx(Size::DW, Reg::R2, 0, Reg::R2);
        ret(&mut a, 0);
        add(
            "reject/bpf_loop_second_iteration",
            Some("alu.pointer_arith"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        // nr_loops = 1 << 32: the kernel's inlined bpf_loop compares all
        // 64 bits and returns -E2BIG (the helper's prototype says u32)
        let mut a = Asm::new();
        let cb = a.label();
        a.ld_imm64(Reg::R1, 1 << 32);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, 0);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        a.alu64(AluOp::And, Reg::R0, 3);
        a.exit();
        a.bind(cb).unwrap();
        ret(&mut a, 1);
        add(
            "accept/bpf_loop_upper_bits",
            None,
            Kernel::Accepts,
            fin(a, Xdp),
        );
    }
    {
        // bpf_loop bounds the number of callback calls, not the work in
        // one call: a callback spinning on its own entry is unbounded
        let mut a = Asm::new();
        let cb = a.label();
        a.mov64(Reg::R1, 1);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, 0);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        ret(&mut a, 2);
        a.bind(cb).unwrap();
        a.ja_raw(-1);
        add(
            "reject/bpf_loop_callback_spins",
            Some("loop.unbounded"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        a.mov64(Reg::R1, 4);
        a.mov64(Reg::R2, 0);
        a.mov64(Reg::R3, 0);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        ret(&mut a, 2);
        add(
            "reject/bpf_loop_not_a_func",
            Some("call.arg_type"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        // BPF_PSEUDO_FUNC pointing at the main program
        let mut a = Asm::new();
        a.raw(RawInsn::new(0x18, 2, 4, 0, -1));
        a.raw(RawInsn::new(0, 0, 0, 0, 0));
        ret(&mut a, 2);
        add(
            "reject/func_to_main",
            Some("ld_imm64.bad_func"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }

    // -------------------------------------------------------- kfuncs ---
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_preempt_disable");
        a.call(h::GET_SMP_PROCESSOR_ID);
        a.mov64(Reg::R6, Reg::R0);
        a.call_kfunc("bpf_preempt_enable");
        a.mov64(Reg::R0, Reg::R6);
        a.alu64(AluOp::And, Reg::R0, 3);
        a.exit();
        add("accept/kfunc_preempt", None, Kernel::Accepts, fin(a, Xdp));
    }
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_preempt_disable");
        ret(&mut a, 2);
        add(
            "reject/kfunc_preempt_held",
            Some("kfunc.preempt_region"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_preempt_enable");
        ret(&mut a, 2);
        add(
            "reject/kfunc_preempt_unbalanced",
            Some("kfunc.unbalanced"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_preempt_disable");
        a.call_kfunc("bpf_preempt_enable");
        ret(&mut a, 2);
        let mut p = fin(a, Xdp);
        p.license = "Proprietary".into();
        add(
            "reject/kfunc_license",
            Some("kfunc.license"),
            Kernel::Rejects,
            p,
        );
    }
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_rcu_read_lock");
        a.call_kfunc("bpf_rcu_read_unlock");
        ret(&mut a, 2);
        add(
            "reject/kfunc_unmodelled",
            Some("call.kfunc_unsupported"),
            Kernel::AcceptsButStricterHere,
            fin(a, Xdp),
        );
    }

    // --------------------------------------------- scalar id linking ---
    {
        // r7 = r6 (linked); the bound learnt on r6 makes r7 a safe offset
        let mut a = Asm::new();
        let am = a
            .map(MapDef::new("am", MapKind::Array, 4, 16, 4))
            .expect("map");
        let out = a.label();
        a.call(h::GET_PRANDOM_U32);
        a.mov64(Reg::R6, Reg::R0);
        a.mov64(Reg::R7, Reg::R6);
        a.st(Size::W, Reg::R10, -4, 0);
        a.ld_map(Reg::R1, am);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -4);
        a.call(h::MAP_LOOKUP_ELEM);
        a.jmp(JmpOp::Eq, Reg::R0, 0, out);
        a.jmp(JmpOp::Gt, Reg::R6, 12, out);
        a.add64(Reg::R0, Reg::R7);
        a.ldx(Size::W, Reg::R1, Reg::R0, 0);
        a.bind(out).unwrap();
        ret(&mut a, 2);
        add("accept/linked_scalars", None, Kernel::Accepts, fin(a, Xdp));
    }
    {
        // the link is broken by a write to r7
        let mut a = Asm::new();
        let am = a
            .map(MapDef::new("am", MapKind::Array, 4, 16, 4))
            .expect("map");
        let out = a.label();
        a.call(h::GET_PRANDOM_U32);
        a.mov64(Reg::R6, Reg::R0);
        a.mov64(Reg::R7, Reg::R6);
        a.add64(Reg::R7, 1);
        a.st(Size::W, Reg::R10, -4, 0);
        a.ld_map(Reg::R1, am);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -4);
        a.call(h::MAP_LOOKUP_ELEM);
        a.jmp(JmpOp::Eq, Reg::R0, 0, out);
        a.jmp(JmpOp::Gt, Reg::R6, 12, out);
        a.add64(Reg::R0, Reg::R7);
        a.ldx(Size::W, Reg::R1, Reg::R0, 0);
        a.bind(out).unwrap();
        ret(&mut a, 2);
        add(
            "reject/link_broken_by_write",
            Some("alu.pointer_arith"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    v
}
