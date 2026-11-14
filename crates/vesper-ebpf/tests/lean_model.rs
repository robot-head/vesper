//! `lean/EbpfFirewall.lean`: the source is free of `sorry`/`admit`/axiom
//! declarations; with `VESPER_LEAN` set (CI sets `VESPER_LEAN=lean`) the
//! file is compiled, every audited theorem's axioms are standard, and the
//! model's fixture table matches the real mitigator program run in the
//! interpreter. The Lean theorems are about the model, not the Rust code.
//! The bounds-tracking model's fixture programs are rebuilt as BPF and
//! must get the model's verdicts from this crate's verifier (and the
//! kernel's, where the host loads programs).

use std::path::PathBuf;
use vesper_ebpf::interp::{XdpEnv, run_xdp};
use vesper_ebpf::mitigator::{MitigatorConfig, build};

const AUDITED_THEOREMS: usize = 10;
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Quot.sound", "Classical.choice"];

fn lean_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean/EbpfFirewall.lean")
}

fn source() -> String {
    std::fs::read_to_string(lean_file()).expect("lean/EbpfFirewall.lean")
}

/// The Lean fixture rows `(ether_type, proto, flags, src, ihl)`, parsed
/// from the source so the two sides cannot drift.
fn fixture_rows(src: &str) -> Vec<[u64; 5]> {
    let start = src.find("def fixture").expect("fixture");
    let body = &src[start..];
    let body = &body[body.find('[').expect("[") + 1..body.find(']').expect("]")];
    body.split(')')
        .filter_map(|t| {
            let t = t.trim_start_matches([',', ' ', '\n', '(']);
            if t.trim().is_empty() {
                return None;
            }
            let v: Vec<u64> = t
                .split(',')
                .map(|x| {
                    let x = x.trim();
                    match x.strip_prefix("0x") {
                        Some(h) => u64::from_str_radix(h, 16).expect("hex"),
                        None => x.parse().expect("number"),
                    }
                })
                .collect();
            Some([v[0], v[1], v[2], v[3], v[4]])
        })
        .collect()
}

/// `mkPacket` in the Lean file.
fn mk_packet([ether, proto, flags, src, ihl]: [u64; 5]) -> Vec<u8> {
    let mut p = vec![2u8; 12];
    p.extend([(ether >> 8) as u8, ether as u8, 0x40 + (ihl % 16) as u8, 0]);
    p.extend([0u8; 7]);
    p.push(proto as u8);
    p.extend([0, 0]);
    p.extend((src as u32).to_be_bytes());
    p.extend([0u8; 4]);
    p.extend(vec![0u8; 4 * ihl.saturating_sub(5) as usize]);
    p.extend([0, 0, 0x01, 0xbb]);
    p.extend([0u8; 8]);
    p.extend([0x50, flags as u8]);
    p.extend([0u8; 6]);
    assert_eq!(p.len(), 54 + 4 * ihl.saturating_sub(5) as usize);
    p
}

fn interpreter_codes(rows: &[[u64; 5]]) -> Vec<u64> {
    let prog = build(&MitigatorConfig {
        pps_threshold: 2,
        ..MitigatorConfig::default()
    })
    .unwrap();
    let mut env = XdpEnv::new(&prog);
    rows.iter()
        .map(|r| run_xdp(&prog, &mut env, &mk_packet(*r)).unwrap().ret)
        .collect()
}

#[test]
fn source_is_complete() {
    let src = source();
    for banned in ["sorry", "admit"] {
        assert!(!src.contains(banned), "Lean model contains {banned}");
    }
    assert!(!src.lines().any(|l| l.trim_start().starts_with("axiom ")));
    assert_eq!(src.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    assert!(src.contains("theorem syn_mitigator_never_drops_ack_packets"));
    assert!(src.contains("theorem reads_within_checked_bounds"));
    assert!(src.contains("theorem drop_iff_spec"));
    // the model reads the same offsets as the program
    use vesper_ebpf::wire::eth_ipv4_tcp as o;
    for (name, off) in [
        ("offEtherType", o::ETHER_TYPE),
        ("offProto", o::PROTO),
        ("offVerIhl", o::VER_IHL),
        ("offSrc", o::SRC_IPV4),
        ("offDstPort", o::DST_PORT),
        ("offFlags", o::DATA_OFFSET_FLAGS),
    ] {
        assert!(
            src.contains(&format!("def {name} : Nat := {off}\n")),
            "{name}"
        );
    }
    // TCP-relative offsets: the fixed ones above are these plus 34 (IHL 5)
    for (name, off) in [
        ("tcpDstPort", o::DST_PORT - o::SRC_PORT),
        ("tcpFlags", o::DATA_OFFSET_FLAGS - o::SRC_PORT),
    ] {
        assert!(
            src.contains(&format!("def {name} : Nat := {off}\n")),
            "{name}"
        );
    }
    assert_eq!(fixture_rows(&src).len(), 21);
}

#[test]
fn lean_checks_and_fixture_matches_the_program() {
    let Some(lean) = std::env::var_os("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN not set; Lean check skipped");
        return;
    };
    let out = std::process::Command::new(lean)
        .current_dir(lean_file().parent().unwrap())
        .arg("EbpfFirewall.lean")
        .output()
        .expect("spawn Lean");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8");
    assert!(
        out.status.success(),
        "Lean failed: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let mut audited = 0;
    let mut fixture = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("\"fixture: [") {
            let rest = rest.strip_suffix("]\"").expect("fixture line");
            fixture = Some(
                rest.split(", ")
                    .map(|x| x.parse::<u64>().expect("code"))
                    .collect::<Vec<_>>(),
            );
            continue;
        }
        if let Some(rest) = line.strip_prefix("\"bounds: [") {
            let rest = rest.strip_suffix("]\"").expect("bounds line");
            let got: Vec<bool> = rest.split(", ").map(|x| x == "true").collect();
            assert_eq!(
                got,
                bounds_verdicts(&source()),
                "Lean #eval vs the proved example"
            );
            continue;
        }
        if line.ends_with("does not depend on any axioms") {
            audited += 1;
            continue;
        }
        let axioms = line
            .split_once(" depends on axioms: [")
            .and_then(|(_, r)| r.strip_suffix(']'))
            .unwrap_or_else(|| panic!("unexpected Lean output: {line}"));
        for a in axioms.split(", ") {
            assert!(
                STANDARD_AXIOMS.contains(&a),
                "unapproved axiom {a} in {line}"
            );
        }
        audited += 1;
    }
    assert_eq!(audited, AUDITED_THEOREMS);
    let rows = fixture_rows(&source());
    assert_eq!(fixture.expect("fixture output"), interpreter_codes(&rows));
}

#[test]
fn fixture_rows_run_in_the_interpreter() {
    let rows = fixture_rows(&source());
    // threshold 2: the third and fourth SYN-only packets from source 7
    // drop; source 11's SYNs carry IPv4 options and are counted all the same
    assert_eq!(
        interpreter_codes(&rows),
        vec![
            2, 2, 1, 1, 2, 2, 2, 2, 2, 1, 2, 1, 1, 2, 2, 2, 2, 1, 2, 2, 2
        ]
    );
}

/// The bounds model's fixture programs, parsed from `boundsPrograms`: one
/// program per line, instructions as `.name arg..`.
fn bounds_programs(src: &str) -> Vec<Vec<(String, Vec<u32>)>> {
    let start = src.find("def boundsPrograms").expect("boundsPrograms");
    let body = &src[start..];
    let body = &body[..body.find("\n\n").expect("end of boundsPrograms")];
    body.lines()
        .filter_map(|l| {
            let a = l.find("[.")?;
            let e = a + l[a..].find(']').expect("]");
            Some(
                l[a + 1..e]
                    .split(", ")
                    .map(|i| {
                        let mut w = i.trim().trim_start_matches('.').split(' ');
                        let name = w.next().expect("name").to_owned();
                        (name, w.map(|x| x.parse().expect("arg")).collect())
                    })
                    .collect(),
            )
        })
        .collect()
}

/// The verdicts the Lean file proves (`example : ... = [..] := by decide`).
fn bounds_verdicts(src: &str) -> Vec<bool> {
    let at = src
        .find("example : boundsPrograms.map (verify init) = [")
        .expect("verdict example");
    let rest = &src[at..];
    let list = &rest[rest.find("= [").expect("[") + 3..rest.find(']').expect("]")];
    list.split(", ").map(|x| x == "true").collect()
}

/// A model program as BPF: model registers are BPF registers 2..=8, `r9`
/// is the bounds-check scratch register, `r0` is `XDP_PASS` throughout.
fn bounds_bpf(insns: &[(String, Vec<u32>)]) -> vesper_ebpf::Program {
    use vesper_ebpf::Asm;
    use vesper_ebpf::insn::{AluOp, JmpOp, Reg, Size};
    let reg = |i: u32| {
        assert!((2..=8).contains(&i), "model register {i}");
        [
            Reg::R2,
            Reg::R3,
            Reg::R4,
            Reg::R5,
            Reg::R6,
            Reg::R7,
            Reg::R8,
        ][i as usize - 2]
    };
    let mut a = Asm::new();
    let out = a.label();
    a.mov64(Reg::R0, 2);
    for (name, v) in insns {
        let imm = |i: usize| v[i] as i32;
        match name.as_str() {
            "ldData" => a.ldx(Size::W, reg(v[0]), Reg::R1, 0),
            "ldDataEnd" => a.ldx(Size::W, reg(v[0]), Reg::R1, 4),
            "movImm" => a.mov64(reg(v[0]), imm(1)),
            "mov" => a.mov64(reg(v[0]), reg(v[1])),
            "addImm" => a.alu64(AluOp::Add, reg(v[0]), imm(1)),
            "andImm" => a.alu64(AluOp::And, reg(v[0]), imm(1)),
            "mulImm" => a.alu64(AluOp::Mul, reg(v[0]), imm(1)),
            "addReg" => a.alu64(AluOp::Add, reg(v[0]), reg(v[1])),
            "exitIfPast" => {
                a.mov64(Reg::R9, reg(v[0]));
                a.alu64(AluOp::Add, Reg::R9, imm(1));
                a.jmp(JmpOp::Gt, Reg::R9, reg(v[2]), out)
            }
            "ldxb" => a.ldx(Size::B, reg(v[0]), reg(v[1]), v[2] as i16),
            other => panic!("unknown model instruction {other}"),
        };
    }
    a.bind(out).unwrap();
    a.exit();
    let mut p = a.finish("bounds", vesper_ebpf::ProgramType::Xdp).unwrap();
    p.license = "GPL".into();
    p
}

#[test]
fn bounds_model_verdicts_match_the_verifiers() {
    use vesper_ebpf::{VerifierPolicy, verify};
    let src = source();
    let progs = bounds_programs(&src);
    let want = bounds_verdicts(&src);
    assert_eq!(progs.len(), want.len());
    assert!(want.contains(&true) && want.contains(&false));
    #[cfg(target_os = "linux")]
    let kernel = vesper_ebpf::kernel::probe().is_ok();
    for (i, (insns, &ok)) in progs.iter().zip(&want).enumerate() {
        let p = bounds_bpf(insns);
        let ours = verify(&p, &VerifierPolicy::default());
        assert_eq!(ours.is_ok(), ok, "program {i}: {ours:?}");
        if ok {
            // accepted: runs without faults on packets of every length
            for len in [0usize, 14, 33, 34, 35, 60, 74, 128] {
                let pkt: Vec<u8> = (0..len).map(|b| (b * 37 + 5) as u8).collect();
                let mut env = XdpEnv::new(&p);
                assert_eq!(run_xdp(&p, &mut env, &pkt).expect("no fault").ret, 2);
            }
        } else {
            let e = ours.unwrap_err();
            assert!(e.rule.starts_with("mem."), "program {i}: {e}");
        }
        #[cfg(target_os = "linux")]
        if kernel {
            let k = vesper_ebpf::kernel::load(&p);
            assert_eq!(k.is_ok(), ok, "program {i}: kernel {:?}", k.err());
        }
    }
}
