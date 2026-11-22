//! CPU instructions that Cranelift has no instruction for (audit gap G4):
//! the functions of `@vesper-std/cpu`'s `.vsys` library `StdCpu`
//! (packages/vesper-std/cpu/cpu.vsys). No library provides their symbols.
//! The native backend defines each `vstd_cpu_*` function it calls as a
//! local leaf function from the bytes below, so the JIT, objects and
//! executables need no host shim. A function of the other architecture is
//! a compile error.
//!
//! The x86_64 functions use the System V calling convention on every
//! operating system (argument in `edi`/`rdi`, result in `rax`), and the
//! call site declares them with it. The AArch64 functions take `w0`/`x0`
//! and return `x0`, which AAPCS64 and Apple's variant agree on.

/// The prefix of the symbols this module defines.
pub const PREFIX: &str = "vstd_cpu_";
/// The link name `cpu.vsys` declares. Nothing is linked for it: an
/// executable's link line leaves it out.
pub const LINK_NAME: &str = "vesper_std_cpu";

/// The architecture a function's code is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

/// `mov r8, rdx; mov r9, rbx; mov eax, edi; mov ecx, esi; cpuid;
/// mov [r8], eax; mov [r8+4], ebx; mov [r8+8], ecx; mov [r8+12], edx;
/// mov rbx, r9; ret`. `cpuid` clobbers the callee-saved `rbx`, which is
/// kept in the volatile `r9`, so the function stays frameless.
const CPUID: [u8; 31] = [
    0x49, 0x89, 0xd0, 0x49, 0x89, 0xd9, 0x89, 0xf8, 0x89, 0xf1, 0x0f, 0xa2, 0x41, 0x89, 0x00, 0x41,
    0x89, 0x58, 0x04, 0x41, 0x89, 0x48, 0x08, 0x41, 0x89, 0x50, 0x0c, 0x4c, 0x89, 0xcb, 0xc3,
];
/// `lfence; rdtsc; shl rdx, 32; or rax, rdx; ret`.
const RDTSC: [u8; 13] = [
    0x0f, 0xae, 0xe8, 0x0f, 0x31, 0x48, 0xc1, 0xe2, 0x20, 0x48, 0x09, 0xd0, 0xc3,
];
/// `mov ecx, edi; lfence; rdpmc; lfence; shl rdx, 32; or rax, rdx; ret`.
const RDPMC: [u8; 18] = [
    0x89, 0xf9, 0x0f, 0xae, 0xe8, 0x0f, 0x33, 0x0f, 0xae, 0xe8, 0x48, 0xc1, 0xe2, 0x20, 0x48, 0x09,
    0xd0, 0xc3,
];
/// `rdrand rax; setc cl; mov [rdi], rax; movzx eax, cl; ret`.
const RDRAND64: [u8; 14] = [
    0x48, 0x0f, 0xc7, 0xf0, 0x0f, 0x92, 0xc1, 0x48, 0x89, 0x07, 0x0f, 0xb6, 0xc1, 0xc3,
];

const ISB: u32 = 0xd503_3fdf;
const RET: u32 = 0xd65f_03c0;
/// `mrs x0, cntvct_el0`.
const MRS_CNTVCT: u32 = 0xd53b_e040;
/// `mrs x0, pmccntr_el0`.
const MRS_PMCCNTR: u32 = 0xd53b_9d00;
/// `mov w0, w0` (zero the upper word of the index).
const MOV_W0_W0: u32 = 0x2a00_03e0;
/// `msr pmselr_el0, x0`.
const MSR_PMSELR: u32 = 0xd51b_9ca0;
/// `mrs x0, pmxevcntr_el0`.
const MRS_PMXEVCNTR: u32 = 0xd53b_9d40;
/// `mrs x1, rndr` (NZCV is 0b0100 when no word was ready).
const MRS_RNDR_X1: u32 = 0xd53b_2401;
/// `str x1, [x0]`.
const STR_X1_X0: u32 = 0xf900_0001;
/// `cset w0, ne`.
const CSET_W0_NE: u32 = 0x1a9f_07e0;

fn words(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The architecture and machine code of `symbol`, when it is one of this
/// module's functions.
pub fn function(symbol: &str) -> Option<(Arch, Vec<u8>)> {
    let name = symbol.strip_prefix(PREFIX)?;
    Some(match name {
        "cpuid" => (Arch::X86_64, CPUID.to_vec()),
        "rdtsc" => (Arch::X86_64, RDTSC.to_vec()),
        "rdpmc" => (Arch::X86_64, RDPMC.to_vec()),
        "rdrand64" => (Arch::X86_64, RDRAND64.to_vec()),
        "cntvct" => (Arch::Aarch64, words(&[ISB, MRS_CNTVCT, RET])),
        "pmccntr" => (Arch::Aarch64, words(&[ISB, MRS_PMCCNTR, ISB, RET])),
        "pmevcntr" => (
            Arch::Aarch64,
            words(&[MOV_W0_W0, MSR_PMSELR, ISB, MRS_PMXEVCNTR, ISB, RET]),
        ),
        "rndr" => (
            Arch::Aarch64,
            words(&[MRS_RNDR_X1, STR_X1_X0, CSET_W0_NE, RET]),
        ),
        _ => return None,
    })
}

/// The code of `symbol` for a target of `arch`: `Ok(None)` when it is not a
/// CPU function, an error when it is one of another architecture.
pub fn code_for(
    symbol: &str,
    arch: target_lexicon::Architecture,
) -> Result<Option<(Arch, Vec<u8>)>, String> {
    let Some((want, code)) = function(symbol) else {
        if symbol.starts_with(PREFIX) {
            return Err(format!("'{symbol}' is not a @vesper-std/cpu function"));
        }
        return Ok(None);
    };
    let have = match arch {
        target_lexicon::Architecture::X86_64 => Some(Arch::X86_64),
        target_lexicon::Architecture::Aarch64(_) => Some(Arch::Aarch64),
        _ => None,
    };
    if have != Some(want) {
        let wanted = match want {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        };
        return Err(format!(
            "'{symbol}' is an {wanted} instruction; this target is {arch}"
        ));
    }
    Ok(Some((want, code)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_function_has_code() {
        for name in [
            "cpuid", "rdtsc", "rdpmc", "rdrand64", "cntvct", "pmccntr", "pmevcntr", "rndr",
        ] {
            let (arch, code) = function(&format!("{PREFIX}{name}")).expect(name);
            assert!(!code.is_empty());
            if arch == Arch::Aarch64 {
                assert_eq!(code.len() % 4, 0, "{name}");
                assert_eq!(&code[code.len() - 4..], &RET.to_le_bytes(), "{name}");
            } else {
                assert_eq!(code.last(), Some(&0xc3), "{name}");
            }
        }
        assert!(function("vstd_cpu_nope").is_none());
        assert!(function("getpid").is_none());
    }

    #[test]
    fn the_other_architecture_is_refused() {
        use target_lexicon::Architecture as A;
        assert!(code_for("vstd_cpu_rdtsc", A::X86_64).unwrap().is_some());
        assert!(
            code_for(
                "vstd_cpu_rdtsc",
                A::Aarch64(target_lexicon::Aarch64Architecture::Aarch64)
            )
            .is_err()
        );
        assert!(code_for("vstd_cpu_cntvct", A::X86_64).is_err());
        assert!(code_for("vstd_cpu_nope", A::X86_64).is_err());
        assert!(code_for("getpid", A::X86_64).unwrap().is_none());
    }
}
