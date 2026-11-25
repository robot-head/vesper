//! The Vesper seccomp-BPF compiler, validator, and evaluator against the
//! Rust reference (both builds are in the crate under `vesper-port`).

use crate::seccomp::{
    AUDIT_ARCH_AARCH64, AUDIT_ARCH_X86_64, Arch, BPF_JMP_JA, BPF_JMP_JEQ_K, BPF_LD_W_ABS,
    BPF_MAXINSNS, BPF_RET_K, DefaultAction, SECCOMP_RET_ALLOW, SECCOMP_RET_KILL_PROCESS,
    SeccompData, SeccompProfile, SockFilter, Syscall, evaluate, evaluate_reference, validate,
    validate_reference,
};

fn stmt(code: u16, k: u32) -> SockFilter {
    SockFilter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

#[test]
fn compile_matches_rust() {
    let profiles = [
        SeccompProfile::vmm(Arch::X86_64, DefaultAction::KillProcess),
        SeccompProfile::vmm(Arch::X86_64, DefaultAction::KillThread),
        SeccompProfile::vmm(Arch::Aarch64, DefaultAction::Errno(1)),
        SeccompProfile {
            arch: Arch::X86_64,
            allowed: vec![Syscall::Write, Syscall::Read, Syscall::Write],
            ioctl_requests: vec![],
            default: DefaultAction::Errno(13),
        },
        SeccompProfile {
            arch: Arch::Aarch64,
            allowed: vec![Syscall::Ioctl],
            ioctl_requests: vec![],
            default: DefaultAction::KillProcess,
        },
    ];
    for p in &profiles {
        assert_eq!(p.compile(), p.compile_reference());
    }
}

#[test]
fn validate_and_evaluate_match_rust() {
    for arch in [Arch::X86_64, Arch::Aarch64] {
        let profile = SeccompProfile::vmm(arch, DefaultAction::Errno(1));
        let prog = profile.compile().unwrap();
        assert_eq!(validate(&prog), validate_reference(&prog));

        let ioctl = Syscall::Ioctl.number(arch).unwrap();
        for nr in 0..360 {
            let d = SeccompData {
                nr,
                arch: arch.audit(),
                instruction_pointer: 0x1234_5678_9abc_def0,
                args: [0, u64::from(profile.ioctl_requests[0]), 2, 3, 4, 5],
            };
            assert_eq!(evaluate(&prog, &d), evaluate_reference(&prog, &d));
        }

        // Wrong arch and x32 bit-30 syscall.
        let wrong_arch = SeccompData {
            nr: 1,
            arch: if arch == Arch::X86_64 {
                AUDIT_ARCH_AARCH64
            } else {
                AUDIT_ARCH_X86_64
            },
            instruction_pointer: 0,
            args: [0; 6],
        };
        assert_eq!(
            evaluate(&prog, &wrong_arch),
            evaluate_reference(&prog, &wrong_arch)
        );
        let ioctl_denied = SeccompData {
            nr: ioctl,
            arch: arch.audit(),
            instruction_pointer: 0,
            args: [3, 0x5401, 0, 0, 0, 0],
        };
        assert_eq!(
            evaluate(&prog, &ioctl_denied),
            evaluate_reference(&prog, &ioctl_denied)
        );
    }

    // Exercise BPF_JMP_JA and ip/arg word loads.
    let ja_prog = vec![
        stmt(BPF_LD_W_ABS, 8),
        SockFilter {
            code: BPF_JMP_JA,
            jt: 0,
            jf: 0,
            k: 1,
        },
        stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        stmt(BPF_LD_W_ABS, 60),
        SockFilter {
            code: BPF_JMP_JEQ_K,
            jt: 1,
            jf: 0,
            k: 0xDEAD_BEEF,
        },
        stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
    ];
    let d = SeccompData {
        nr: 0,
        arch: AUDIT_ARCH_X86_64,
        instruction_pointer: 0x1122_3344_5566_7788,
        args: [0, 0, 0, 0, 0, 0xDEAD_BEEF_0000_0001],
    };
    assert_eq!(evaluate(&ja_prog, &d), evaluate_reference(&ja_prog, &d));

    // Malformed programs must fail identically.
    let bad_programs: Vec<Vec<SockFilter>> = vec![
        vec![],
        vec![stmt(BPF_LD_W_ABS, 0)],
        vec![stmt(BPF_LD_W_ABS, 2), stmt(BPF_RET_K, SECCOMP_RET_ALLOW)],
        vec![stmt(BPF_LD_W_ABS, 64), stmt(BPF_RET_K, SECCOMP_RET_ALLOW)],
        vec![
            SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 1,
                jf: 0,
                k: 0,
            },
            stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
        ],
        vec![
            SockFilter {
                code: BPF_JMP_JA,
                jt: 0,
                jf: 0,
                k: 1,
            },
            stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
        ],
        vec![stmt(0xffff, 0), stmt(BPF_RET_K, SECCOMP_RET_ALLOW)],
        vec![stmt(BPF_RET_K, SECCOMP_RET_ALLOW); BPF_MAXINSNS + 1],
    ];
    for p in &bad_programs {
        assert_eq!(validate(p), validate_reference(p));
        assert_eq!(evaluate(p, &d), evaluate_reference(p, &d));
    }
}
