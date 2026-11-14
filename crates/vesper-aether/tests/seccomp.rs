//! The VMM seccomp-bpf profile: compiled program semantics (interpreter, both
//! architectures) and, on Linux, a live install in a child process.
use vesper_aether::seccomp::*;

fn data(arch: Arch, nr: u32, args: [u64; 6]) -> SeccompData {
    SeccompData {
        nr,
        arch: arch.audit(),
        instruction_pointer: 0x40_0000,
        args,
    }
}

const EPERM: u16 = 1;

#[test]
fn compiled_profile_allows_exactly_the_allowlist() {
    for arch in [Arch::X86_64, Arch::Aarch64] {
        let profile = SeccompProfile::vmm(arch, DefaultAction::Errno(EPERM));
        let prog = profile.compile().unwrap();
        validate(&prog).unwrap();
        let run = |nr, args| evaluate(&prog, &data(arch, nr, args)).unwrap();
        for s in &profile.allowed {
            assert_eq!(
                run(s.number(arch).unwrap(), [0; 6]),
                SECCOMP_RET_ALLOW,
                "{arch:?} {s:?}"
            );
        }
        for s in [
            Syscall::Openat,
            Syscall::Socket,
            Syscall::Execve,
            Syscall::Getpid,
            Syscall::Ptrace,
        ] {
            assert_eq!(
                run(s.number(arch).unwrap(), [0; 6]),
                SECCOMP_RET_ERRNO | u32::from(EPERM),
                "{arch:?} {s:?} must be denied"
            );
        }
        // Every number not in the allowlist is denied (exhaustive over 0..1024).
        let allowed: Vec<u32> = profile
            .allowed
            .iter()
            .map(|s| s.number(arch).unwrap())
            .collect();
        let ioctl = Syscall::Ioctl.number(arch).unwrap();
        for nr in 0..1024 {
            let want = if allowed.contains(&nr) {
                SECCOMP_RET_ALLOW
            } else {
                SECCOMP_RET_ERRNO | 1
            };
            if nr != ioctl {
                assert_eq!(run(nr, [0; 6]), want, "{arch:?} nr {nr}");
            }
        }
        // ioctl: only the listed requests, compared on the low 32 bits.
        for &req in &profile.ioctl_requests {
            assert_eq!(
                run(ioctl, [3, u64::from(req), 0, 0, 0, 0]),
                SECCOMP_RET_ALLOW
            );
            assert_eq!(
                run(
                    ioctl,
                    [3, 0xffff_ffff_0000_0000 | u64::from(req), 0, 0, 0, 0]
                ),
                SECCOMP_RET_ALLOW
            );
        }
        assert_eq!(
            run(ioctl, [3, 0x5401, 0, 0, 0, 0]),
            SECCOMP_RET_ERRNO | 1,
            "TCGETS"
        );
        assert_eq!(
            run(ioctl, [3, 0xae01, 0, 0, 0, 0]),
            SECCOMP_RET_ERRNO | 1,
            "KVM_CREATE_VM"
        );
        // Another audit arch is killed outright.
        let other = SeccompData {
            arch: 0x4000_0003, // AUDIT_ARCH_I386
            ..data(arch, 0, [0; 6])
        };
        assert_eq!(evaluate(&prog, &other).unwrap(), SECCOMP_RET_KILL_PROCESS);
        let kill = SeccompProfile::vmm(arch, DefaultAction::KillProcess)
            .compile()
            .unwrap();
        assert_eq!(
            evaluate(
                &kill,
                &data(arch, Syscall::Execve.number(arch).unwrap(), [0; 6])
            )
            .unwrap(),
            SECCOMP_RET_KILL_PROCESS
        );
    }
    // x32 ABI numbers (bit 30 set) never match on x86_64.
    let prog = SeccompProfile::vmm(Arch::X86_64, DefaultAction::Errno(EPERM))
        .compile()
        .unwrap();
    assert_eq!(
        evaluate(&prog, &data(Arch::X86_64, 0x4000_0000, [0; 6])).unwrap(),
        SECCOMP_RET_ERRNO | 1
    );
}

#[test]
fn malformed_programs_are_rejected() {
    let ret = SockFilter {
        code: BPF_RET_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    };
    assert_eq!(validate(&[]), Err(SeccompError::EmptyProgram));
    assert_eq!(
        validate(&[
            SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 5,
                jf: 0,
                k: 0
            },
            ret
        ]),
        Err(SeccompError::JumpOutOfRange { at: 0 })
    );
    assert_eq!(
        validate(&[
            SockFilter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: 64
            },
            ret
        ]),
        Err(SeccompError::BadLoad { at: 0 })
    );
    assert_eq!(
        validate(&[
            SockFilter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: 2
            },
            ret
        ]),
        Err(SeccompError::BadLoad { at: 0 })
    );
    assert_eq!(
        validate(&[
            SockFilter {
                code: 0x07,
                jt: 0,
                jf: 0,
                k: 0
            },
            ret
        ]),
        Err(SeccompError::UnsupportedInstruction { at: 0, code: 7 })
    );
    assert_eq!(
        validate(&[SockFilter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 0
        }]),
        Err(SeccompError::NoTerminatingReturn)
    );
    assert_eq!(
        validate(&vec![ret; BPF_MAXINSNS + 1]),
        Err(SeccompError::TooLong)
    );
    let mut p = SeccompProfile::vmm(Arch::X86_64, DefaultAction::KillProcess);
    p.allowed.push(Syscall::Ioctl);
    assert_eq!(p.compile(), Err(SeccompError::IoctlInPlainAllowlist));
    // A profile too large for 8-bit jump offsets is refused, not truncated.
    let mut p = SeccompProfile::vmm(Arch::X86_64, DefaultAction::KillProcess);
    p.ioctl_requests = (0..400).collect();
    assert_eq!(p.compile(), Err(SeccompError::TooLong));
    // Encoding is 8 bytes per instruction.
    assert_eq!(encode(&[ret, ret]).len(), 16);
}

#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::useless_conversion)] // c_long is i64 on the 64-bit targets tested
fn syscall_numbers_match_libc_on_this_host() {
    let arch = Arch::host().unwrap();
    let pairs: &[(Syscall, libc::c_long)] = &[
        (Syscall::Read, libc::SYS_read),
        (Syscall::Write, libc::SYS_write),
        (Syscall::Readv, libc::SYS_readv),
        (Syscall::Writev, libc::SYS_writev),
        (Syscall::Pread64, libc::SYS_pread64),
        (Syscall::Pwrite64, libc::SYS_pwrite64),
        (Syscall::Close, libc::SYS_close),
        (Syscall::Ioctl, libc::SYS_ioctl),
        (Syscall::Ppoll, libc::SYS_ppoll),
        (Syscall::EpollPwait, libc::SYS_epoll_pwait),
        (Syscall::Eventfd2, libc::SYS_eventfd2),
        (Syscall::Futex, libc::SYS_futex),
        (Syscall::Mmap, libc::SYS_mmap),
        (Syscall::Munmap, libc::SYS_munmap),
        (Syscall::Mprotect, libc::SYS_mprotect),
        (Syscall::Madvise, libc::SYS_madvise),
        (Syscall::Brk, libc::SYS_brk),
        (Syscall::RtSigreturn, libc::SYS_rt_sigreturn),
        (Syscall::RtSigprocmask, libc::SYS_rt_sigprocmask),
        (Syscall::Sigaltstack, libc::SYS_sigaltstack),
        (Syscall::Exit, libc::SYS_exit),
        (Syscall::ExitGroup, libc::SYS_exit_group),
        (Syscall::ClockGettime, libc::SYS_clock_gettime),
        (Syscall::SchedYield, libc::SYS_sched_yield),
        (Syscall::Getrandom, libc::SYS_getrandom),
        (Syscall::Gettid, libc::SYS_gettid),
        (Syscall::Tgkill, libc::SYS_tgkill),
        (Syscall::Openat, libc::SYS_openat),
        (Syscall::Socket, libc::SYS_socket),
        (Syscall::Execve, libc::SYS_execve),
        (Syscall::Getpid, libc::SYS_getpid),
        (Syscall::Ptrace, libc::SYS_ptrace),
    ];
    for &(s, n) in pairs {
        assert_eq!(i64::from(s.number(arch).unwrap()), i64::from(n), "{s:?}");
    }
}

/// Child half of the live test: installs the profile (default EPERM) and
/// reports what each probe returned. Does nothing unless re-executed.
#[test]
fn seccomp_child() {
    if std::env::var_os("VESPER_SECCOMP_CHILD").is_none() {
        return;
    }
    use vesper_aether::host::{SeccompProbe, install_seccomp, seccomp_probe};
    let arch = Arch::host().unwrap();
    let prog = SeccompProfile::vmm(arch, DefaultAction::Errno(EPERM))
        .compile()
        .unwrap();
    let before = seccomp_probe(SeccompProbe::Getpid);
    if let Err(e) = install_seccomp(&prog) {
        println!("\nSECCOMP-UNAVAILABLE {e:?}");
        std::process::exit(0);
    }
    let getpid = seccomp_probe(SeccompProbe::Getpid);
    let socket = seccomp_probe(SeccompProbe::Socket);
    let kvm_run = seccomp_probe(SeccompProbe::Ioctl(ioctl::KVM_RUN));
    let tcgets = seccomp_probe(SeccompProbe::Ioctl(0x5401));
    println!(
        "\nSECCOMP-RESULT before={before} getpid={getpid} socket={socket} kvm_run={kvm_run} tcgets={tcgets}"
    );
    std::process::exit(0);
}

#[test]
fn live_filter_in_a_child_process() {
    if !cfg!(target_os = "linux") || Arch::host().is_none() {
        eprintln!("seccomp live install: unsupported platform");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let out = match std::process::Command::new(exe)
        .args([
            "--exact",
            "seccomp_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("VESPER_SECCOMP_CHILD", "1")
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            assert!(std::env::var_os("VESPER_REQUIRE_SECCOMP").is_none(), "{e}");
            eprintln!("seccomp live install: could not re-execute the test binary: {e}");
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| l.find("SECCOMP-").map(|i| &l[i..]))
        .unwrap_or_else(|| {
            panic!(
                "child printed no result: {stdout} {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    eprintln!("{line}");
    if line.starts_with("SECCOMP-UNAVAILABLE") {
        assert!(
            std::env::var_os("VESPER_REQUIRE_SECCOMP").is_none(),
            "{line}"
        );
        return;
    }
    let field = |k: &str| -> i64 {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(&format!("{k}=")))
            .unwrap()
            .parse()
            .unwrap()
    };
    assert!(field("before") > 0);
    // getpid is not in the VMM allowlist: EPERM after install.
    assert_eq!(field("getpid"), -1);
    assert_eq!(field("socket"), -1);
    // An allowed ioctl request passes the filter and fails EBADF on fd -1.
    assert_eq!(field("kvm_run"), -9);
    // A request outside the list is stopped by the filter.
    assert_eq!(field("tcgets"), -1);
}
