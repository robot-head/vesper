//! Corpus cases for the wave-seven additions: the RCU, irq-flag and
//! open-coded `bpf_iter_num` kfuncs, and widening of loop states
//! (`bpf_loop` callbacks and iterators). Appended to
//! [`super::corpus::corpus`]; every case's kernel verdict is checked by
//! `tests/kernel_diff.rs` where the host loads programs.

use super::corpus::{Case, Kernel};
use vesper_ebpf::helpers as h;
use vesper_ebpf::insn::{AluOp, AtomicOp, JmpOp, Reg, Size};
use vesper_ebpf::{Asm, MapDef, MapKind, Program, ProgramType};

fn fin(a: Asm, t: ProgramType) -> Program {
    a.finish("corpus", t).expect("assemble")
}

fn ret(a: &mut Asm, v: i32) {
    a.mov64(Reg::R0, v);
    a.exit();
}

/// `r1 = fp + off`.
fn fp(a: &mut Asm, off: i32) {
    a.mov64(Reg::R1, Reg::R10);
    a.add64(Reg::R1, off);
}

/// `bpf_iter_num_new(fp-8, start, end)`.
fn iter_new(a: &mut Asm, start: i32, end: i32) {
    fp(a, -8);
    a.mov64(Reg::R2, start);
    a.mov64(Reg::R3, end);
    a.call_kfunc("bpf_iter_num_new");
}

fn iter_next(a: &mut Asm) {
    fp(a, -8);
    a.call_kfunc("bpf_iter_num_next");
}

fn iter_destroy(a: &mut Asm) {
    fp(a, -8);
    a.call_kfunc("bpf_iter_num_destroy");
}

/// `sum = 0; bpf_for(i, 0, end) sum += i; return sum` (the shape libbpf's
/// `bpf_for` macro expands to).
pub fn iter_sum(end: i32) -> Program {
    let mut a = Asm::new();
    let head = a.label();
    let done = a.label();
    a.mov64(Reg::R6, 0);
    iter_new(&mut a, 0, end);
    a.bind(head).unwrap();
    iter_next(&mut a);
    a.jmp(JmpOp::Eq, Reg::R0, 0, done);
    a.ldx(Size::W, Reg::R1, Reg::R0, 0);
    a.add64(Reg::R6, Reg::R1);
    a.ja(head);
    a.bind(done).unwrap();
    iter_destroy(&mut a);
    a.mov64(Reg::R0, Reg::R6);
    a.exit();
    fin(a, ProgramType::Xdp)
}

/// An iterator loop that counts its iterations in a register and on the
/// stack: neither converges without widening (0, 1, 2, ...). The count is
/// stored in an array map; returns `XDP_PASS`.
pub fn iter_counter(end: i32) -> Program {
    let mut a = Asm::new();
    let am = a
        .map(MapDef::new("count", MapKind::Array, 4, 8, 1))
        .expect("map");
    let head = a.label();
    let done = a.label();
    let out = a.label();
    a.mov64(Reg::R6, 0);
    a.st(Size::DW, Reg::R10, -16, 0);
    iter_new(&mut a, 0, end);
    a.bind(head).unwrap();
    iter_next(&mut a);
    a.jmp(JmpOp::Eq, Reg::R0, 0, done);
    a.add64(Reg::R6, 1);
    a.ldx(Size::DW, Reg::R1, Reg::R10, -16);
    a.add64(Reg::R1, 2);
    a.stx(Size::DW, Reg::R10, -16, Reg::R1);
    a.ja(head);
    a.bind(done).unwrap();
    iter_destroy(&mut a);
    a.ldx(Size::DW, Reg::R7, Reg::R10, -16);
    a.add64(Reg::R7, Reg::R6);
    a.st(Size::W, Reg::R10, -4, 0);
    a.ld_map(Reg::R1, am);
    a.mov64(Reg::R2, Reg::R10);
    a.add64(Reg::R2, -4);
    a.call(h::MAP_LOOKUP_ELEM);
    a.jmp(JmpOp::Eq, Reg::R0, 0, out);
    a.stx(Size::DW, Reg::R0, 0, Reg::R7);
    a.bind(out).unwrap();
    ret(&mut a, 2);
    fin(a, ProgramType::Xdp)
}

/// `bpf_loop(n, cb, &ctx, 0)` where the callback adds 3 to a counter in
/// the caller's stack (the callback context): the caller's state changes
/// on every iteration, so only widening makes it converge. Returns the
/// counter masked to its low byte plus the iteration count.
pub fn loop_counter(n: i32) -> Program {
    let mut a = Asm::new();
    let cb = a.label();
    a.st(Size::DW, Reg::R10, -8, 0);
    a.mov64(Reg::R1, n);
    a.ld_func(Reg::R2, cb);
    a.mov64(Reg::R3, Reg::R10);
    a.add64(Reg::R3, -8);
    a.mov64(Reg::R4, 0);
    a.call(h::LOOP);
    a.mov64(Reg::R6, Reg::R0);
    a.ldx(Size::DW, Reg::R0, Reg::R10, -8);
    a.alu64(AluOp::And, Reg::R0, 0xff);
    a.add64(Reg::R0, Reg::R6);
    a.exit();
    a.bind(cb).unwrap();
    a.ldx(Size::DW, Reg::R3, Reg::R2, 0);
    a.add64(Reg::R3, 3);
    a.stx(Size::DW, Reg::R2, 0, Reg::R3);
    ret(&mut a, 0);
    fin(a, ProgramType::Xdp)
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

    // ----------------------------------------------------------- RCU ---
    for (name, t) in [
        ("accept/kfunc_rcu_xdp", Xdp),
        ("accept/kfunc_rcu_tc", SchedCls),
        ("accept/kfunc_rcu_tracepoint", Tracepoint),
        ("accept/kfunc_rcu_kprobe", Kprobe),
    ] {
        let mut a = Asm::new();
        a.call_kfunc("bpf_rcu_read_lock");
        a.call(h::GET_SMP_PROCESSOR_ID);
        a.mov64(Reg::R6, Reg::R0);
        a.call_kfunc("bpf_rcu_read_unlock");
        a.mov64(Reg::R0, Reg::R6);
        a.alu64(AluOp::And, Reg::R0, 1);
        a.exit();
        add(name, None, Kernel::Accepts, fin(a, t));
    }
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_preempt_disable");
        a.call_kfunc("bpf_rcu_read_lock");
        a.call_kfunc("bpf_rcu_read_unlock");
        a.call_kfunc("bpf_preempt_enable");
        ret(&mut a, 2);
        add(
            "accept/kfunc_rcu_in_preempt",
            None,
            Kernel::Accepts,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_rcu_read_lock");
        a.call_kfunc("bpf_rcu_read_lock");
        a.call_kfunc("bpf_rcu_read_unlock");
        a.call_kfunc("bpf_rcu_read_unlock");
        ret(&mut a, 2);
        add(
            "reject/kfunc_rcu_nested",
            Some("kfunc.rcu_nested"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_rcu_read_unlock");
        ret(&mut a, 2);
        add(
            "reject/kfunc_rcu_unbalanced",
            Some("kfunc.unbalanced"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        a.call_kfunc("bpf_rcu_read_lock");
        ret(&mut a, 2);
        add(
            "reject/kfunc_rcu_held",
            Some("kfunc.region"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }

    // ----------------------------------------------------- irq flags ---
    {
        let mut a = Asm::new();
        fp(&mut a, -8);
        a.call_kfunc("bpf_local_irq_save");
        fp(&mut a, -16);
        a.call_kfunc("bpf_local_irq_save");
        fp(&mut a, -16);
        a.call_kfunc("bpf_local_irq_restore");
        fp(&mut a, -8);
        a.call_kfunc("bpf_local_irq_restore");
        ret(&mut a, 2);
        add(
            "accept/kfunc_irq_nested",
            None,
            Kernel::Accepts,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        fp(&mut a, -8);
        a.call_kfunc("bpf_local_irq_save");
        fp(&mut a, -16);
        a.call_kfunc("bpf_local_irq_save");
        fp(&mut a, -8);
        a.call_kfunc("bpf_local_irq_restore");
        fp(&mut a, -16);
        a.call_kfunc("bpf_local_irq_restore");
        ret(&mut a, 2);
        add(
            "reject/kfunc_irq_order",
            Some("kfunc.irq_order"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        a.st(Size::DW, Reg::R10, -8, 0);
        fp(&mut a, -8);
        a.call_kfunc("bpf_local_irq_restore");
        ret(&mut a, 2);
        add(
            "reject/kfunc_irq_not_saved",
            Some("kfunc.arg_type"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        fp(&mut a, -8);
        a.call_kfunc("bpf_local_irq_save");
        ret(&mut a, 2);
        add(
            "reject/kfunc_irq_held",
            Some("kfunc.region"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        fp(&mut a, -8);
        a.call_kfunc("bpf_local_irq_save");
        a.ldx(Size::DW, Reg::R0, Reg::R10, -8);
        fp(&mut a, -8);
        a.call_kfunc("bpf_local_irq_restore");
        ret(&mut a, 2);
        add(
            "reject/kfunc_irq_flag_read",
            Some("mem.kfunc_object"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }

    // ------------------------------------------------- bpf_iter_num ---
    add("accept/iter_num_sum", None, Kernel::Accepts, iter_sum(10));
    add(
        "accept/iter_num_counter",
        None,
        Kernel::Accepts,
        iter_counter(1000),
    );
    {
        // two nested iterators
        let mut a = Asm::new();
        let outer = a.label();
        let inner = a.label();
        let inner_done = a.label();
        let done = a.label();
        a.mov64(Reg::R6, 0);
        iter_new(&mut a, 0, 3);
        a.bind(outer).unwrap();
        iter_next(&mut a);
        a.jmp(JmpOp::Eq, Reg::R0, 0, done);
        fp(&mut a, -16);
        a.mov64(Reg::R2, 0);
        a.mov64(Reg::R3, 4);
        a.call_kfunc("bpf_iter_num_new");
        a.bind(inner).unwrap();
        fp(&mut a, -16);
        a.call_kfunc("bpf_iter_num_next");
        a.jmp(JmpOp::Eq, Reg::R0, 0, inner_done);
        a.add64(Reg::R6, 1);
        a.ja(inner);
        a.bind(inner_done).unwrap();
        fp(&mut a, -16);
        a.call_kfunc("bpf_iter_num_destroy");
        a.ja(outer);
        a.bind(done).unwrap();
        iter_destroy(&mut a);
        a.mov64(Reg::R0, Reg::R6);
        a.exit();
        add("accept/iter_num_nested", None, Kernel::Accepts, fin(a, Xdp));
    }
    {
        let mut a = Asm::new();
        iter_new(&mut a, 0, 4);
        ret(&mut a, 2);
        add(
            "reject/iter_num_leak",
            Some("kfunc.region"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        iter_new(&mut a, 0, 4);
        iter_new(&mut a, 0, 4);
        iter_destroy(&mut a);
        ret(&mut a, 2);
        add(
            "reject/iter_num_slot_in_use",
            Some("kfunc.slot_in_use"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        iter_new(&mut a, 0, 4);
        a.st(Size::DW, Reg::R10, -8, 0);
        iter_destroy(&mut a);
        ret(&mut a, 2);
        add(
            "reject/iter_num_overwrite",
            Some("mem.kfunc_object"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    {
        let mut a = Asm::new();
        a.st(Size::DW, Reg::R10, -8, 0);
        iter_next(&mut a);
        ret(&mut a, 2);
        add(
            "reject/iter_num_uninit_next",
            Some("kfunc.arg_type"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }
    for (name, rule, write, size) in [
        (
            "reject/iter_num_value_write",
            "mem.rdonly_write",
            true,
            Size::W,
        ),
        (
            "reject/iter_num_value_oob",
            "mem.rdonly_out_of_bounds",
            false,
            Size::DW,
        ),
    ] {
        let mut a = Asm::new();
        let done = a.label();
        iter_new(&mut a, 0, 4);
        iter_next(&mut a);
        a.jmp(JmpOp::Eq, Reg::R0, 0, done);
        if write {
            a.st(size, Reg::R0, 0, 1);
        } else {
            a.ldx(size, Reg::R1, Reg::R0, 0);
        }
        a.bind(done).unwrap();
        iter_destroy(&mut a);
        ret(&mut a, 2);
        add(name, Some(rule), Kernel::Rejects, fin(a, Xdp));
    }
    {
        // looping on next() of a drained iterator never ends
        let mut a = Asm::new();
        let head = a.label();
        let spin = a.label();
        iter_new(&mut a, 0, 1);
        a.bind(head).unwrap();
        iter_next(&mut a);
        a.jmp(JmpOp::Ne, Reg::R0, 0, head);
        a.bind(spin).unwrap();
        iter_next(&mut a);
        a.jmp(JmpOp::Eq, Reg::R0, 0, spin);
        iter_destroy(&mut a);
        ret(&mut a, 2);
        add(
            "reject/iter_num_drained_spin",
            Some("loop.unbounded"),
            Kernel::Rejects,
            fin(a, Xdp),
        );
    }

    // ------------------------------------------------------ widening ---
    add(
        "accept/bpf_loop_widened_counter",
        None,
        Kernel::Accepts,
        loop_counter(5),
    );
    {
        // the atomic form of the same counter, on a map value
        let mut a = Asm::new();
        let am = a
            .map(MapDef::new("am", MapKind::Array, 4, 8, 1))
            .expect("map");
        let cb = a.label();
        let out = a.label();
        a.st(Size::DW, Reg::R10, -8, 0);
        a.mov64(Reg::R1, 16);
        a.ld_func(Reg::R2, cb);
        a.mov64(Reg::R3, Reg::R10);
        a.add64(Reg::R3, -8);
        a.mov64(Reg::R4, 0);
        a.call(h::LOOP);
        ret(&mut a, 2);
        a.bind(cb).unwrap();
        a.mov64(Reg::R6, Reg::R2);
        a.st(Size::W, Reg::R10, -4, 0);
        a.ld_map(Reg::R1, am);
        a.mov64(Reg::R2, Reg::R10);
        a.add64(Reg::R2, -4);
        a.call(h::MAP_LOOKUP_ELEM);
        a.jmp(JmpOp::Eq, Reg::R0, 0, out);
        a.mov64(Reg::R1, 1);
        a.atomic(
            Size::DW,
            AtomicOp::Add { fetch: false },
            Reg::R0,
            0,
            Reg::R1,
        );
        a.ldx(Size::DW, Reg::R3, Reg::R6, 0);
        a.add64(Reg::R3, 1);
        a.stx(Size::DW, Reg::R6, 0, Reg::R3);
        a.bind(out).unwrap();
        ret(&mut a, 0);
        add(
            "accept/bpf_loop_widened_map",
            None,
            Kernel::Accepts,
            fin(a, Xdp),
        );
    }
    {
        // a kfunc this crate does not model, which the kernel accepts
        let mut a = Asm::new();
        a.call_kfunc("bpf_cast_to_kern_ctx");
        ret(&mut a, 2);
        add(
            "reject/kfunc_unmodelled",
            Some("call.kfunc_unsupported"),
            Kernel::AcceptsButStricterHere,
            fin(a, Xdp),
        );
    }
    v
}
