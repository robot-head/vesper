//! Typed release discipline and final bytecode checks, with real kernel
//! load/test-run evidence when bpf() is available. Set
//! VESPER_EBPF_REQUIRE_KERNEL=1 to make unavailable evidence a failure.

use vesper_ebpf::helpers as h;
use vesper_ebpf::insn::{AluOp, CallTarget, Insn, JmpOp, Reg, Size};
use vesper_ebpf::interp::{ListeningSocket, RingBufReader, XdpEnv, run_xdp};
use vesper_ebpf::{
    Asm, BpfBuilder, BuildError, LinearAudit, MapDef, Program, ProgramType, RingbufWakeup as Wake,
    SocketTuple, VerifierPolicy, verify,
};

fn mov(dst: Reg, src: impl Into<vesper_ebpf::insn::Operand>) -> Insn {
    Insn::Alu {
        wide: true,
        op: AluOp::Mov,
        dst,
        src: src.into(),
    }
}

fn ret(b: &mut BpfBuilder) {
    b.emit(mov(Reg::R0, 2));
    b.emit(Insn::Exit);
}

fn ring_builder() -> (BpfBuilder, usize) {
    let mut a = Asm::new();
    let ring = a.map(MapDef::ringbuf("events", 4096)).unwrap();
    a.call(h::GET_PRANDOM_U32);
    a.alu64(AluOp::And, Reg::R0, 1);
    a.mov64(Reg::R7, Reg::R0);
    (BpfBuilder::from_asm(a), ring)
}

fn ring_program() -> Program {
    let (mut b, ring) = ring_builder();
    let mut slot = b.reserve_ringbuf(ring, 8, Reg::R6).unwrap();
    slot.store(Size::DW, 0, 42).unwrap();
    // A second helper clobbers r0..r5; r6 must retain the reference.
    slot.emit(Insn::Call(CallTarget::Helper(h::KTIME_GET_NS)));
    slot.branch(
        JmpOp::Eq,
        Reg::R7,
        0,
        |slot| slot.submit(Wake::ForceWakeup),
        |slot| {
            slot.branch(
                JmpOp::Eq,
                Reg::R7,
                1,
                |slot| slot.discard(Wake::NoWakeup),
                |slot| slot.submit(Wake::Adaptive),
            )
        },
    );
    assert_eq!(b.audit(), LinearAudit::default());
    // Repeat acquisition after the join, this time keeping the pointer in r0.
    b.reserve_ringbuf(ring, 8, Reg::R0)
        .unwrap()
        .discard(Wake::Adaptive);
    ret(&mut b);
    b.finish("linear_ring", ProgramType::Xdp).unwrap()
}

fn socket_program(tuple: SocketTuple) -> Program {
    let mut a = Asm::new();
    a.mov64(Reg::R8, Reg::R1);
    a.call(h::GET_PRANDOM_U32);
    a.mov64(Reg::R7, Reg::R0);
    let mut b = BpfBuilder::from_asm(a);
    let mut socket = b.sk_lookup_tcp(Reg::R8, tuple, -40, Reg::R6).unwrap();
    socket.emit(Insn::Call(CallTarget::Helper(h::KTIME_GET_NS)));
    socket.branch(
        JmpOp::Eq,
        Reg::R7,
        0,
        |socket| socket.release(),
        |socket| {
            socket.branch(
                JmpOp::Gt,
                Reg::R7,
                10,
                |socket| socket.release(),
                |socket| socket.release(),
            )
        },
    );
    // Another lookup also exercises the joined null/success paths.
    b.sk_lookup_tcp(Reg::R8, tuple, -40, Reg::R0)
        .unwrap()
        .release();
    ret(&mut b);
    b.finish("linear_socket", ProgramType::Xdp).unwrap()
}

fn ipv4_tuple(daddr: [u8; 4], dport: u16) -> SocketTuple {
    let mut tuple = [0; 12];
    tuple[..4].copy_from_slice(&[127, 0, 0, 2]);
    tuple[4..8].copy_from_slice(&daddr);
    tuple[8..10].copy_from_slice(&12345u16.to_be_bytes());
    tuple[10..12].copy_from_slice(&dport.to_be_bytes());
    SocketTuple::Ipv4(tuple)
}

#[test]
fn ring_branches_repeated_acquisitions_and_full_ring_run_without_leaks() {
    let p = ring_program();
    verify(&p, &VerifierPolicy::default()).unwrap();
    let mut env = XdpEnv::new(&p);
    for _ in 0..1000 {
        assert_eq!(run_xdp(&p, &mut env, &[0; 64]).unwrap().ret, 2);
    }
    let ring = env.ring(0).unwrap();
    let mut reader = RingBufReader::new(&ring.data, ring.consumer, ring.producer).unwrap();
    let mut records = 0;
    while let Some(bytes) = reader.next_record().unwrap() {
        assert_eq!(bytes, 42u64.to_le_bytes());
        records += 1;
    }
    assert!(records > 0);
    // Less than one header + payload remains: later reserves take the null path.
    assert!(ring.producer - ring.consumer + 16 > ring.data.len() as u64 - 1);
}

#[test]
fn both_socket_tuple_families_and_nested_release_arms_verify() {
    for tuple in [ipv4_tuple([127, 0, 0, 1], 8080), SocketTuple::Ipv6([0; 36])] {
        let p = socket_program(tuple);
        verify(&p, &VerifierPolicy::default()).unwrap();
        let mut env = XdpEnv::new(&p);
        assert_eq!(run_xdp(&p, &mut env, &[0; 64]).unwrap().ret, 2);
        // IPv4 now hits a listener, exercising real release in the interpreter.
        // Its IPv6 lookup model always returns null; the verifier checks both arms.
        env.sockets.push(ListeningSocket {
            daddr: [127, 0, 0, 1],
            dport: 8080u16.to_be_bytes(),
        });
        for _ in 0..100 {
            assert_eq!(run_xdp(&p, &mut env, &[0; 64]).unwrap().ret, 2);
        }
    }
}

#[test]
fn dropping_or_forgetting_either_handle_refuses_finalization() {
    for forget in [false, true] {
        let (mut b, ring) = ring_builder();
        let slot = b.reserve_ringbuf(ring, 8, Reg::R6).unwrap();
        if forget {
            std::mem::forget(slot);
        } else {
            drop(slot);
        }
        let expected = LinearAudit {
            outstanding: 1,
            dropped: usize::from(!forget),
        };
        assert_eq!(b.audit(), expected);
        ret(&mut b);
        assert_eq!(
            b.finish("leak", ProgramType::Xdp),
            Err(BuildError::Unreleased(expected))
        );

        let mut b = BpfBuilder::new();
        let socket = b
            .sk_lookup_tcp(Reg::R1, SocketTuple::Ipv4([0; 12]), -16, Reg::R6)
            .unwrap();
        if forget {
            std::mem::forget(socket);
        } else {
            drop(socket);
        }
        assert_eq!(b.audit(), expected);
        ret(&mut b);
        assert_eq!(
            b.finish("leak", ProgramType::Xdp),
            Err(BuildError::Unreleased(expected))
        );
    }
}

#[test]
fn leaking_one_branch_arm_is_detected_for_both_handle_kinds() {
    let (mut b, ring) = ring_builder();
    b.reserve_ringbuf(ring, 8, Reg::R6).unwrap().branch(
        JmpOp::Eq,
        Reg::R7,
        0,
        |slot| slot.submit(Wake::Adaptive),
        |slot| std::mem::forget(slot),
    );
    ret(&mut b);
    assert!(matches!(
        b.finish("leak", ProgramType::Xdp),
        Err(BuildError::Unreleased(LinearAudit {
            outstanding: 1,
            dropped: 0
        }))
    ));

    let mut b = BpfBuilder::new();
    b.sk_lookup_tcp(Reg::R1, SocketTuple::Ipv4([0; 12]), -16, Reg::R6)
        .unwrap()
        .branch(
            JmpOp::Eq,
            Reg::R6,
            0,
            |socket| socket.release(),
            |socket| drop(socket),
        );
    ret(&mut b);
    assert!(matches!(
        b.finish("leak", ProgramType::Xdp),
        Err(BuildError::Unreleased(LinearAudit {
            outstanding: 1,
            dropped: 1
        }))
    ));
}

#[test]
fn raw_control_flow_cannot_skip_a_typed_release() {
    for socket in [false, true] {
        let (mut b, ring) = ring_builder();
        // For socket lookup preserve ctx before the helper in ring_builder.
        if socket {
            let mut a = Asm::new();
            a.mov64(Reg::R8, Reg::R1);
            a.call(h::GET_PRANDOM_U32);
            a.mov64(Reg::R7, Reg::R0);
            b = BpfBuilder::from_asm(a);
            let mut socket = b
                .sk_lookup_tcp(Reg::R8, SocketTuple::Ipv4([0; 12]), -16, Reg::R6)
                .unwrap();
            socket.emit(Insn::Jcond {
                wide: true,
                op: JmpOp::Eq,
                dst: Reg::R7,
                src: 0.into(),
                off: 2,
            });
            socket.release();
        } else {
            let mut slot = b.reserve_ringbuf(ring, 8, Reg::R6).unwrap();
            slot.emit(Insn::Jcond {
                wide: true,
                op: JmpOp::Eq,
                dst: Reg::R7,
                src: 0.into(),
                off: 3,
            });
            slot.discard(Wake::Adaptive);
        }
        assert_eq!(b.audit(), LinearAudit::default());
        ret(&mut b);
        assert!(
            matches!(b.finish("raw_leak", ProgramType::Xdp), Err(BuildError::Verify(e)) if e.rule == "ref.leak")
        );
    }
}

#[test]
fn manually_double_releasing_is_rejected_by_the_verifier() {
    let (mut b, ring) = ring_builder();
    let mut slot = b.reserve_ringbuf(ring, 8, Reg::R6).unwrap();
    slot.emit(mov(Reg::R1, Reg::R6));
    slot.emit(mov(Reg::R2, 0));
    slot.emit(Insn::Call(CallTarget::Helper(h::RINGBUF_DISCARD)));
    slot.submit(Wake::Adaptive);
    ret(&mut b);
    assert!(matches!(
        b.finish("twice", ProgramType::Xdp),
        Err(BuildError::Verify(_))
    ));

    let mut b = BpfBuilder::new();
    let mut socket = b
        .sk_lookup_tcp(Reg::R1, SocketTuple::Ipv4([0; 12]), -16, Reg::R6)
        .unwrap();
    socket.emit(mov(Reg::R1, Reg::R6));
    socket.emit(Insn::Call(CallTarget::Helper(h::SK_RELEASE)));
    socket.release();
    ret(&mut b);
    assert!(matches!(
        b.finish("twice", ProgramType::Xdp),
        Err(BuildError::Verify(_))
    ));
}

#[test]
fn bad_arguments_emit_nothing_and_failed_writes_retain_the_owner() {
    let (mut b, ring) = ring_builder();
    for (map, bytes, reg) in [
        (usize::MAX, 8, Reg::R6),
        (ring, 0, Reg::R6),
        (ring, u32::MAX, Reg::R6),
        (ring, 8, Reg::R10),
    ] {
        assert!(matches!(
            b.reserve_ringbuf(map, bytes, reg),
            Err(BuildError::InvalidArgument(_))
        ));
    }
    for off in [-513, -510, -4, 0, i16::MAX] {
        assert!(matches!(
            b.sk_lookup_tcp(Reg::R1, SocketTuple::Ipv4([0; 12]), off, Reg::R6),
            Err(BuildError::InvalidArgument(_))
        ));
    }
    let mut slot = b.reserve_ringbuf(ring, 8, Reg::R6).unwrap();
    for off in [-1, 1, 8, i16::MAX] {
        assert!(slot.store(Size::DW, off, 1).is_err());
    }
    slot.discard(Wake::Adaptive);
    ret(&mut b);
    b.finish("recovered", ProgramType::Xdp).unwrap();
}

#[test]
fn imported_raw_reference_leaks_are_also_checked() {
    let mut a = Asm::new();
    let ring = a.map(MapDef::ringbuf("events", 4096)).unwrap();
    a.ld_map(Reg::R1, ring);
    a.mov64(Reg::R2, 8);
    a.mov64(Reg::R3, 0);
    a.call(h::RINGBUF_RESERVE);
    let mut b = BpfBuilder::from_asm(a);
    ret(&mut b);
    assert!(
        matches!(b.finish("raw_leak", ProgramType::Xdp), Err(BuildError::Verify(e)) if e.rule == "ref.leak")
    );
}

#[test]
fn interrupted_branch_construction_cannot_finalize_a_partial_program() {
    let (mut b, ring) = ring_builder();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        b.reserve_ringbuf(ring, 8, Reg::R6).unwrap().branch(
            JmpOp::Eq,
            Reg::R7,
            0,
            |slot| slot.submit(Wake::Adaptive),
            |slot| {
                slot.discard(Wake::Adaptive);
                panic!("interrupted emission");
            },
        );
    }));
    assert!(result.is_err());
    assert_eq!(b.audit(), LinearAudit::default());
    ret(&mut b);
    assert!(matches!(
        b.finish("partial", ProgramType::Xdp),
        Err(BuildError::Asm(_))
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn kernel_loads_and_runs_builder_programs() {
    use vesper_ebpf::kernel::{self, LoadError};
    match kernel::probe() {
        Ok(()) => (),
        Err(e @ LoadError::Unavailable { .. }) => {
            assert!(
                std::env::var_os("VESPER_EBPF_REQUIRE_KERNEL").is_none(),
                "kernel BPF evidence required: {e}"
            );
            eprintln!("kernel builder load evidence unavailable on this host: {e}");
            return;
        }
        Err(e) => panic!("kernel probe failed: {e}"),
    }
    for p in [
        ring_program(),
        socket_program(SocketTuple::Ipv4([0; 12])),
        socket_program(SocketTuple::Ipv6([0; 36])),
    ] {
        let loaded = kernel::load(&p).unwrap_or_else(|e| panic!("{} failed to load: {e}", p.name));
        for _ in 0..10 {
            assert_eq!(kernel::test_run(&loaded.prog, &[0; 64]).unwrap().retval, 2);
        }
        eprintln!("kernel loaded {} and completed 10 test runs", p.name);
    }

    // Acquire an actual listening socket, rather than only executing the null
    // lookup path. The result marker is emitted only inside the non-null scope.
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let tuple = ipv4_tuple([127, 0, 0, 1], listener.local_addr().unwrap().port());
    let mut b = BpfBuilder::new();
    b.emit(mov(Reg::R9, 2));
    let mut socket = b.sk_lookup_tcp(Reg::R1, tuple, -16, Reg::R6).unwrap();
    socket.emit(mov(Reg::R9, 3));
    socket.release();
    b.emit(mov(Reg::R0, Reg::R9));
    b.emit(Insn::Exit);
    let p = b.finish("socket_hit", ProgramType::Xdp).unwrap();
    let loaded = kernel::load(&p).unwrap();
    assert_eq!(kernel::test_run(&loaded.prog, &[0; 64]).unwrap().retval, 3);
    eprintln!("kernel acquired and released an actual TCP listener");
}
