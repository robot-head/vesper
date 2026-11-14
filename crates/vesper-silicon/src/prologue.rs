//! Stack-pointer accounting read from the machine code Cranelift emitted.
//!
//! The decoder recognises the prologue sequences Cranelift 0.125 emits on
//! x86_64, AArch64 and riscv64 (with or without the compressed `Zca`/`Zcd`
//! encodings) and sums every stack-pointer decrement up to the first
//! instruction that is not part of a recognised prologue. It deliberately
//! knows nothing about the IR: the number it returns is what the processor
//! does to SP on entry. A sequence it cannot classify is not guessed at; the
//! caller cross-checks the result against Cranelift's own frame layout and
//! rejects a mismatch.

/// Stack bytes consumed by one activation, measured from machine code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeasuredPrologue {
    /// Bytes the caller's `call` pushes (the x86_64 return address); zero on
    /// AArch64, where `bl` writes the link register instead.
    pub call_push_bytes: u32,
    /// Bytes pushed by the frame-record store (`push rbp` / `stp x29, x30`).
    pub frame_record_bytes: u32,
    /// Bytes pushed for callee-saved registers with pre-indexed stores.
    pub clobber_push_bytes: u32,
    /// Bytes reserved by explicit `sub sp` instructions.
    pub sp_sub_bytes: u32,
    /// Number of machine instructions classified as prologue.
    pub instructions: u32,
}

impl MeasuredPrologue {
    /// Total stack bytes this activation occupies below the caller's SP.
    pub fn total(&self) -> Option<u32> {
        self.call_push_bytes
            .checked_add(self.frame_record_bytes)?
            .checked_add(self.clobber_push_bytes)?
            .checked_add(self.sp_sub_bytes)
    }
}

/// x86_64: `push rbp; mov rbp, rsp; sub rsp, imm; mov [rsp+off], reg...`.
pub fn measure_x86_64(code: &[u8]) -> MeasuredPrologue {
    let mut m = MeasuredPrologue {
        call_push_bytes: 8,
        frame_record_bytes: 0,
        clobber_push_bytes: 0,
        sp_sub_bytes: 0,
        instructions: 0,
    };
    let mut i = 0usize;
    loop {
        let rest = &code[i.min(code.len())..];
        // endbr64 (CET landing pad) may precede the frame setup.
        if rest.starts_with(&[0xf3, 0x0f, 0x1e, 0xfa]) {
            i += 4;
        } else if rest.first() == Some(&0x55) {
            // push rbp
            m.frame_record_bytes = m.frame_record_bytes.saturating_add(8);
            i += 1;
        } else if rest.starts_with(&[0x48, 0x89, 0xe5]) {
            // mov rbp, rsp
            i += 3;
        } else if rest.len() >= 4 && rest[..3] == [0x48, 0x83, 0xec] {
            // sub rsp, imm8 (sign-extended; Cranelift only emits positive)
            let imm = rest[3] as i8;
            if imm < 0 {
                break;
            }
            m.sp_sub_bytes = m.sp_sub_bytes.saturating_add(imm as u32);
            i += 4;
        } else if rest.len() >= 7 && rest[..3] == [0x48, 0x81, 0xec] {
            // sub rsp, imm32
            let imm = i32::from_le_bytes([rest[3], rest[4], rest[5], rest[6]]);
            if imm < 0 {
                break;
            }
            m.sp_sub_bytes = m.sp_sub_bytes.saturating_add(imm as u32);
            i += 7;
        } else if let Some(len) = x64_clobber_store_len(rest) {
            // mov [rsp + disp], r64 / movdqu [rsp + disp], xmm: no SP change.
            i += len;
        } else {
            break;
        }
        m.instructions += 1;
    }
    m
}

/// Recognises `mov qword [rsp + disp8/disp32], r64` (with optional REX.R/B)
/// and `movdqu [rsp + disp], xmm`, the forms Cranelift's x64 clobber save uses.
fn x64_clobber_store_len(b: &[u8]) -> Option<usize> {
    // Integer: REX.W(+R) 89 modrm(sib=rsp) sib=0x24 disp
    let (prefix, body) = match b.first()? {
        0x48 | 0x4c => (1usize, &b[1..]),
        0xf3 => {
            // movdqu: F3 [REX] 0F 7F modrm 24 disp
            let mut k = 1usize;
            if matches!(b.get(1), Some(0x44)) {
                k = 2;
            }
            if b.get(k..k + 2)? != [0x0f, 0x7f] {
                return None;
            }
            return modrm_rsp_disp(b.get(k + 2..)?).map(|l| k + 2 + l);
        }
        _ => return None,
    };
    if *body.first()? != 0x89 {
        return None;
    }
    modrm_rsp_disp(&body[1..]).map(|l| prefix + 1 + l)
}

/// modrm with rm=100 (SIB) and SIB=0x24 (base rsp, no index).
fn modrm_rsp_disp(b: &[u8]) -> Option<usize> {
    let modrm = *b.first()?;
    if modrm & 0x07 != 0x04 || *b.get(1)? != 0x24 {
        return None;
    }
    match modrm >> 6 {
        0 => Some(2),
        1 => (b.len() >= 3).then_some(3),
        2 => (b.len() >= 6).then_some(6),
        _ => None,
    }
}

/// AArch64: `[paciasp|bti] stp x29, x30, [sp, #-16]!; mov x29, sp;
/// stp/str ..., [sp, #-n]!; sub sp, sp, #imm | movz/movk x16 + sub sp, sp, x16`.
pub fn measure_aarch64(code: &[u8]) -> MeasuredPrologue {
    let mut m = MeasuredPrologue {
        call_push_bytes: 0,
        frame_record_bytes: 0,
        clobber_push_bytes: 0,
        sp_sub_bytes: 0,
        instructions: 0,
    };
    // Value tracked for the scratch register Cranelift uses for large frames.
    let mut x16: Option<u64> = None;
    let (words, _tail) = code.as_chunks::<4>();
    for word in words {
        let w = u32::from_le_bytes(*word);
        let rn = (w >> 5) & 31;
        if w == 0xd503_233f || w == 0xd503_237f || (w & 0xffff_ff3f) == 0xd503_241f {
            // paciasp, pacibsp, bti {c,j,jc}
        } else if w == 0xa9bf_7bfd {
            // stp x29, x30, [sp, #-16]!
            m.frame_record_bytes = m.frame_record_bytes.saturating_add(16);
        } else if w == 0x9100_03fd {
            // mov x29, sp
        } else if (w & 0xffc0_0000) == 0xa980_0000 || (w & 0xffc0_0000) == 0x6d80_0000 {
            // stp x/d, [sp, #imm7*8]! (pre-index)
            if rn != 31 {
                break;
            }
            let imm7 = ((w >> 15) & 0x7f) as i32;
            let imm7 = if imm7 & 0x40 != 0 { imm7 - 0x80 } else { imm7 };
            if imm7 >= 0 {
                break;
            }
            m.clobber_push_bytes = m.clobber_push_bytes.saturating_add((-imm7 * 8) as u32);
        } else if (w & 0xffe0_0c00) == 0xf800_0c00 || (w & 0xffe0_0c00) == 0xfc00_0c00 {
            // str x/d, [sp, #imm9]! (pre-index)
            if rn != 31 {
                break;
            }
            let imm9 = ((w >> 12) & 0x1ff) as i32;
            let imm9 = if imm9 & 0x100 != 0 {
                imm9 - 0x200
            } else {
                imm9
            };
            if imm9 >= 0 {
                break;
            }
            m.clobber_push_bytes = m.clobber_push_bytes.saturating_add((-imm9) as u32);
        } else if (w & 0xff80_0000) == 0xd100_0000 && rn == 31 && (w & 31) == 31 {
            // sub sp, sp, #imm12{, lsl #12}
            let imm12 = (w >> 10) & 0xfff;
            let shift = if (w >> 22) & 1 == 1 { 12 } else { 0 };
            m.sp_sub_bytes = m.sp_sub_bytes.saturating_add(imm12 << shift);
        } else if (w & 0x7f80_001f) == 0x5280_0010 {
            // movz w16/x16, #imm16, lsl #hw
            let hw = (w >> 21) & 3;
            x16 = Some(u64::from((w >> 5) & 0xffff) << (16 * hw));
        } else if (w & 0x7f80_001f) == 0x7280_0010 {
            // movk w16/x16, #imm16, lsl #hw
            let hw = (w >> 21) & 3;
            let Some(v) = x16 else { break };
            let mask = !(0xffffu64 << (16 * hw));
            x16 = Some((v & mask) | (u64::from((w >> 5) & 0xffff) << (16 * hw)));
        } else if w == 0xcb30_63ff {
            // sub sp, sp, x16, uxtx
            let Some(v) = x16.take() else { break };
            let Ok(v) = u32::try_from(v) else { break };
            m.sp_sub_bytes = m.sp_sub_bytes.saturating_add(v);
        } else {
            break;
        }
        m.instructions += 1;
    }
    m
}

/// riscv64: `addi sp,sp,-16; sd ra,8(sp); sd fp,0(sp); mv fp,sp;
/// addi sp,sp,-n | lui/addi t6 + add sp,sp,t6; sd/fsd ...(sp)`, in the 32-bit
/// base encodings or their `C` (Zca/Zcd) forms.
///
/// `jal`/`jalr` write the return address to `ra` and push nothing, so
/// `call_push_bytes` is zero. The first SP decrement is the frame record when
/// the next instruction stores `ra` relative to SP; every later decrement
/// (clobbers, fixed slots and outgoing arguments, which Cranelift's riscv64
/// backend reserves with one adjustment) is counted in `sp_sub_bytes`.
/// Constants are tracked through `lui`/`addi`/`addiw`/`c.lui`/`c.li`/`c.addi`
/// into any integer register, so `add sp, sp, rX` and `sub sp, sp, rX` are
/// measured only when `rX` holds a known value; otherwise decoding stops and
/// the caller's layout cross-check rejects the function.
pub fn measure_riscv64(code: &[u8]) -> MeasuredPrologue {
    let mut m = MeasuredPrologue {
        call_push_bytes: 0,
        frame_record_bytes: 0,
        clobber_push_bytes: 0,
        sp_sub_bytes: 0,
        instructions: 0,
    };
    let insts = decode_riscv(code);
    let mut regs: [Option<i64>; 32] = [None; 32];
    regs[0] = Some(0);
    let mut decrements = 0u32;
    for (k, inst) in insts.iter().enumerate() {
        let dec: Option<i64> = match *inst {
            RvInst::AddImm {
                rd: SP,
                rs1: SP,
                imm,
                word: false,
            } => Some(imm),
            RvInst::AddReg {
                rd: SP,
                rs1: SP,
                rs2,
                sub,
            } => {
                let Some(v) = regs[rs2 as usize] else { break };
                Some(if sub { v.wrapping_neg() } else { v })
            }
            RvInst::AddImm {
                rd: S0,
                rs1: SP,
                imm: 0,
                word: false,
            }
            | RvInst::AddReg {
                rd: S0,
                rs1: 0,
                rs2: SP,
                sub: false,
            } => None, // mv fp, sp
            RvInst::Store { base: SP, .. } => None,
            RvInst::Lui { rd, value } if rd != SP && rd != 0 => {
                regs[rd as usize] = Some(value);
                None
            }
            RvInst::AddImm { rd, rs1, imm, word } if rd != SP && rd != 0 && rd != S0 => {
                let Some(base) = regs[rs1 as usize] else {
                    break;
                };
                let v = base.wrapping_add(imm);
                regs[rd as usize] = Some(if word { i64::from(v as i32) } else { v });
                None
            }
            _ => break,
        };
        if let Some(delta) = dec {
            if delta >= 0 {
                // SP increments belong to an epilogue, not a prologue.
                break;
            }
            let Ok(bytes) = u32::try_from(delta.unsigned_abs()) else {
                break;
            };
            let stores_ra = matches!(
                insts.get(k + 1),
                Some(RvInst::Store {
                    base: SP,
                    src: RA,
                    float: false,
                    ..
                })
            );
            if decrements == 0 && stores_ra {
                m.frame_record_bytes = m.frame_record_bytes.saturating_add(bytes);
            } else {
                m.sp_sub_bytes = m.sp_sub_bytes.saturating_add(bytes);
            }
            decrements += 1;
        }
        m.instructions += 1;
    }
    m
}

const RA: u8 = 1;
const SP: u8 = 2;
const S0: u8 = 8;

/// The riscv64 instruction forms the prologue decoder distinguishes. All
/// compressed forms are expanded to their base equivalents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RvInst {
    /// `addi`/`addiw` (and `c.addi`, `c.addi16sp`, `c.li`, `c.addiw`).
    AddImm {
        rd: u8,
        rs1: u8,
        imm: i64,
        word: bool,
    },
    /// `add`/`sub` (and `c.add`, `c.mv`, `c.sub`).
    AddReg { rd: u8, rs1: u8, rs2: u8, sub: bool },
    /// `lui` (and `c.lui`), with the sign-extended 64-bit value.
    Lui { rd: u8, value: i64 },
    /// `sd`/`sw`/`fsd`/`fsw` (and `c.sdsp`, `c.fsdsp`, `c.swsp`).
    Store {
        base: u8,
        src: u8,
        offset: i64,
        float: bool,
    },
    /// Anything else, with its length in bytes.
    Other { len: u8 },
}

fn sext(value: u32, bits: u32) -> i64 {
    let shift = 32 - bits;
    i64::from(((value << shift) as i32) >> shift)
}

/// Decodes riscv64 machine code into [`RvInst`]s until the bytes run out or
/// an incomplete instruction is reached.
pub fn decode_riscv(code: &[u8]) -> Vec<RvInst> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 2 <= code.len() {
        let half = u16::from_le_bytes([code[i], code[i + 1]]);
        if half & 3 != 3 {
            out.push(decode_rvc(half));
            i += 2;
            continue;
        }
        if half & 0x1f == 0x1f || i + 4 > code.len() {
            // 48-bit and longer encodings are not used by Cranelift.
            break;
        }
        let w = u32::from_le_bytes([code[i], code[i + 1], code[i + 2], code[i + 3]]);
        out.push(decode_rv32(w));
        i += 4;
    }
    out
}

fn decode_rv32(w: u32) -> RvInst {
    let opcode = w & 0x7f;
    let rd = ((w >> 7) & 31) as u8;
    let funct3 = (w >> 12) & 7;
    let rs1 = ((w >> 15) & 31) as u8;
    let rs2 = ((w >> 20) & 31) as u8;
    let funct7 = w >> 25;
    match (opcode, funct3) {
        (0x13, 0) | (0x1b, 0) => RvInst::AddImm {
            rd,
            rs1,
            imm: sext(w >> 20, 12),
            word: opcode == 0x1b,
        },
        (0x37, _) => RvInst::Lui {
            rd,
            value: i64::from((w & 0xffff_f000) as i32),
        },
        (0x33, 0) if funct7 == 0 || funct7 == 0x20 => RvInst::AddReg {
            rd,
            rs1,
            rs2,
            sub: funct7 == 0x20,
        },
        (0x23, 2) | (0x23, 3) | (0x27, 2) | (0x27, 3) => RvInst::Store {
            base: rs1,
            src: rs2,
            offset: sext(((w >> 25) << 5) | ((w >> 7) & 31), 12),
            float: opcode == 0x27,
        },
        _ => RvInst::Other { len: 4 },
    }
}

fn decode_rvc(h: u16) -> RvInst {
    let h = u32::from(h);
    let op = h & 3;
    let funct3 = h >> 13;
    let rd = ((h >> 7) & 31) as u8;
    let rs2 = ((h >> 2) & 31) as u8;
    let imm6 = ((h >> 7) & 0x20) | ((h >> 2) & 0x1f);
    match (op, funct3) {
        // c.addi rd, nzimm  (rd == 0 is c.nop)
        (1, 0) => RvInst::AddImm {
            rd,
            rs1: rd,
            imm: sext(imm6, 6),
            word: false,
        },
        // c.addiw rd, imm
        (1, 1) if rd != 0 => RvInst::AddImm {
            rd,
            rs1: rd,
            imm: sext(imm6, 6),
            word: true,
        },
        // c.li rd, imm
        (1, 2) => RvInst::AddImm {
            rd,
            rs1: 0,
            imm: sext(imm6, 6),
            word: false,
        },
        // c.addi16sp
        (1, 3) if rd == SP => {
            let nz = ((h >> 3) & 0x200)
                | ((h >> 2) & 0x10)
                | ((h << 1) & 0x40)
                | ((h << 4) & 0x180)
                | ((h << 3) & 0x20);
            RvInst::AddImm {
                rd: SP,
                rs1: SP,
                imm: sext(nz, 10),
                word: false,
            }
        }
        // c.lui rd, nzimm
        (1, 3) if rd != 0 => RvInst::Lui {
            rd,
            value: sext(imm6, 6) << 12,
        },
        // c.mv / c.add (c.jr / c.jalr / c.ebreak when rs2 == 0)
        (2, 4) if rs2 != 0 && rd != 0 => RvInst::AddReg {
            rd,
            rs1: if (h >> 12) & 1 == 1 { rd } else { 0 },
            rs2,
            sub: false,
        },
        // c.fsdsp / c.swsp / c.sdsp
        (2, 5) | (2, 6) | (2, 7) => {
            let offset = if funct3 == 6 {
                ((h >> 7) & 0x3c) | ((h >> 1) & 0xc0)
            } else {
                ((h >> 7) & 0x38) | ((h >> 1) & 0x1c0)
            };
            RvInst::Store {
                base: SP,
                src: rs2,
                offset: i64::from(offset),
                float: funct3 == 5,
            }
        }
        _ => RvInst::Other { len: 2 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x64_sequences() {
        // push rbp; mov rbp,rsp; sub rsp,0x20; mov [rsp+0x10],rbx; ret
        let code = [
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0x48, 0x89, 0x5c, 0x24, 0x10, 0xc3,
        ];
        let m = measure_x86_64(&code);
        assert_eq!(m.total(), Some(8 + 8 + 0x20));
        assert_eq!(m.instructions, 4);
        let big = [
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x81, 0xec, 0x00, 0x10, 0x00, 0x00, 0xc3,
        ];
        assert_eq!(measure_x86_64(&big).total(), Some(16 + 4096));
        // A negative immediate is not a Cranelift prologue: stop.
        let neg = [0x55, 0x48, 0x83, 0xec, 0xf0];
        assert_eq!(measure_x86_64(&neg).total(), Some(16));
    }

    #[test]
    fn aarch64_sequences() {
        let words: [u32; 5] = [
            0xa9bf_7bfd, // stp x29, x30, [sp,#-16]!
            0x9100_03fd, // mov x29, sp
            0xa9bf_53f3, // stp x19, x20, [sp,#-16]!
            0xd100_43ff, // sub sp, sp, #16
            0xd65f_03c0, // ret
        ];
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let m = measure_aarch64(&code);
        assert_eq!(m.frame_record_bytes, 16);
        assert_eq!(m.clobber_push_bytes, 16);
        assert_eq!(m.sp_sub_bytes, 16);
        assert_eq!(m.total(), Some(48));
        // movz x16, #0x1, lsl #16 ; sub sp, sp, x16
        let words: [u32; 2] = [0xd2a0_0030, 0xcb30_63ff];
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(measure_aarch64(&code).sp_sub_bytes, 0x1_0000);
        // Cranelift's 32-bit form: movz w16, #0x1170 ; movk w16, #1, lsl #16 ; sub sp, sp, x16
        let words: [u32; 3] = [0x5282_2e10, 0x72a0_0030, 0xcb30_63ff];
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(measure_aarch64(&code).sp_sub_bytes, 70_000);
        // sub without a known x16 value stops the prologue.
        let code = 0xcb30_63ffu32.to_le_bytes();
        assert_eq!(measure_aarch64(&code).instructions, 0);
    }

    fn rv(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn riscv64_base_sequences() {
        // addi sp,sp,-16; sd ra,8(sp); sd fp,0(sp); mv fp,sp; addi sp,sp,-32
        let code = rv(&[
            0xff01_0113,
            0x0011_3423,
            0x0081_3023,
            0x0001_0413,
            0xfe01_0113,
        ]);
        let m = measure_riscv64(&code);
        assert_eq!(m.frame_record_bytes, 16);
        assert_eq!(m.sp_sub_bytes, 32);
        assert_eq!(m.call_push_bytes, 0);
        assert_eq!(m.instructions, 5);
        // ...; lui t6,-17; addi t6,t6,-368; add sp,sp,t6  (70 000 bytes)
        let code = rv(&[
            0xff01_0113,
            0x0011_3423,
            0x0081_3023,
            0x0001_0413,
            0xfffe_ffb7,
            0xe90f_8f93,
            0x01f1_0133,
        ]);
        assert_eq!(measure_riscv64(&code).sp_sub_bytes, 70_000);
        // add sp,sp,t6 with an unknown t6 stops decoding.
        assert_eq!(measure_riscv64(&rv(&[0x01f1_0133])).instructions, 0);
        // An SP increment (epilogue) stops decoding.
        assert_eq!(measure_riscv64(&rv(&[0x0201_0113])).instructions, 0);
    }

    #[test]
    fn riscv64_compressed_sequences() {
        // c.addi16sp sp,-16; c.sdsp ra,8(sp); c.sdsp s0,0(sp); c.mv s0,sp;
        // c.addi16sp sp,-32
        let code = [0x7d, 0x71, 0x06, 0xe4, 0x22, 0xe0, 0x0a, 0x84, 0x3d, 0x71];
        let m = measure_riscv64(&code);
        assert_eq!((m.frame_record_bytes, m.sp_sub_bytes), (16, 32));
        // c.lui t6,-1; addi t6,t6,1088; c.add sp,t6  (3008 bytes)
        let code = [
            0x7d, 0x71, 0x06, 0xe4, 0x22, 0xe0, 0x0a, 0x84, 0xfd, 0x7f, 0x93, 0x8f, 0x0f, 0x44,
            0x7e, 0x91,
        ];
        assert_eq!(measure_riscv64(&code).sp_sub_bytes, 3008);
        assert_eq!(
            decode_rvc(0x6141),
            RvInst::AddImm {
                rd: SP,
                rs1: SP,
                imm: 16,
                word: false
            }
        );
    }
}
