//! Entry stubs, written as machine code with relocations.
//!
//! The stub is the only code in an image that touches control registers.
//! RISC-V (machine mode, `-bios none`):
//!
//! ```text
//! _start:   csrci mstatus, MIE        ; mask local interrupts
//!           csrw  mie, zero           ; and every source
//!           csrr  t0, mhartid ; bnez t0, park
//!           la    t0, trap_entry ; csrw mtvec, t0
//!           ld    sp, =__stack_top
//!           zero __bss_start..__bss_end
//!           call  vesper_freestanding_main
//! park:     wfi ; j park
//! trap_entry: (4-aligned, direct mode)
//!           csrr a0, mcause ; csrr a1, mepc ; csrr a2, mtval
//!           ld   sp, =__stack_top     ; a fresh stack for the handler
//!           call vesper_freestanding_trap ; j park
//! vesper_board_exit(a0 = status)      ; SiFive test finisher or park
//! ```
//!
//! AArch64 (EL1, MMU off): `msr daifset, #0xf`, secondary cores park, VBAR_EL1
//! points at a 16-entry vector table in the stub, FP/SIMD is enabled through
//! CPACR_EL1 (Cranelift may use it), then the same stack, `.bss` and call
//! sequence; `vesper_board_exit` uses semihosting `SYS_EXIT` (`hlt #0xf000`).
//!
//! The interrupt-mask immediates are derived from Silicon's interrupt model
//! ([`vesper_silicon::irq::irq_enable_bit`]), so the stub masks the bit the
//! model calls the local-interrupt enable.

use std::collections::BTreeMap;

use object::{
    self,
    write::{Object, Relocation, SectionId, Symbol, SymbolId, SymbolSection},
};

use super::link::{
    BOOT_SECTION, ENTRY_SYMBOL, R_AARCH64_ABS64, R_AARCH64_CALL26, R_RISCV_64, R_RISCV_CALL_PLT,
};
use super::runtime::{MAIN_SYMBOL, TRAP_SYMBOL};
use super::{Board, ExitDevice, FreestandingError, FreestandingTarget};

pub const EXIT_SYMBOL: &str = "vesper_board_exit";

/// Bytes of the semihosting `SYS_EXIT` parameter block on the stack.
const SEMIHOSTING_BLOCK_BYTES: u32 = 16;

/// Stack bytes `vesper_board_exit` uses (the semihosting parameter block).
pub fn exit_stack_bytes(target: FreestandingTarget, exit: ExitDevice) -> u32 {
    match (target, exit) {
        (FreestandingTarget::Aarch64, ExitDevice::Semihosting) => SEMIHOSTING_BLOCK_BYTES,
        _ => 0,
    }
}

/// DAIF bit of F, the lowest of D, A, I and F; DAIFSet bit 0.
const DAIF_F_BIT: u32 = 6;
/// DAIFSet immediate that masks D, A, I and F.
const DAIFSET_ALL: u32 = 0b1111;

/// `csrci mstatus, imm` immediate: the model's local-interrupt enable bit.
pub fn riscv_mie_mask() -> u32 {
    let (bit, set_means_enabled) =
        vesper_silicon::irq::irq_enable_bit(vesper_silicon::Arch::Riscv64);
    assert!(set_means_enabled && bit < 32);
    bit as u32
}

/// `msr daifset, #imm` immediate: D, A, I and F. The model's I bit (DAIF bit
/// 7, set = masked) is DAIFSet bit 1.
pub fn aarch64_daifset_mask() -> u32 {
    let (bit, set_means_enabled) =
        vesper_silicon::irq::irq_enable_bit(vesper_silicon::Arch::Aarch64);
    assert!(!set_means_enabled);
    let i = (bit.trailing_zeros())
        .checked_sub(DAIF_F_BIT)
        .expect("DAIF bit");
    DAIFSET_ALL | (1 << i)
}

#[derive(Clone, Copy)]
enum Fix {
    /// RISC-V B-type branch at `pos` to a label.
    RvBranch,
    /// RISC-V `jal` at `pos`.
    RvJal,
    /// RISC-V I-type immediate at `pos`, relative to the `auipc` at `base`.
    RvPcrelI { base: usize },
    /// AArch64 `imm19` at bits 5..24 (`ldr` literal, `cbnz`, `b.cond`).
    A64Imm19,
    /// AArch64 `imm26` (`b`).
    A64Imm26,
    /// AArch64 `adr`.
    A64Adr,
}

/// Alignment of the stub's 64-bit literals (and so of a RISC-V stub).
const LITERAL_ALIGN: usize = 8;

#[derive(Default)]
struct Asm {
    code: Vec<u8>,
    labels: BTreeMap<&'static str, usize>,
    fixups: Vec<(usize, &'static str, Fix)>,
    relocs: Vec<(usize, &'static str, u32)>,
}

impl Asm {
    fn here(&self) -> usize {
        self.code.len()
    }
    fn word(&mut self, w: u32) -> usize {
        let at = self.here();
        self.code.extend_from_slice(&w.to_le_bytes());
        at
    }
    fn label(&mut self, name: &'static str) {
        let at = self.here();
        assert!(
            self.labels.insert(name, at).is_none(),
            "duplicate label {name}"
        );
    }
    fn align(&mut self, a: usize, fill: u32) {
        while !self.here().is_multiple_of(a) {
            if a >= 4 && self.here().is_multiple_of(4) {
                self.word(fill);
            } else {
                self.code.push(0);
            }
        }
    }
    fn fix(&mut self, at: usize, label: &'static str, kind: Fix) {
        self.fixups.push((at, label, kind));
    }
    fn literal(&mut self, label: &'static str, symbol: &'static str, r_type: u32) {
        self.align(LITERAL_ALIGN, 0);
        self.label(label);
        let at = self.here();
        self.code.extend_from_slice(&0u64.to_le_bytes());
        self.relocs.push((at, symbol, r_type));
    }
    fn literal_value(&mut self, label: &'static str, value: u64) {
        self.align(LITERAL_ALIGN, 0);
        self.label(label);
        self.code.extend_from_slice(&value.to_le_bytes());
    }
    fn resolve(&mut self) {
        for (at, label, kind) in self.fixups.clone() {
            let target = self.labels[label] as i64;
            let mut w = u32::from_le_bytes(self.code[at..at + 4].try_into().unwrap());
            let off = target - at as i64;
            match kind {
                Fix::RvBranch => {
                    assert!((-RV_BRANCH_RANGE..RV_BRANCH_RANGE).contains(&off) && off % 2 == 0);
                    let o = off as u32;
                    w |= ((o >> 12) & 1) << 31
                        | ((o >> 5) & 0x3f) << 25
                        | ((o >> 1) & 0xf) << 8
                        | ((o >> 11) & 1) << 7;
                }
                Fix::RvJal => {
                    assert!((-RV_JAL_RANGE..RV_JAL_RANGE).contains(&off) && off % 2 == 0);
                    let o = off as u32;
                    w |= ((o >> 20) & 1) << 31
                        | ((o >> 1) & 0x3ff) << 21
                        | ((o >> 11) & 1) << 20
                        | ((o >> 12) & 0xff) << 12;
                }
                Fix::RvPcrelI { base } => {
                    let off = target - base as i64;
                    assert!((-RV_IMM12_RANGE..RV_IMM12_RANGE).contains(&off));
                    w |= ((off as u32) & RV_IMM12_MASK) << RV_I_IMM_SHIFT;
                }
                Fix::A64Imm19 => {
                    assert!(off % 4 == 0 && (-A64_IMM19_RANGE..A64_IMM19_RANGE).contains(&off));
                    w |= (((off >> 2) as u32) & A64_IMM19_MASK) << A64_IMM19_SHIFT;
                }
                Fix::A64Imm26 => {
                    assert!(off % 4 == 0);
                    w |= ((off >> 2) as u32) & A64_IMM26_MASK;
                }
                Fix::A64Adr => {
                    assert!((-A64_ADR_RANGE..A64_ADR_RANGE).contains(&off));
                    let o = off as u32;
                    w |= (o & A64_ADR_IMMLO_MASK) << A64_ADR_IMMLO_SHIFT
                        | ((o >> 2) & A64_IMM19_MASK) << A64_IMM19_SHIFT;
                }
            }
            self.code[at..at + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
}

// --- RISC-V encodings -------------------------------------------------------

const ZERO: u32 = 0;
const RA: u32 = 1;
const SP: u32 = 2;
const T0: u32 = 5;
const T1: u32 = 6;
const T2: u32 = 7;
const A0: u32 = 10;
const A1: u32 = 11;
const A2: u32 = 12;

const CSR_MSTATUS: u32 = 0x300;
const CSR_MIE: u32 = 0x304;
const CSR_MTVEC: u32 = 0x305;
const CSR_MEPC: u32 = 0x341;
const CSR_MCAUSE: u32 = 0x342;
const CSR_MTVAL: u32 = 0x343;
const CSR_MHARTID: u32 = 0xf14;

/// Base opcodes (`inst[6:0]`).
pub(super) const RV_OPCODE_MASK: u32 = 0x7f;
const RV_OPCODE_LOAD: u32 = 0x03;
const RV_OPCODE_OP_IMM: u32 = 0x13;
pub(super) const RV_OPCODE_AUIPC: u32 = 0x17;
const RV_OPCODE_STORE: u32 = 0x23;
const RV_OPCODE_OP: u32 = 0x33;
const RV_OPCODE_LUI: u32 = 0x37;
const RV_OPCODE_BRANCH: u32 = 0x63;
pub(super) const RV_OPCODE_JALR: u32 = 0x67;
const RV_OPCODE_JAL: u32 = 0x6f;
const RV_OPCODE_SYSTEM: u32 = 0x73;

/// `funct3` of the instructions the stub emits.
const RV_FUNCT3_ADDI: u32 = 0;
const RV_FUNCT3_SLLI: u32 = 1;
const RV_FUNCT3_OR: u32 = 6;
const RV_FUNCT3_LD: u32 = 3;
const RV_FUNCT3_SW: u32 = 2;
const RV_FUNCT3_SD: u32 = 3;
const RV_FUNCT3_BNE: u32 = 1;
const RV_FUNCT3_BGEU: u32 = 7;
const RV_FUNCT3_JALR: u32 = 0;
const RV_FUNCT3_CSRRW: u32 = 1;
const RV_FUNCT3_CSRRS: u32 = 2;
const RV_FUNCT3_CSRRCI: u32 = 7;

/// Bit positions of the register and `funct3` fields.
const RV_RD_SHIFT: u32 = 7;
const RV_FUNCT3_SHIFT: u32 = 12;
const RV_RS1_SHIFT: u32 = 15;
const RV_RS2_SHIFT: u32 = 20;
/// Bit position of the I-type immediate (and of a CSR number).
pub(super) const RV_I_IMM_SHIFT: u32 = 20;
/// Bit position of the U-type (`lui`, `auipc`) immediate.
pub(super) const RV_U_IMM_SHIFT: u32 = 12;
pub(super) const RV_IMM12_MASK: u32 = 0xfff;
pub(super) const RV_IMM20_MASK: u32 = 0xfffff;

/// Reach (exclusive, either way) of an I-type offset, a B-type branch and
/// a `jal`.
const RV_IMM12_RANGE: i64 = 1 << 11;
const RV_BRANCH_RANGE: i64 = 1 << 12;
const RV_JAL_RANGE: i64 = 1 << 20;

/// `mtvec` in direct mode needs a 4-byte aligned trap entry.
const RV_MTVEC_ALIGN: usize = 4;

fn rv_i(op: u32, f3: u32, rd: u32, rs1: u32, imm: i32) -> u32 {
    ((imm as u32) & RV_IMM12_MASK) << RV_I_IMM_SHIFT
        | rs1 << RV_RS1_SHIFT
        | f3 << RV_FUNCT3_SHIFT
        | rd << RV_RD_SHIFT
        | op
}
fn rv_csr(f3: u32, rd: u32, rs1_or_uimm: u32, csr: u32) -> u32 {
    csr << RV_I_IMM_SHIFT
        | rs1_or_uimm << RV_RS1_SHIFT
        | f3 << RV_FUNCT3_SHIFT
        | rd << RV_RD_SHIFT
        | RV_OPCODE_SYSTEM
}
fn rv_s(f3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
    let i = imm as u32;
    ((i >> 5) & 0x7f) << 25
        | rs2 << RV_RS2_SHIFT
        | rs1 << RV_RS1_SHIFT
        | f3 << RV_FUNCT3_SHIFT
        | (i & 0x1f) << 7
        | RV_OPCODE_STORE
}
fn rv_b(f3: u32, rs1: u32, rs2: u32) -> u32 {
    rs2 << RV_RS2_SHIFT | rs1 << RV_RS1_SHIFT | f3 << RV_FUNCT3_SHIFT | RV_OPCODE_BRANCH
}
fn rv_lui(rd: u32, imm20: u32) -> u32 {
    (imm20 & RV_IMM20_MASK) << RV_U_IMM_SHIFT | rd << RV_RD_SHIFT | RV_OPCODE_LUI
}
fn rv_auipc(rd: u32) -> u32 {
    rd << RV_RD_SHIFT | RV_OPCODE_AUIPC
}
const RV_WFI: u32 = 0x1050_0073;

impl Asm {
    fn rv_la(&mut self, rd: u32, label: &'static str) {
        let base = self.word(rv_auipc(rd));
        let at = self.word(rv_i(RV_OPCODE_OP_IMM, RV_FUNCT3_ADDI, rd, rd, 0));
        self.fix(at, label, Fix::RvPcrelI { base });
    }
    fn rv_ld_lit(&mut self, rd: u32, label: &'static str) {
        let base = self.word(rv_auipc(rd));
        let at = self.word(rv_i(RV_OPCODE_LOAD, RV_FUNCT3_LD, rd, rd, 0));
        self.fix(at, label, Fix::RvPcrelI { base });
    }
    fn rv_call(&mut self, symbol: &'static str) {
        let at = self.word(rv_auipc(RA));
        self.word(rv_i(RV_OPCODE_JALR, RV_FUNCT3_JALR, RA, RA, 0));
        self.relocs.push((at, symbol, R_RISCV_CALL_PLT));
    }
    fn rv_branch(&mut self, f3: u32, rs1: u32, rs2: u32, label: &'static str) {
        let at = self.word(rv_b(f3, rs1, rs2));
        self.fix(at, label, Fix::RvBranch);
    }
    fn rv_j(&mut self, label: &'static str) {
        let at = self.word(RV_OPCODE_JAL);
        self.fix(at, label, Fix::RvJal);
    }
}

fn riscv(board: &Board) -> Result<Asm, FreestandingError> {
    let mut a = Asm::default();
    a.label(ENTRY_SYMBOL);
    // csrci mstatus, MIE
    a.word(rv_csr(
        RV_FUNCT3_CSRRCI,
        ZERO,
        riscv_mie_mask(),
        CSR_MSTATUS,
    ));
    a.word(rv_csr(RV_FUNCT3_CSRRW, ZERO, ZERO, CSR_MIE)); // csrw mie, zero
    a.word(rv_csr(RV_FUNCT3_CSRRS, T0, ZERO, CSR_MHARTID)); // csrr t0, mhartid
    a.rv_branch(RV_FUNCT3_BNE, T0, ZERO, "park"); // bnez t0, park
    a.rv_la(T0, "trap_entry");
    a.word(rv_csr(RV_FUNCT3_CSRRW, ZERO, T0, CSR_MTVEC)); // csrw mtvec, t0
    a.rv_ld_lit(SP, "lit_stack_top");
    a.rv_ld_lit(T0, "lit_bss_start");
    a.rv_ld_lit(T1, "lit_bss_end");
    a.label("bss_loop");
    a.rv_branch(RV_FUNCT3_BGEU, T0, T1, "bss_done"); // bgeu t0, t1
    a.word(rv_s(RV_FUNCT3_SD, T0, ZERO, 0)); // sd zero, 0(t0)
    a.word(rv_i(RV_OPCODE_OP_IMM, RV_FUNCT3_ADDI, T0, T0, 8)); // addi t0, t0, 8
    a.rv_j("bss_loop");
    a.label("bss_done");
    a.rv_call(MAIN_SYMBOL);
    a.label("park");
    a.word(RV_WFI);
    a.rv_j("park");
    a.align(RV_MTVEC_ALIGN, 0);
    a.label("trap_entry");
    a.word(rv_csr(RV_FUNCT3_CSRRS, A0, ZERO, CSR_MCAUSE));
    a.word(rv_csr(RV_FUNCT3_CSRRS, A1, ZERO, CSR_MEPC));
    a.word(rv_csr(RV_FUNCT3_CSRRS, A2, ZERO, CSR_MTVAL));
    a.rv_ld_lit(SP, "lit_stack_top");
    a.rv_call(TRAP_SYMBOL);
    a.rv_j("park");
    a.label(EXIT_SYMBOL);
    match board.exit {
        ExitDevice::SifiveTest { .. } => {
            a.rv_ld_lit(T0, "lit_exit");
            a.rv_branch(RV_FUNCT3_BNE, A0, ZERO, "exit_fail");
            a.word(rv_lui(T1, 0x5));
            a.word(rv_i(RV_OPCODE_OP_IMM, RV_FUNCT3_ADDI, T1, T1, 0x555)); // t1 = 0x5555
            a.word(rv_s(RV_FUNCT3_SW, T0, T1, 0)); // sw t1, 0(t0)
            a.rv_j("park");
            a.label("exit_fail");
            a.word(rv_i(RV_OPCODE_OP_IMM, RV_FUNCT3_SLLI, T1, A0, 16)); // slli t1, a0, 16
            a.word(rv_lui(T2, 0x3));
            a.word(rv_i(RV_OPCODE_OP_IMM, RV_FUNCT3_ADDI, T2, T2, 0x333)); // t2 = 0x3333
            // or t1, t1, t2
            a.word(
                T2 << RV_RS2_SHIFT
                    | T1 << RV_RS1_SHIFT
                    | RV_FUNCT3_OR << RV_FUNCT3_SHIFT
                    | T1 << RV_RD_SHIFT
                    | RV_OPCODE_OP,
            );
            a.word(rv_s(RV_FUNCT3_SW, T0, T1, 0));
            a.rv_j("park");
        }
        ExitDevice::Park => a.rv_j("park"),
        ExitDevice::Semihosting => {
            return Err(FreestandingError::UnsupportedTarget(
                "semihosting exit on riscv64 (use the SiFive test device)".into(),
            ));
        }
    }
    a.literal("lit_stack_top", "__stack_top", R_RISCV_64);
    a.literal("lit_bss_start", "__bss_start", R_RISCV_64);
    a.literal("lit_bss_end", "__bss_end", R_RISCV_64);
    if let ExitDevice::SifiveTest { base } = board.exit {
        a.literal_value("lit_exit", base);
    }
    let _ = (A1, A2);
    Ok(a)
}

// --- AArch64 encodings ------------------------------------------------------

const A64_ISB: u32 = 0xd503_3fdf;
const A64_WFE: u32 = 0xd503_205f;

/// Instruction words whose operand fields the stub or a fixup fills in.
/// `ldr xt, label` (literal).
const A64_LDR_LIT_X: u32 = 0x5800_0000;
const A64_B: u32 = 0x1400_0000;
const A64_BL: u32 = 0x9400_0000;
/// `cbnz x0, label`.
const A64_CBNZ_X: u32 = 0xb500_0000;
/// `adr x0, label`.
const A64_ADR: u32 = 0x1000_0000;
/// `b.hs label`.
const A64_B_HS: u32 = 0x5400_0002;
/// `msr daifset, #imm`, with the immediate in `CRm`.
const A64_MSR_DAIFSET: u32 = 0xd503_40df;
const A64_CRM_SHIFT: u32 = 8;

/// `imm19` field (`ldr` literal, `cbnz`, `b.cond`, and `adr`'s `immhi`).
const A64_IMM19_SHIFT: u32 = 5;
const A64_IMM19_MASK: u32 = 0x7ffff;
/// `imm26` field of `b` and `bl`.
pub(super) const A64_IMM26_MASK: u32 = 0x03ff_ffff;
/// `immlo` field of `adr`.
const A64_ADR_IMMLO_SHIFT: u32 = 29;
const A64_ADR_IMMLO_MASK: u32 = 3;
/// Reach (exclusive, either way) of an `imm19` offset and of `adr`.
const A64_IMM19_RANGE: i64 = 1 << 20;
const A64_ADR_RANGE: i64 = 1 << 20;

/// `VBAR_EL1` alignment, and the 16 entries of 0x80 bytes in the table.
const A64_VECTOR_TABLE_ALIGN: usize = 0x800;
const A64_VECTOR_ENTRIES: usize = 16;
const A64_VECTOR_ENTRY_BYTES: usize = 0x80;

impl Asm {
    fn a64_ldr_lit(&mut self, rt: u32, label: &'static str) {
        let at = self.word(A64_LDR_LIT_X | rt);
        self.fix(at, label, Fix::A64Imm19);
    }
    fn a64_b(&mut self, label: &'static str) {
        let at = self.word(A64_B);
        self.fix(at, label, Fix::A64Imm26);
    }
    fn a64_bl(&mut self, symbol: &'static str) {
        let at = self.word(A64_BL);
        self.relocs.push((at, symbol, R_AARCH64_CALL26));
    }
}

fn aarch64(board: &Board) -> Result<Asm, FreestandingError> {
    let mut a = Asm::default();
    a.label(ENTRY_SYMBOL);
    a.word(A64_MSR_DAIFSET | aarch64_daifset_mask() << A64_CRM_SHIFT); // msr daifset, #mask
    a.word(0xd538_00a0); // mrs x0, mpidr_el1
    a.word(0x9240_1c00); // and x0, x0, #0xff
    let at = a.word(A64_CBNZ_X); // cbnz x0, park
    a.fix(at, "park", Fix::A64Imm19);
    let at = a.word(A64_ADR); // adr x0, vectors
    a.fix(at, "vectors", Fix::A64Adr);
    a.word(0xd518_c000); // msr vbar_el1, x0
    a.word(0xd2a0_0600); // movz x0, #0x30, lsl #16 (CPACR_EL1.FPEN = 0b11)
    a.word(0xd518_1040); // msr cpacr_el1, x0
    a.word(A64_ISB);
    a.a64_ldr_lit(0, "lit_stack_top");
    a.word(0x9100_001f); // mov sp, x0
    a.a64_ldr_lit(0, "lit_bss_start");
    a.a64_ldr_lit(1, "lit_bss_end");
    a.label("bss_loop");
    a.word(0xeb01_001f); // cmp x0, x1
    let at = a.word(A64_B_HS); // b.hs bss_done
    a.fix(at, "bss_done", Fix::A64Imm19);
    a.word(0xf800_841f); // str xzr, [x0], #8
    a.a64_b("bss_loop");
    a.label("bss_done");
    a.a64_bl(MAIN_SYMBOL);
    a.label("park");
    a.word(A64_WFE);
    a.a64_b("park");
    a.label(EXIT_SYMBOL);
    match board.exit {
        ExitDevice::Semihosting => {
            a.word(0xd100_43ff); // sub sp, sp, #16
            a.word(0xd280_04c2); // movz x2, #0x26
            a.word(0xf2a0_0042); // movk x2, #0x2, lsl #16 (ADP_Stopped_ApplicationExit)
            a.word(0xa900_03e2); // stp x2, x0, [sp]
            a.word(0x9100_03e1); // mov x1, sp
            a.word(0x5280_0300); // mov w0, #0x18 (SYS_EXIT)
            a.word(0xd45e_0000); // hlt #0xf000
            a.a64_b("park");
        }
        ExitDevice::SifiveTest { .. } => {
            // w2 = status == 0 ? 0x5555 : status << 16 | 0x3333
            a.a64_ldr_lit(1, "lit_exit");
            let at = a.word(A64_CBNZ_X); // cbnz x0, exit_fail
            a.fix(at, "exit_fail", Fix::A64Imm19);
            a.word(0x528a_aaa2); // mov w2, #0x5555
            a.word(0xb900_0022); // str w2, [x1]
            a.a64_b("park");
            a.label("exit_fail");
            a.word(0x5286_6662); // mov w2, #0x3333
            a.word(0x2a00_4042); // orr w2, w2, w0, lsl #16
            a.word(0xb900_0022); // str w2, [x1]
            a.a64_b("park");
        }
        ExitDevice::Park => a.a64_b("park"),
    }
    a.literal("lit_stack_top", "__stack_top", R_AARCH64_ABS64);
    a.literal("lit_bss_start", "__bss_start", R_AARCH64_ABS64);
    a.literal("lit_bss_end", "__bss_end", R_AARCH64_ABS64);
    if let ExitDevice::SifiveTest { base } = board.exit {
        a.literal_value("lit_exit", base);
    }
    // VBAR_EL1 needs 2 KiB alignment; every entry is 0x80 bytes.
    a.align(A64_VECTOR_TABLE_ALIGN, A64_WFE);
    a.label("vectors");
    for i in 0..A64_VECTOR_ENTRIES {
        let start = a.here();
        debug_assert_eq!(start, a.labels["vectors"] + i * A64_VECTOR_ENTRY_BYTES);
        a.word(0xd538_5200); // mrs x0, esr_el1
        a.word(0xd538_4021); // mrs x1, elr_el1
        a.word(0xd538_6002); // mrs x2, far_el1
        a.a64_ldr_lit(3, "lit_stack_top");
        a.word(0x9100_007f); // mov sp, x3
        a.a64_bl(TRAP_SYMBOL);
        a.a64_b("park");
        while a.here() < start + A64_VECTOR_ENTRY_BYTES {
            a.word(A64_WFE);
        }
    }
    Ok(a)
}

/// The stub as a relocatable ELF object with `e_flags` matching the
/// backend's object (the RISC-V float ABI and RVC flags must agree for a
/// GNU-compatible linker).
pub fn build(
    target: FreestandingTarget,
    board: &Board,
    e_flags: u32,
) -> Result<Vec<u8>, FreestandingError> {
    let mut asm = match target {
        FreestandingTarget::Riscv64Imac => riscv(board)?,
        FreestandingTarget::Aarch64 => aarch64(board)?,
    };
    asm.resolve();
    let arch = match target {
        FreestandingTarget::Riscv64Imac => object::Architecture::Riscv64,
        FreestandingTarget::Aarch64 => object::Architecture::Aarch64,
    };
    let mut obj = Object::new(object::BinaryFormat::Elf, arch, object::Endianness::Little);
    obj.flags = object::FileFlags::Elf {
        os_abi: 0,
        abi_version: 0,
        e_flags,
    };
    let section: SectionId = obj.add_section(
        Vec::new(),
        BOOT_SECTION.as_bytes().to_vec(),
        object::SectionKind::Text,
    );
    let align = if target == FreestandingTarget::Aarch64 {
        A64_VECTOR_TABLE_ALIGN
    } else {
        LITERAL_ALIGN
    };
    obj.set_section_data(section, asm.code.clone(), align as u64);
    for (name, size) in [(ENTRY_SYMBOL, 0u64), (EXIT_SYMBOL, 0)] {
        obj.add_symbol(Symbol {
            name: name.as_bytes().to_vec(),
            value: asm.labels[name] as u64,
            size,
            kind: object::SymbolKind::Text,
            scope: object::SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(section),
            flags: object::SymbolFlags::None,
        });
    }
    let mut externs: BTreeMap<&str, SymbolId> = BTreeMap::new();
    for (at, symbol, r_type) in &asm.relocs {
        let id = *externs.entry(symbol).or_insert_with(|| {
            obj.add_symbol(Symbol {
                name: symbol.as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: object::SymbolKind::Unknown,
                scope: object::SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Undefined,
                flags: object::SymbolFlags::None,
            })
        });
        obj.add_relocation(
            section,
            Relocation {
                offset: *at as u64,
                symbol: id,
                addend: 0,
                flags: object::RelocationFlags::Elf { r_type: *r_type },
            },
        )
        .map_err(|e| FreestandingError::Object(e.to_string()))?;
    }
    obj.write()
        .map_err(|e| FreestandingError::Object(e.to_string()))
}
