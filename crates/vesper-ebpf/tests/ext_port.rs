//! The port's `.BTF.ext` reader, `local_type` and `maps_from_btf` against
//! the Rust ones, result for result: objects this crate writes for every
//! corpus program (with maps, kfuncs, line info and CO-RE relocations of
//! every kind added), thousands of mutated copies of their `.BTF.ext` and
//! `.BTF`, and every type of random and host BTF. Port wave P3a lane 6
//! (#1041).
#![cfg(feature = "vesper-port")]

mod common;

use common::Rng;
use vesper_ebpf::btf::Btf;
use vesper_ebpf::btf_ext::{build, local_type, maps_from_btf, parse_ext};
use vesper_ebpf::core::{CoreKind, CoreRelo, CoreSpec, LocalType};
use vesper_ebpf::{LineInfo, Program};
use vesper_ebpf_rust as rust;

fn local(rng: &mut Rng, depth: u32) -> LocalType {
    let names = ["task_struct", "sk_buff___v1", "pt_regs", "x", "", "ü"];
    match rng.below(if depth > 2 { 2 } else { 6 }) {
        0 => LocalType::int(
            rng.pick(&["int", "long", "u8"]),
            rng.pick(&[1, 4, 8]),
            rng.chance(50),
        ),
        1 => LocalType::enumeration(rng.pick(&names), 4, &[("A", -1), ("B", 7)]),
        2 => LocalType::Ptr(Box::new(local(rng, depth + 1))),
        3 => LocalType::Array {
            elem: Box::new(local(rng, depth + 1)),
            len: rng.below(5) as u32,
        },
        _ => {
            let mut ms = Vec::new();
            let mut bit = 0;
            for _ in 0..rng.below(4) {
                let width = if rng.chance(30) {
                    1 + rng.below(9) as u32
                } else {
                    0
                };
                ms.push((rng.pick(&names), local(rng, depth + 1), bit, width));
                bit += if width != 0 { width } else { 64 };
            }
            LocalType::structure_bits(rng.pick(&names), bit.div_ceil(8), ms)
        }
    }
}

/// `prog` with line info and CO-RE relocations added.
fn decorate(rng: &mut Rng, mut prog: Program) -> Program {
    let n = prog.insns.len().max(1);
    for _ in 0..rng.below(4) {
        prog.lines.push(LineInfo {
            slot: rng.below(n as u64) as usize,
            file: rng.pick(&["a.ves", "b/c.ves", ""]).to_owned(),
            text: rng.pick(&["let x = 1;", "", "ü 中"]).to_owned(),
            line: rng.below(5000) as u32,
            col: rng.below(1023) as u32,
        });
    }
    for _ in 0..rng.below(5) {
        let root = local(rng, 0);
        let kind = CoreKind::from_code(rng.below(13) as u32).unwrap();
        let spec = match &root {
            LocalType::Enum { .. } if rng.chance(50) => {
                CoreSpec::enumval(CoreKind::EnumvalValue, root, "B")
            }
            LocalType::Struct { members, .. } if !members.is_empty() && rng.chance(70) => {
                let m = members[0].name.clone();
                CoreSpec::field(CoreKind::FieldByteOffset, root, &[m.as_str()])
            }
            _ => CoreSpec::of_type(
                if kind.code() >= 6 && kind.code() != 10 && kind.code() != 11 {
                    kind
                } else {
                    CoreKind::TypeSize
                },
                root,
            ),
        };
        if let Ok(spec) = spec {
            prog.core_relos.push(CoreRelo {
                slot: rng.below(n as u64) as usize,
                spec,
            });
        }
    }
    prog
}

fn port_ext(btf: &[u8], ext: &[u8], section: &str) -> String {
    match Btf::parse(btf) {
        Ok(b) => format!("{:?} {:?}", parse_ext(&b, ext, section), maps_from_btf(&b)),
        Err(e) => format!("btf {e}"),
    }
}

fn rust_ext(btf: &[u8], ext: &[u8], section: &str) -> String {
    match rust::btf::Btf::parse(btf) {
        Ok(b) => format!(
            "{:?} {:?}",
            rust::btf_ext::parse_ext(&b, ext, section),
            rust::btf_ext::maps_from_btf(&b)
        ),
        Err(e) => format!("btf {e}"),
    }
}

fn mutate(rng: &mut Rng, b: &[u8]) -> Vec<u8> {
    let mut m = b.to_vec();
    if m.is_empty() {
        return m;
    }
    match rng.below(4) {
        0 => m.truncate(rng.below(m.len() as u64 + 1) as usize),
        1 => {
            let at = (rng.below(m.len() as u64 / 4) * 4) as usize;
            let v = rng.pick(&[0u32, 1, 3, 4, 7, 8, 13, 16, 24, 256, 257, u32::MAX]);
            let e = (at + 4).min(m.len());
            m[at..e].copy_from_slice(&v.to_le_bytes()[..e - at]);
        }
        _ => {
            for _ in 0..1 + rng.below(3) {
                let at = rng.below(m.len() as u64) as usize;
                m[at] = rng.next() as u8;
            }
        }
    }
    m
}

#[test]
fn port_matches_rust_on_objects_and_mutations() {
    let mut rng = Rng(0x00e7_0b7f_1041);
    let mut ok = 0;
    let mut errors = std::collections::BTreeSet::new();
    let mut objects = 0;
    for c in common::corpus::corpus() {
        let prog = decorate(&mut rng, c.prog.clone());
        let Ok(obj) = build(&prog, "xdp") else {
            continue;
        };
        objects += 1;
        for section in ["xdp", "tc", ""] {
            assert_eq!(
                port_ext(&obj.btf, &obj.ext, section),
                rust_ext(&obj.btf, &obj.ext, section)
            );
        }
        for i in 0..60 {
            let (btf, ext) = if i % 4 == 3 {
                (mutate(&mut rng, &obj.btf), obj.ext.clone())
            } else {
                (obj.btf.clone(), mutate(&mut rng, &obj.ext))
            };
            let p = port_ext(&btf, &ext, "xdp");
            assert_eq!(p, rust_ext(&btf, &ext, "xdp"), "{} mutation {i}", c.name);
            if p.starts_with("Ok") {
                ok += 1;
            } else if let Some(e) = p.split('"').nth(1) {
                errors.insert(
                    e.chars()
                        .take_while(|c| !c.is_ascii_digit())
                        .collect::<String>(),
                );
            }
        }
    }
    assert!(objects > 20, "{objects} objects");
    assert!(ok > 100, "{ok}");
    assert!(errors.len() >= 8, "{errors:?}");
}

#[test]
fn port_matches_rust_on_local_types_of_host_btf() {
    let Ok(blob) = std::fs::read("/sys/kernel/btf/vmlinux") else {
        eprintln!("no /sys/kernel/btf/vmlinux: skipped");
        return;
    };
    let b = Btf::parse(&blob).unwrap();
    let rb = rust::btf::Btf::parse(&blob).unwrap();
    let mut rng = Rng(0x0010_ca17_1041);
    for _ in 0..3000 {
        let id = rng.below(b.len() as u64 + 2) as u32;
        assert_eq!(
            format!("{:?}", local_type(&b, id)),
            format!("{:?}", rust::btf_ext::local_type(&rb, id)),
            "type {id}"
        );
    }
    assert_eq!(
        format!("{:?}", maps_from_btf(&b)),
        format!("{:?}", rust::btf_ext::maps_from_btf(&rb))
    );
}
