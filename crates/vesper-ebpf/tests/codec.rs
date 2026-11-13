//! Instruction codec: every decodable slot sequence re-encodes to the same
//! bytes, random bytes never panic the decoder, and every instruction
//! family round-trips.

mod common;

use common::Rng;
use vesper_ebpf::insn::{
    AluOp, AtomicOp, CallTarget, Endian, Imm64, Insn, JmpOp, Operand, RawInsn, Reg, Size, decode,
    decode_all, encode, slots_from_bytes, slots_to_bytes,
};

fn regs() -> Vec<Reg> {
    (0..=10).map(|n| Reg::new(n).unwrap()).collect()
}

fn families() -> Vec<Insn> {
    let mut v = Vec::new();
    let r = regs();
    let alu = [
        AluOp::Add,
        AluOp::Sub,
        AluOp::Mul,
        AluOp::Div,
        AluOp::SDiv,
        AluOp::Or,
        AluOp::And,
        AluOp::Lsh,
        AluOp::Rsh,
        AluOp::Mod,
        AluOp::SMod,
        AluOp::Xor,
        AluOp::Mov,
        AluOp::Arsh,
    ];
    for wide in [false, true] {
        for op in alu {
            v.push(Insn::Alu {
                wide,
                op,
                dst: r[3],
                src: Operand::Reg(r[4]),
            });
            v.push(Insn::Alu {
                wide,
                op,
                dst: r[3],
                src: Operand::Imm(-7),
            });
        }
        v.push(Insn::Alu {
            wide,
            op: AluOp::Neg,
            dst: r[1],
            src: Operand::Imm(0),
        });
        for bits in [8u8, 16] {
            v.push(Insn::Alu {
                wide,
                op: AluOp::MovSx(bits),
                dst: r[2],
                src: Operand::Reg(r[5]),
            });
        }
    }
    v.push(Insn::Alu {
        wide: true,
        op: AluOp::MovSx(32),
        dst: r[2],
        src: Operand::Reg(r[5]),
    });
    for bits in [16u8, 32, 64] {
        for kind in [Endian::ToLe, Endian::ToBe, Endian::Swap] {
            v.push(Insn::Endian {
                kind,
                bits,
                dst: r[6],
            });
        }
    }
    let jmps = [
        JmpOp::Eq,
        JmpOp::Gt,
        JmpOp::Ge,
        JmpOp::Set,
        JmpOp::Ne,
        JmpOp::SGt,
        JmpOp::SGe,
        JmpOp::Lt,
        JmpOp::Le,
        JmpOp::SLt,
        JmpOp::SLe,
    ];
    for wide in [false, true] {
        for op in jmps {
            v.push(Insn::Jcond {
                wide,
                op,
                dst: r[1],
                src: Operand::Reg(r[2]),
                off: -3,
            });
            v.push(Insn::Jcond {
                wide,
                op,
                dst: r[1],
                src: Operand::Imm(99),
                off: 17,
            });
        }
    }
    v.push(Insn::Ja { off: -1 });
    v.push(Insn::Ja { off: 70_000 }); // gotol
    v.push(Insn::Call(CallTarget::Helper(1)));
    v.push(Insn::Call(CallTarget::Local(-5)));
    v.push(Insn::Call(CallTarget::Kfunc(1234)));
    v.push(Insn::Exit);
    v.push(Insn::LdImm64 {
        dst: r[1],
        imm: Imm64::Value(0x0123_4567_89ab_cdef),
    });
    v.push(Insn::LdImm64 {
        dst: r[1],
        imm: Imm64::MapFd(3),
    });
    for size in [Size::B, Size::H, Size::W, Size::DW] {
        v.push(Insn::Ldx {
            size,
            dst: r[0],
            src: r[10],
            off: -8,
            sign_extend: false,
        });
        v.push(Insn::St {
            size,
            dst: r[10],
            off: -16,
            imm: 5,
        });
        v.push(Insn::Stx {
            size,
            dst: r[10],
            src: r[3],
            off: -24,
        });
        if size != Size::DW {
            v.push(Insn::Ldx {
                size,
                dst: r[0],
                src: r[2],
                off: 1,
                sign_extend: true,
            });
        }
    }
    for size in [Size::W, Size::DW] {
        for op in [
            AtomicOp::Add { fetch: false },
            AtomicOp::Add { fetch: true },
            AtomicOp::Or { fetch: false },
            AtomicOp::Or { fetch: true },
            AtomicOp::And { fetch: false },
            AtomicOp::And { fetch: true },
            AtomicOp::Xor { fetch: false },
            AtomicOp::Xor { fetch: true },
            AtomicOp::Xchg,
            AtomicOp::CmpXchg,
        ] {
            v.push(Insn::Atomic {
                size,
                op,
                dst: r[10],
                src: r[1],
                off: -8,
            });
        }
    }
    v
}

#[test]
fn every_family_round_trips() {
    for i in families() {
        let slots = encode(&i);
        assert_eq!(slots.len(), i.slots());
        assert_eq!(decode(&slots, 0), Ok(i), "{i:?} -> {slots:?}");
        let bytes = slots_to_bytes(&slots);
        assert_eq!(slots_from_bytes(&bytes).unwrap(), slots);
    }
}

#[test]
fn decodable_random_slots_are_canonical() {
    let mut rng = Rng(42);
    let mut decoded = 0;
    for _ in 0..500_000 {
        let n = 1 + rng.below(3) as usize;
        let mut slots: Vec<RawInsn> = (0..n)
            .map(|_| {
                let b: [u8; 8] = rng.bytes(8).try_into().unwrap();
                RawInsn::from_bytes(b)
            })
            .collect();
        if rng.chance(50) {
            // bias towards small immediates and registers
            for s in &mut slots {
                s.dst %= 11;
                s.src %= 11;
                if rng.chance(50) {
                    s.imm = (s.imm as i8) as i32;
                    s.off = (s.off as i8) as i16;
                }
            }
        }
        if let Ok(i) = decode(&slots, 0) {
            decoded += 1;
            let re = encode(&i);
            assert_eq!(&re[..], &slots[..i.slots()], "{i:?} is not canonical");
        }
        let _ = decode_all(&slots);
    }
    assert!(decoded > 2_000, "only {decoded} decodable");
}

#[test]
fn malformed_input_is_a_typed_error() {
    assert!(slots_from_bytes(&[0u8; 7]).is_err());
    // ld_imm64 missing its second slot
    let ld = encode(&Insn::LdImm64 {
        dst: Reg::R1,
        imm: Imm64::Value(1),
    });
    assert!(decode(&ld[..1], 0).is_err());
    // register 11
    assert!(decode(&[RawInsn::new(0xb7, 11, 0, 0, 0)], 0).is_err());
    // unknown opcode
    assert!(decode(&[RawInsn::new(0xff, 0, 0, 0, 0)], 0).is_err());
    assert!(decode(&[], 0).is_err());
}
