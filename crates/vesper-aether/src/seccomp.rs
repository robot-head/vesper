//! Seccomp-BPF profile for the embedded VMM (§37.1.5).
//!
//! [`SeccompProfile::vmm`] is an allowlist of the system calls the VMM's vCPU
//! and device threads need after setup (memory, file I/O on already-open
//! descriptors, polling, futexes, signals, clocks, exit), with `ioctl` limited
//! to the KVM and userfaultfd requests the run loop issues. Everything else,
//! including `execve`, `open*`, `socket`, `ptrace` and `mmap` of new devices, gets
//! the profile's default action.
//!
//! [`SeccompProfile::compile`] emits classic BPF for `SECCOMP_SET_MODE_FILTER`:
//! check `seccomp_data.arch` (any other audit arch, including the x32 ABI on
//! x86_64 because its syscall numbers carry bit 30 and never match, gets
//! `KILL_PROCESS`), load `nr`, compare against the allowlist, and for `ioctl`
//! compare the low 32 bits of the request argument (the kernel's `ioctl`
//! takes an `unsigned int` command). [`evaluate`] is an interpreter for exactly
//! the instructions the compiler emits, used to unit-test programs without a
//! kernel; [`validate`] applies the kernel's structural checks (bounded length,
//! in-range forward jumps, terminating return).
//!
//! The gated Linux backend installs a compiled profile with
//! `PR_SET_NO_NEW_PRIVS` and `seccomp(SECCOMP_SET_MODE_FILTER)`.
#![forbid(unsafe_code)]

pub const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
pub const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;
pub const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
pub const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
pub const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
/// The kernel's `BPF_MAXINSNS`.
pub const BPF_MAXINSNS: usize = 4096;

// Classic BPF opcodes used here.
pub const BPF_LD_W_ABS: u16 = 0x20;
pub const BPF_JMP_JEQ_K: u16 = 0x15;
pub const BPF_JMP_JA: u16 = 0x05;
pub const BPF_RET_K: u16 = 0x06;

// `struct seccomp_data` offsets.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARGS: u32 = 16;
const SECCOMP_DATA_BYTES: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}
impl Arch {
    pub fn audit(self) -> u32 {
        match self {
            Self::X86_64 => AUDIT_ARCH_X86_64,
            Self::Aarch64 => AUDIT_ARCH_AARCH64,
        }
    }
    /// The architecture this crate was compiled for, if supported.
    pub fn host() -> Option<Self> {
        if cfg!(target_arch = "x86_64") {
            Some(Self::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Self::Aarch64)
        } else {
            None
        }
    }
}

/// System calls the VMM profile names. Numbers per architecture are in
/// [`Syscall::number`] (checked against `libc` in the tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Syscall {
    Read,
    Write,
    Readv,
    Writev,
    Pread64,
    Pwrite64,
    Close,
    Ioctl,
    Ppoll,
    EpollPwait,
    Eventfd2,
    Futex,
    Mmap,
    Munmap,
    Mprotect,
    Madvise,
    Brk,
    RtSigreturn,
    RtSigprocmask,
    Sigaltstack,
    Exit,
    ExitGroup,
    ClockGettime,
    SchedYield,
    Getrandom,
    Gettid,
    Tgkill,
    // Not allowed by the VMM profile; named so tests can probe them.
    Openat,
    Socket,
    Execve,
    Getpid,
    Ptrace,
}
impl Syscall {
    /// Linux syscall number on `arch` (x86_64 `arch/x86/entry/syscalls/syscall_64.tbl`,
    /// AArch64 the generic `include/uapi/asm-generic/unistd.h`). `None`: no
    /// such call on that architecture.
    pub fn number(self, arch: Arch) -> Option<u32> {
        use Syscall::*;
        let (x86, a64) = match self {
            Read => (0, 63),
            Write => (1, 64),
            Readv => (19, 65),
            Writev => (20, 66),
            Pread64 => (17, 67),
            Pwrite64 => (18, 68),
            Close => (3, 57),
            Ioctl => (16, 29),
            Ppoll => (271, 73),
            EpollPwait => (281, 22),
            Eventfd2 => (290, 19),
            Futex => (202, 98),
            Mmap => (9, 222),
            Munmap => (11, 215),
            Mprotect => (10, 226),
            Madvise => (28, 233),
            Brk => (12, 214),
            RtSigreturn => (15, 139),
            RtSigprocmask => (14, 135),
            Sigaltstack => (131, 132),
            Exit => (60, 93),
            ExitGroup => (231, 94),
            ClockGettime => (228, 113),
            SchedYield => (24, 124),
            Getrandom => (318, 278),
            Gettid => (186, 178),
            Tgkill => (234, 131),
            Openat => (257, 56),
            Socket => (41, 198),
            Execve => (59, 221),
            Getpid => (39, 172),
            Ptrace => (101, 117),
        };
        Some(match arch {
            Arch::X86_64 => x86,
            Arch::Aarch64 => a64,
        })
    }
}

/// `ioctl` requests the KVM run loop and userfaultfd server issue.
pub mod ioctl {
    pub const KVM_RUN: u32 = 0xae80;
    pub const KVM_GET_REGS: u32 = 0x8090_ae81;
    pub const KVM_SET_REGS: u32 = 0x4090_ae82;
    pub const KVM_GET_SREGS: u32 = 0x8138_ae83;
    pub const KVM_SET_SREGS: u32 = 0x4138_ae84;
    pub const KVM_IRQ_LINE: u32 = 0x4008_ae61;
    pub const KVM_GET_DIRTY_LOG: u32 = 0x4010_ae42;
    pub const UFFDIO_WAKE: u32 = 0x8010_aa02;
    pub const UFFDIO_COPY: u32 = 0xc028_aa03;
    pub const UFFDIO_ZEROPAGE: u32 = 0xc020_aa04;
    pub const UFFDIO_CONTINUE: u32 = 0xc020_aa07;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultAction {
    KillProcess,
    /// Fail the call with this errno (for testing a profile in a live process).
    Errno(u16),
}
impl DefaultAction {
    fn ret(self) -> u32 {
        match self {
            Self::KillProcess => SECCOMP_RET_KILL_PROCESS,
            Self::Errno(e) => SECCOMP_RET_ERRNO | u32::from(e),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeccompError {
    /// A named call has no number on the target architecture.
    UnknownSyscall(Syscall),
    TooLong,
    EmptyProgram,
    JumpOutOfRange {
        at: usize,
    },
    BadLoad {
        at: usize,
    },
    UnsupportedInstruction {
        at: usize,
        code: u16,
    },
    NoTerminatingReturn,
    /// The interpreter ran off the end (cannot happen for validated programs).
    FellOff,
    /// `ioctl` is allowed only through its request list.
    IoctlInPlainAllowlist,
}

/// One classic BPF instruction (`struct sock_filter`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

/// `struct seccomp_data` for the interpreter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeccompData {
    pub nr: u32,
    pub arch: u32,
    pub instruction_pointer: u64,
    pub args: [u64; 6],
}
impl SeccompData {
    fn word(&self, offset: u32) -> Option<u32> {
        if !offset.is_multiple_of(4) || offset >= SECCOMP_DATA_BYTES {
            return None;
        }
        Some(match offset {
            OFF_NR => self.nr,
            OFF_ARCH => self.arch,
            8 => self.instruction_pointer as u32,
            12 => (self.instruction_pointer >> 32) as u32,
            o => {
                let i = ((o - OFF_ARGS) / 8) as usize;
                if (o - OFF_ARGS).is_multiple_of(8) {
                    self.args[i] as u32
                } else {
                    (self.args[i] >> 32) as u32
                }
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeccompProfile {
    pub arch: Arch,
    pub allowed: Vec<Syscall>,
    pub ioctl_requests: Vec<u32>,
    pub default: DefaultAction,
}

impl SeccompProfile {
    /// The VMM's post-setup profile.
    pub fn vmm(arch: Arch, default: DefaultAction) -> Self {
        use Syscall::*;
        Self {
            arch,
            allowed: vec![
                Read,
                Write,
                Readv,
                Writev,
                Pread64,
                Pwrite64,
                Close,
                Ppoll,
                EpollPwait,
                Eventfd2,
                Futex,
                Mmap,
                Munmap,
                Mprotect,
                Madvise,
                Brk,
                RtSigreturn,
                RtSigprocmask,
                Sigaltstack,
                Exit,
                ExitGroup,
                ClockGettime,
                SchedYield,
                Getrandom,
                Gettid,
                Tgkill,
            ],
            ioctl_requests: vec![
                ioctl::KVM_RUN,
                ioctl::KVM_GET_REGS,
                ioctl::KVM_SET_REGS,
                ioctl::KVM_GET_SREGS,
                ioctl::KVM_SET_SREGS,
                ioctl::KVM_IRQ_LINE,
                ioctl::KVM_GET_DIRTY_LOG,
                ioctl::UFFDIO_WAKE,
                ioctl::UFFDIO_COPY,
                ioctl::UFFDIO_ZEROPAGE,
                ioctl::UFFDIO_CONTINUE,
            ],
            default,
        }
    }

    /// Compiles to classic BPF (see the module docs for the layout).
    pub fn compile(&self) -> Result<Vec<SockFilter>, SeccompError> {
        let mut nrs = Vec::with_capacity(self.allowed.len());
        for &s in &self.allowed {
            if s == Syscall::Ioctl {
                return Err(SeccompError::IoctlInPlainAllowlist);
            }
            nrs.push(s.number(self.arch).ok_or(SeccompError::UnknownSyscall(s))?);
        }
        nrs.sort_unstable();
        nrs.dedup();
        let mut reqs = self.ioctl_requests.clone();
        reqs.sort_unstable();
        reqs.dedup();
        let ioctl_nr = Syscall::Ioctl
            .number(self.arch)
            .ok_or(SeccompError::UnknownSyscall(Syscall::Ioctl))?;
        let stmt = |code, k| SockFilter {
            code,
            jt: 0,
            jf: 0,
            k,
        };
        // Layout:
        //   0  ld arch
        //   1  jeq AUDIT, 1, 0
        //   2  ret KILL_PROCESS
        //   3  ld nr
        //   .. jeq nr_i -> ALLOW          (n entries)
        //   .. jeq ioctl -> IOCTL_BLOCK   (1 entry, only if requests)
        //   .. ret DEFAULT
        //   IOCTL_BLOCK: ld args[1].lo, jeq req_j -> ALLOW (m entries), ret DEFAULT
        //   ALLOW: ret ALLOW
        let n = nrs.len();
        let has_ioctl = !reqs.is_empty();
        let m = reqs.len();
        let ioctl_block = 4 + n + usize::from(has_ioctl) + 1;
        let allow = if has_ioctl {
            ioctl_block + 1 + m + 1
        } else {
            ioctl_block
        };
        if allow + 1 > BPF_MAXINSNS {
            return Err(SeccompError::TooLong);
        }
        let jump = |from: usize, to: usize| -> Result<u8, SeccompError> {
            u8::try_from(to - from - 1).map_err(|_| SeccompError::TooLong)
        };
        let mut p = vec![
            stmt(BPF_LD_W_ABS, OFF_ARCH),
            SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 1,
                jf: 0,
                k: self.arch.audit(),
            },
            stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
            stmt(BPF_LD_W_ABS, OFF_NR),
        ];
        for nr in nrs {
            let at = p.len();
            p.push(SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: jump(at, allow)?,
                jf: 0,
                k: nr,
            });
        }
        if has_ioctl {
            let at = p.len();
            p.push(SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: jump(at, ioctl_block)?,
                jf: 0,
                k: ioctl_nr,
            });
        }
        p.push(stmt(BPF_RET_K, self.default.ret()));
        if has_ioctl {
            debug_assert_eq!(p.len(), ioctl_block);
            p.push(stmt(BPF_LD_W_ABS, OFF_ARGS + 8));
            for req in reqs {
                let at = p.len();
                p.push(SockFilter {
                    code: BPF_JMP_JEQ_K,
                    jt: jump(at, allow)?,
                    jf: 0,
                    k: req,
                });
            }
            p.push(stmt(BPF_RET_K, self.default.ret()));
        }
        debug_assert_eq!(p.len(), allow);
        p.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
        validate(&p)?;
        Ok(p)
    }
}

/// The kernel's structural checks for the subset this module emits.
pub fn validate(program: &[SockFilter]) -> Result<(), SeccompError> {
    if program.is_empty() {
        return Err(SeccompError::EmptyProgram);
    }
    if program.len() > BPF_MAXINSNS {
        return Err(SeccompError::TooLong);
    }
    for (at, ins) in program.iter().enumerate() {
        match ins.code {
            BPF_LD_W_ABS => {
                if !ins.k.is_multiple_of(4) || ins.k >= SECCOMP_DATA_BYTES {
                    return Err(SeccompError::BadLoad { at });
                }
            }
            BPF_JMP_JEQ_K => {
                let far = at + 1 + usize::from(ins.jt.max(ins.jf));
                if far >= program.len() {
                    return Err(SeccompError::JumpOutOfRange { at });
                }
            }
            BPF_JMP_JA => {
                if at as u64 + 1 + u64::from(ins.k) >= program.len() as u64 {
                    return Err(SeccompError::JumpOutOfRange { at });
                }
            }
            BPF_RET_K => {}
            code => return Err(SeccompError::UnsupportedInstruction { at, code }),
        }
    }
    if program.last().map(|i| i.code) != Some(BPF_RET_K) {
        return Err(SeccompError::NoTerminatingReturn);
    }
    Ok(())
}

/// Runs a validated program on `data` and returns the seccomp action.
pub fn evaluate(program: &[SockFilter], data: &SeccompData) -> Result<u32, SeccompError> {
    validate(program)?;
    let mut acc = 0u32;
    let mut pc = 0usize;
    loop {
        let ins = program.get(pc).ok_or(SeccompError::FellOff)?;
        match ins.code {
            BPF_LD_W_ABS => {
                acc = data.word(ins.k).ok_or(SeccompError::BadLoad { at: pc })?;
                pc += 1;
            }
            BPF_JMP_JEQ_K => {
                pc += 1 + usize::from(if acc == ins.k { ins.jt } else { ins.jf });
            }
            BPF_JMP_JA => pc += 1 + ins.k as usize,
            BPF_RET_K => return Ok(ins.k),
            code => return Err(SeccompError::UnsupportedInstruction { at: pc, code }),
        }
    }
}

/// `struct sock_filter` bytes, as `sock_fprog.filter` points to them.
pub fn encode(program: &[SockFilter]) -> Vec<u8> {
    let mut out = Vec::with_capacity(program.len() * 8);
    for i in program {
        out.extend_from_slice(&i.code.to_ne_bytes());
        out.push(i.jt);
        out.push(i.jf);
        out.extend_from_slice(&i.k.to_ne_bytes());
    }
    out
}
