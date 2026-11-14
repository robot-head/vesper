//! Shared test support: a deterministic PRNG and a random XDP program
//! generator biased towards programs that are close to verifiable.
#![allow(dead_code)]

pub mod corpus;
pub mod depth;
pub mod kfuncs;

use vesper_ebpf::helpers as h;
use vesper_ebpf::insn::{AluOp, AtomicOp, Endian, JmpOp, Reg, Size};
use vesper_ebpf::{Asm, MapDef, MapKind, Program, ProgramType};

/// xorshift64* (deterministic, no dependency).
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

pub const SCALARS: [Reg; 5] = [Reg::R0, Reg::R5, Reg::R7, Reg::R8, Reg::R9];

const ALU_OPS: [AluOp; 13] = [
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
];

const JMP_OPS: [JmpOp; 11] = [
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

const SIZES: [Size; 4] = [Size::B, Size::H, Size::W, Size::DW];

/// Options for [`random_program`].
#[derive(Clone, Copy, Debug)]
pub struct GenOptions {
    pub items: u64,
    /// Allow helpers whose results differ between the kernel and the
    /// interpreter (`bpf_get_prandom_u32`, `bpf_ktime_get_ns`).
    pub nondeterministic_helpers: bool,
}

impl Default for GenOptions {
    fn default() -> Self {
        Self {
            items: 14,
            nondeterministic_helpers: false,
        }
    }
}

fn imm(rng: &mut Rng) -> i32 {
    match rng.below(6) {
        0 => 0,
        1 => 1,
        2 => -1,
        3 => rng.below(64) as i32,
        4 => rng.below(0x1_0000) as i32,
        _ => rng.next() as i32,
    }
}

/// Reload `r2 = data`, `r3 = data_end` from the saved ctx in r6 and
/// re-establish a packet range of `n` bytes (else exit through `out`).
fn reload_packet(asm: &mut Asm, n: i32, out: vesper_ebpf::program::Label) {
    asm.ldx(Size::W, Reg::R2, Reg::R6, 0);
    asm.ldx(Size::W, Reg::R3, Reg::R6, 4);
    asm.mov64(Reg::R4, Reg::R2);
    asm.add64(Reg::R4, n);
    asm.jmp(JmpOp::Gt, Reg::R4, Reg::R3, out);
    asm.mov64(Reg::R5, n);
}

/// A random XDP program over maps `counts` (hash u32 -> u64), `arr`
/// (array u32 -> 16 bytes) and `ring` (ringbuf).
pub fn random_program(rng: &mut Rng, opts: GenOptions) -> Program {
    let mut asm = Asm::new();
    let hash = asm
        .map(MapDef::new("counts", MapKind::Hash, 4, 8, 64))
        .expect("map");
    let arr = asm
        .map(MapDef::new("arr", MapKind::Array, 4, 16, 4))
        .expect("map");
    let ring = asm.map(MapDef::ringbuf("ring", 4096)).expect("map");
    let out = asm.label();
    let checked = rng.below(70) as i32;
    asm.mov64(Reg::R6, Reg::R1);
    reload_packet(&mut asm, checked, out);
    for r in SCALARS {
        if rng.chance(97) {
            let v = imm(rng);
            asm.mov64(r, v);
        }
    }
    let mut open: Vec<(vesper_ebpf::program::Label, u64)> = Vec::new();
    for _ in 0..opts.items {
        gen_item(rng, &mut asm, opts, checked, out, hash, arr, ring);
        // close forward jumps whose distance ran out
        open.retain_mut(|(l, left)| {
            if *left == 0 {
                asm.bind(*l).expect("bind");
                false
            } else {
                *left -= 1;
                true
            }
        });
        if rng.chance(15) {
            let l = asm.label();
            let op = rng.pick(&JMP_OPS);
            let a = rng.pick(&SCALARS);
            if rng.chance(50) {
                let b = imm(rng);
                if rng.chance(20) {
                    asm.jmp32(op, a, b, l);
                } else {
                    asm.jmp(op, a, b, l);
                }
            } else {
                let b = rng.pick(&SCALARS);
                asm.jmp(op, a, b, l);
            }
            open.push((l, rng.below(4)));
        }
    }
    for (l, _) in open {
        asm.bind(l).expect("bind");
    }
    // return a scalar derived from the state, clamped to an XDP action
    if rng.chance(50) {
        asm.mov64(Reg::R0, rng.below(4) as i32);
    } else {
        asm.alu64(AluOp::And, Reg::R0, 3);
    }
    asm.exit();
    asm.bind(out).expect("bind");
    asm.mov64(Reg::R0, 2);
    asm.exit();
    asm.finish("fuzz", ProgramType::Xdp).expect("assemble")
}

#[allow(clippy::too_many_arguments)]
fn gen_item(
    rng: &mut Rng,
    asm: &mut Asm,
    opts: GenOptions,
    checked: i32,
    out: vesper_ebpf::program::Label,
    hash: usize,
    arr: usize,
    ring: usize,
) {
    let d = rng.pick(&SCALARS);
    match rng.below(20) {
        0..=4 => {
            let op = rng.pick(&ALU_OPS);
            let wide = rng.chance(70);
            if op == AluOp::Neg {
                if wide {
                    asm.alu64(op, d, 0);
                } else {
                    asm.alu32(op, d, 0);
                }
            } else if rng.chance(60) {
                let mut i = imm(rng);
                if matches!(op, AluOp::Lsh | AluOp::Rsh | AluOp::Arsh) && rng.chance(90) {
                    i = (i as u32 % if wide { 64 } else { 32 }) as i32;
                }
                if matches!(op, AluOp::Div | AluOp::Mod) && i == 0 && rng.chance(90) {
                    i = 3;
                }
                if wide {
                    asm.alu64(op, d, i);
                } else {
                    asm.alu32(op, d, i);
                }
            } else {
                let s = rng.pick(&SCALARS);
                if wide {
                    asm.alu64(op, d, s);
                } else {
                    asm.alu32(op, d, s);
                }
            }
        }
        5 => {
            let kind = rng.pick(&[Endian::ToBe, Endian::ToLe, Endian::Swap]);
            let bits = rng.pick(&[16u8, 32, 64]);
            asm.endian(kind, bits, d);
        }
        6 | 7 => {
            // packet load, sometimes past the checked range
            let size = rng.pick(&SIZES);
            let off = if rng.chance(85) {
                rng.below(checked.max(1) as u64) as i16
            } else {
                rng.below(80) as i16
            };
            asm.ldx(size, d, Reg::R2, off);
        }
        8 => {
            // packet store
            let size = rng.pick(&SIZES);
            let off = rng.below((checked + 4).max(1) as u64) as i16;
            asm.stx(size, Reg::R2, off, d);
        }
        9 => {
            // stack store then load
            let size = rng.pick(&SIZES);
            let slot = -(8 * (1 + rng.below(6) as i16));
            let off = slot
                + if rng.chance(80) {
                    0
                } else {
                    rng.below(8) as i16
                };
            if rng.chance(50) {
                asm.stx(size, Reg::R10, off, d);
            } else {
                asm.st(size, Reg::R10, off, imm(rng));
            }
        }
        10 => {
            let size = rng.pick(&SIZES);
            let slot = -(8 * (1 + rng.below(6) as i16));
            asm.ldx(size, d, Reg::R10, slot);
        }
        11 => {
            // variable-offset packet access with its own bounds check
            let skip = asm.label();
            let mask = rng.pick(&[0x7, 0xf, 0x3f, 0xff, 0xffff, -1]);
            let n = rng.pick(&[1, 2, 4, 8]);
            asm.mov64(Reg::R4, d);
            asm.alu64(AluOp::And, Reg::R4, mask);
            asm.add64(Reg::R4, Reg::R2);
            asm.mov64(Reg::R1, Reg::R4);
            asm.add64(Reg::R1, if rng.chance(85) { n } else { n - 1 });
            asm.jmp(JmpOp::Gt, Reg::R1, Reg::R3, skip);
            let size = Size::from_bytes(n as u32).expect("size");
            asm.ldx(size, d, Reg::R4, 0);
            asm.bind(skip).expect("bind");
        }
        12 | 13 => {
            // map lookup with a stack key, sometimes without a null check
            let use_arr = rng.chance(40);
            let m = if use_arr { arr } else { hash };
            asm.stx(Size::W, Reg::R10, -4, d);
            asm.ld_map(Reg::R1, m);
            asm.mov64(Reg::R2, Reg::R10);
            asm.add64(Reg::R2, -4);
            asm.call(h::MAP_LOOKUP_ELEM);
            let skip = asm.label();
            if rng.chance(92) {
                asm.jmp(JmpOp::Eq, Reg::R0, 0, skip);
            }
            let vs = if use_arr { 16 } else { 8 };
            let size = rng.pick(&SIZES);
            let off = if rng.chance(85) {
                rng.below((vs - size.bytes() as u64) + 1) as i16
            } else {
                rng.below(24) as i16
            };
            let v = rng.pick(&[Reg::R7, Reg::R8, Reg::R9]);
            match rng.below(3) {
                0 => {
                    asm.ldx(size, v, Reg::R0, off);
                }
                1 => {
                    asm.stx(size, Reg::R0, off, v);
                }
                _ => {
                    let sz = rng.pick(&[Size::W, Size::DW]);
                    let off = if rng.chance(90) { 0 } else { off };
                    asm.atomic(
                        sz,
                        AtomicOp::Add {
                            fetch: rng.chance(50),
                        },
                        Reg::R0,
                        off,
                        v,
                    );
                }
            }
            asm.bind(skip).expect("bind");
            asm.mov64(Reg::R0, Reg::R7);
            reload_packet(asm, checked, out);
        }
        14 => {
            // hash update from stack key/value
            asm.stx(Size::W, Reg::R10, -4, d);
            asm.stx(Size::DW, Reg::R10, -16, rng.pick(&SCALARS));
            asm.ld_map(Reg::R1, hash);
            asm.mov64(Reg::R2, Reg::R10);
            asm.add64(Reg::R2, -4);
            asm.mov64(Reg::R3, Reg::R10);
            asm.add64(Reg::R3, -16);
            asm.mov64(Reg::R4, 0);
            asm.call(h::MAP_UPDATE_ELEM);
            reload_packet(asm, checked, out);
        }
        15 | 16 => {
            // ringbuf reserve, fill, release (sometimes forgotten)
            let size = rng.pick(&[8, 16, 24]);
            asm.ld_map(Reg::R1, ring);
            asm.mov64(Reg::R2, size);
            asm.mov64(Reg::R3, 0);
            asm.call(h::RINGBUF_RESERVE);
            let skip = asm.label();
            if rng.chance(95) {
                asm.jmp(JmpOp::Eq, Reg::R0, 0, skip);
            }
            let off = if rng.chance(90) {
                8 * rng.below(size as u64 / 8) as i16
            } else {
                size as i16
            };
            asm.stx(
                Size::DW,
                Reg::R0,
                off,
                rng.pick(&[Reg::R7, Reg::R8, Reg::R9]),
            );
            let release = rng.below(20);
            if release < 18 {
                asm.mov64(Reg::R1, Reg::R0);
                asm.mov64(Reg::R2, 0);
                asm.call(if release < 12 {
                    h::RINGBUF_SUBMIT
                } else {
                    h::RINGBUF_DISCARD
                });
            }
            asm.bind(skip).expect("bind");
            asm.mov64(Reg::R0, Reg::R8);
            reload_packet(asm, checked, out);
        }
        17 => {
            if opts.nondeterministic_helpers {
                asm.call(if rng.chance(50) {
                    h::GET_PRANDOM_U32
                } else {
                    h::KTIME_GET_NS
                });
                asm.mov64(Reg::R9, Reg::R0);
            } else {
                asm.call(h::GET_SMP_PROCESSOR_ID);
                asm.mov64(Reg::R9, Reg::R0);
            }
            reload_packet(asm, checked, out);
        }
        18 => {
            // spill and refill the packet pointer
            asm.stx(Size::DW, Reg::R10, -48, Reg::R2);
            asm.ldx(Size::DW, Reg::R2, Reg::R10, -48);
        }
        _ => {
            // a bounded counting loop
            let n = rng.below(6) as i32 + 1;
            asm.mov64(Reg::R4, 0);
            let head = asm.here();
            asm.add64(d, Reg::R4);
            asm.add64(Reg::R4, 1);
            asm.jmp(JmpOp::Lt, Reg::R4, n, head);
        }
    }
}

/// A random packet: a plausible Ethernet/IPv4/TCP header half of the time.
pub fn random_packet(rng: &mut Rng) -> Vec<u8> {
    let n = rng.below(100) as usize;
    let mut p = rng.bytes(n);
    if n >= 54 && rng.chance(50) {
        p[12] = 0x08;
        p[13] = 0x00;
        p[23] = 6;
    }
    p
}

/// The same random program as another program type: `data`/`data_end`
/// loads (`ldx w, [r6 + 0/4]`) move to the type's context layout
/// (`__sk_buff` offsets 76/80 for tc; for tracing types the loads become
/// plain scalar reads at valid offsets, so packet accesses turn into
/// scalar dereferences both verifiers reject).
pub fn retarget(prog: &Program, t: ProgramType) -> Program {
    let mut p = prog.clone();
    p.prog_type = t;
    for i in &mut p.insns {
        if i.code == 0x61 && i.src == 6 && (i.off == 0 || i.off == 4) {
            i.off = match t {
                ProgramType::Xdp => i.off,
                ProgramType::SchedCls => 76 + i.off,
                ProgramType::Tracepoint => 8 + i.off,
                ProgramType::Kprobe => i.off,
            };
        }
    }
    p
}
