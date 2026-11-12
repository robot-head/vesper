//! `lean/EbpfFirewall.lean`: the source is free of `sorry`/`admit`/axiom
//! declarations; with `VESPER_LEAN` set (CI sets `VESPER_LEAN=lean`) the
//! file is compiled, every audited theorem's axioms are standard, and the
//! model's fixture table matches the real mitigator program run in the
//! interpreter. The Lean theorems are about the model, not the Rust code.

use std::path::PathBuf;
use vesper_ebpf::interp::{XdpEnv, run_xdp};
use vesper_ebpf::mitigator::{MitigatorConfig, build};

const AUDITED_THEOREMS: usize = 7;
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Quot.sound", "Classical.choice"];

fn lean_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean/EbpfFirewall.lean")
}

fn source() -> String {
    std::fs::read_to_string(lean_file()).expect("lean/EbpfFirewall.lean")
}

/// The Lean fixture rows `(ether_type, proto, flags, src)`, parsed from the
/// source so the two sides cannot drift.
fn fixture_rows(src: &str) -> Vec<[u64; 4]> {
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
            Some([v[0], v[1], v[2], v[3]])
        })
        .collect()
}

/// `mkPacket` in the Lean file.
fn mk_packet([ether, proto, flags, src]: [u64; 4]) -> Vec<u8> {
    let mut p = vec![2u8; 12];
    p.extend([(ether >> 8) as u8, ether as u8, 0x45, 0]);
    p.extend([0u8; 7]);
    p.push(proto as u8);
    p.extend([0, 0]);
    p.extend((src as u32).to_be_bytes());
    p.extend([0u8; 6]);
    p.extend([0x01, 0xbb]);
    p.extend([0u8; 8]);
    p.extend([0x50, flags as u8]);
    p.extend([0u8; 6]);
    assert_eq!(p.len(), 54);
    p
}

fn interpreter_codes(rows: &[[u64; 4]]) -> Vec<u64> {
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
        ("offSrc", o::SRC_IPV4),
        ("offDstPort", o::DST_PORT),
        ("offFlags", o::DATA_OFFSET_FLAGS),
    ] {
        assert!(
            src.contains(&format!("def {name} : Nat := {off}\n")),
            "{name}"
        );
    }
    assert_eq!(fixture_rows(&src).len(), 15);
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
    // threshold 2: the third and fourth SYN-only packets from source 7 drop
    assert_eq!(
        interpreter_codes(&rows),
        vec![2, 2, 1, 1, 2, 2, 2, 2, 2, 1, 2, 1, 1, 2, 2]
    );
}
