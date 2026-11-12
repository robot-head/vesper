//! Stack-pointer accounting read from the machine code Cranelift emitted.
//!
//! The decoder recognises the prologue sequences Cranelift 0.125 emits on
//! x86_64 and AArch64 and sums every stack-pointer decrement up to the first
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
    for word in code.chunks_exact(4) {
        let w = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
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
}
