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

use super::link::{R_AARCH64_ABS64, R_AARCH64_CALL26, R_RISCV_64, R_RISCV_CALL_PLT};
use super::runtime::{MAIN_SYMBOL, TRAP_SYMBOL};
use super::{Board, ExitDevice, FreestandingError, FreestandingTarget};

pub const EXIT_SYMBOL: &str = "vesper_board_exit";

/// Stack bytes `vesper_board_exit` uses (the semihosting parameter block).
pub fn exit_stack_bytes(target: FreestandingTarget, exit: ExitDevice) -> u32 {
    match (target, exit) {
        (FreestandingTarget::Aarch64, ExitDevice::Semihosting) => 16,
        _ => 0,
    }
}

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
    let i = (bit.trailing_zeros()).checked_sub(6).expect("DAIF bit");
    0b1111 | (1 << i)
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
        self.align(8, 0);
        self.label(label);
        let at = self.here();
        self.code.extend_from_slice(&0u64.to_le_bytes());
        self.relocs.push((at, symbol, r_type));
    }
    fn literal_value(&mut self, label: &'static str, value: u64) {
        self.align(8, 0);
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
                    assert!((-4096..4096).contains(&off) && off % 2 == 0);
                    let o = off as u32;
                    w |= ((o >> 12) & 1) << 31
                        | ((o >> 5) & 0x3f) << 25
                        | ((o >> 1) & 0xf) << 8
                        | ((o >> 11) & 1) << 7;
                }
                Fix::RvJal => {
                    assert!((-(1 << 20)..(1 << 20)).contains(&off) && off % 2 == 0);
                    let o = off as u32;
                    w |= ((o >> 20) & 1) << 31
                        | ((o >> 1) & 0x3ff) << 21
                        | ((o >> 11) & 1) << 20
                        | ((o >> 12) & 0xff) << 12;
                }
                Fix::RvPcrelI { base } => {
                    let off = target - base as i64;
                    assert!((-2048..2048).contains(&off));
                    w |= ((off as u32) & 0xfff) << 20;
                }
                Fix::A64Imm19 => {
                    assert!(off % 4 == 0 && (-(1 << 20)..(1 << 20)).contains(&off));
                    w |= (((off >> 2) as u32) & 0x7ffff) << 5;
                }
                Fix::A64Imm26 => {
                    assert!(off % 4 == 0);
                    w |= ((off >> 2) as u32) & 0x03ff_ffff;
                }
                Fix::A64Adr => {
                    assert!((-(1 << 20)..(1 << 20)).contains(&off));
                    let o = off as u32;
                    w |= (o & 3) << 29 | ((o >> 2) & 0x7ffff) << 5;
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

fn rv_i(op: u32, f3: u32, rd: u32, rs1: u32, imm: i32) -> u32 {
    ((imm as u32) & 0xfff) << 20 | rs1 << 15 | f3 << 12 | rd << 7 | op
}
fn rv_csr(f3: u32, rd: u32, rs1_or_uimm: u32, csr: u32) -> u32 {
    csr << 20 | rs1_or_uimm << 15 | f3 << 12 | rd << 7 | 0x73
}
fn rv_s(f3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
    let i = imm as u32;
    ((i >> 5) & 0x7f) << 25 | rs2 << 20 | rs1 << 15 | f3 << 12 | (i & 0x1f) << 7 | 0x23
}
fn rv_b(f3: u32, rs1: u32, rs2: u32) -> u32 {
    rs2 << 20 | rs1 << 15 | f3 << 12 | 0x63
}
fn rv_lui(rd: u32, imm20: u32) -> u32 {
    (imm20 & 0xfffff) << 12 | rd << 7 | 0x37
}
fn rv_auipc(rd: u32) -> u32 {
    rd << 7 | 0x17
}
const RV_WFI: u32 = 0x1050_0073;

impl Asm {
    fn rv_la(&mut self, rd: u32, label: &'static str) {
        let base = self.word(rv_auipc(rd));
        let at = self.word(rv_i(0x13, 0, rd, rd, 0));
        self.fix(at, label, Fix::RvPcrelI { base });
    }
    fn rv_ld_lit(&mut self, rd: u32, label: &'static str) {
        let base = self.word(rv_auipc(rd));
        let at = self.word(rv_i(0x03, 3, rd, rd, 0));
        self.fix(at, label, Fix::RvPcrelI { base });
    }
    fn rv_call(&mut self, symbol: &'static str) {
        let at = self.word(rv_auipc(RA));
        self.word(rv_i(0x67, 0, RA, RA, 0));
        self.relocs.push((at, symbol, R_RISCV_CALL_PLT));
    }
    fn rv_branch(&mut self, f3: u32, rs1: u32, rs2: u32, label: &'static str) {
        let at = self.word(rv_b(f3, rs1, rs2));
        self.fix(at, label, Fix::RvBranch);
    }
    fn rv_j(&mut self, label: &'static str) {
        let at = self.word(0x6f);
        self.fix(at, label, Fix::RvJal);
    }
}

fn riscv(board: &Board) -> Result<Asm, FreestandingError> {
    let mut a = Asm::default();
    a.label("_start");
    a.word(rv_csr(7, ZERO, riscv_mie_mask(), CSR_MSTATUS)); // csrci mstatus, MIE
    a.word(rv_csr(1, ZERO, ZERO, CSR_MIE)); // csrw mie, zero
    a.word(rv_csr(2, T0, ZERO, CSR_MHARTID)); // csrr t0, mhartid
    a.rv_branch(1, T0, ZERO, "park"); // bnez t0, park
    a.rv_la(T0, "trap_entry");
    a.word(rv_csr(1, ZERO, T0, CSR_MTVEC)); // csrw mtvec, t0
    a.rv_ld_lit(SP, "lit_stack_top");
    a.rv_ld_lit(T0, "lit_bss_start");
    a.rv_ld_lit(T1, "lit_bss_end");
    a.label("bss_loop");
    a.rv_branch(7, T0, T1, "bss_done"); // bgeu t0, t1
    a.word(rv_s(3, T0, ZERO, 0)); // sd zero, 0(t0)
    a.word(rv_i(0x13, 0, T0, T0, 8)); // addi t0, t0, 8
    a.rv_j("bss_loop");
    a.label("bss_done");
    a.rv_call(MAIN_SYMBOL);
    a.label("park");
    a.word(RV_WFI);
    a.rv_j("park");
    a.align(4, 0);
    a.label("trap_entry");
    a.word(rv_csr(2, A0, ZERO, CSR_MCAUSE));
    a.word(rv_csr(2, A1, ZERO, CSR_MEPC));
    a.word(rv_csr(2, A2, ZERO, CSR_MTVAL));
    a.rv_ld_lit(SP, "lit_stack_top");
    a.rv_call(TRAP_SYMBOL);
    a.rv_j("park");
    a.label(EXIT_SYMBOL);
    match board.exit {
        ExitDevice::SifiveTest { .. } => {
            a.rv_ld_lit(T0, "lit_exit");
            a.rv_branch(1, A0, ZERO, "exit_fail");
            a.word(rv_lui(T1, 0x5));
            a.word(rv_i(0x13, 0, T1, T1, 0x555)); // t1 = 0x5555
            a.word(rv_s(2, T0, T1, 0)); // sw t1, 0(t0)
            a.rv_j("park");
            a.label("exit_fail");
            a.word(rv_i(0x13, 1, T1, A0, 16)); // slli t1, a0, 16
            a.word(rv_lui(T2, 0x3));
            a.word(rv_i(0x13, 0, T2, T2, 0x333)); // t2 = 0x3333
            a.word(T2 << 20 | T1 << 15 | 6 << 12 | T1 << 7 | 0x33); // or t1, t1, t2
            a.word(rv_s(2, T0, T1, 0));
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

impl Asm {
    fn a64_ldr_lit(&mut self, rt: u32, label: &'static str) {
        let at = self.word(0x5800_0000 | rt);
        self.fix(at, label, Fix::A64Imm19);
    }
    fn a64_b(&mut self, label: &'static str) {
        let at = self.word(0x1400_0000);
        self.fix(at, label, Fix::A64Imm26);
    }
    fn a64_bl(&mut self, symbol: &'static str) {
        let at = self.word(0x9400_0000);
        self.relocs.push((at, symbol, R_AARCH64_CALL26));
    }
}

fn aarch64(board: &Board) -> Result<Asm, FreestandingError> {
    let mut a = Asm::default();
    a.label("_start");
    a.word(0xd503_40df | aarch64_daifset_mask() << 8); // msr daifset, #mask
    a.word(0xd538_00a0); // mrs x0, mpidr_el1
    a.word(0x9240_1c00); // and x0, x0, #0xff
    let at = a.word(0xb500_0000); // cbnz x0, park
    a.fix(at, "park", Fix::A64Imm19);
    let at = a.word(0x1000_0000); // adr x0, vectors
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
    let at = a.word(0x5400_0002); // b.hs bss_done
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
            let at = a.word(0xb500_0000); // cbnz x0, exit_fail
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
    a.align(0x800, A64_WFE);
    a.label("vectors");
    for i in 0..16 {
        let start = a.here();
        debug_assert_eq!(start, a.labels["vectors"] + i * 0x80);
        a.word(0xd538_5200); // mrs x0, esr_el1
        a.word(0xd538_4021); // mrs x1, elr_el1
        a.word(0xd538_6002); // mrs x2, far_el1
        a.a64_ldr_lit(3, "lit_stack_top");
        a.word(0x9100_007f); // mov sp, x3
        a.a64_bl(TRAP_SYMBOL);
        a.a64_b("park");
        while a.here() < start + 0x80 {
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
        b".text.boot".to_vec(),
        object::SectionKind::Text,
    );
    let align = if target == FreestandingTarget::Aarch64 {
        0x800
    } else {
        8
    };
    obj.set_section_data(section, asm.code.clone(), align);
    for (name, size) in [("_start", 0u64), (EXIT_SYMBOL, 0)] {
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
