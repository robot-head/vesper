//! The port's CO-RE matcher against the Rust matcher, result for result:
//! random relocations (every kind, valid and invalid access paths,
//! flavoured, renamed and reshaped local types) against random target BTF
//! with anonymous members, bitfields, arrays, modifiers and forward
//! declarations, against split module BTF, and against the host's vmlinux
//! when present; plus `patch` on random instructions. Local values,
//! candidates, target values and errors must agree exactly. Port wave P3a
//! lane 6 (#1041).
#![cfg(feature = "vesper-port")]

mod common;

use common::Rng;
use vesper_ebpf::btf::{self, Btf, BtfBuilder, BtfType};
use vesper_ebpf::btf_ext::local_type;
use vesper_ebpf::core::{Bitfield, CoreKind, CoreSpec, LocalMember, LocalType, patch};
use vesper_ebpf::insn::RawInsn;
use vesper_ebpf_rust as rust;

const NAMES: [&str; 10] = [
    "task",
    "task___v2",
    "sock",
    "sock___old",
    "regs",
    "flags",
    "len",
    "data",
    "x",
    "",
];

fn to_rust(t: &LocalType) -> rust::core::LocalType {
    use rust::core::LocalType as R;
    match t {
        LocalType::Void => R::Void,
        LocalType::Int { name, size, signed } => R::Int {
            name: name.clone(),
            size: *size,
            signed: *signed,
        },
        LocalType::Ptr(to) => R::Ptr(Box::new(to_rust(to))),
        LocalType::Array { elem, len } => R::Array {
            elem: Box::new(to_rust(elem)),
            len: *len,
        },
        LocalType::Struct {
            name,
            union,
            size,
            members,
        } => R::Struct {
            name: name.clone(),
            union: *union,
            size: *size,
            members: members
                .iter()
                .map(|m| rust::core::LocalMember {
                    name: m.name.clone(),
                    ty: to_rust(&m.ty),
                    offset: m.offset,
                    bitfield: m.bitfield.map(|b| rust::core::Bitfield {
                        bit_offset: b.bit_offset,
                        bits: b.bits,
                    }),
                })
                .collect(),
        },
        LocalType::Enum { name, size, values } => R::Enum {
            name: name.clone(),
            size: *size,
            values: values.clone(),
        },
    }
}

fn rust_spec(s: &CoreSpec) -> rust::core::CoreSpec {
    rust::core::CoreSpec {
        kind: rust::core::CoreKind::from_code(s.kind.code()).unwrap(),
        root: to_rust(&s.root),
        access: s.access.clone(),
    }
}

/// A random target: ints, enums, structs and unions (some anonymous, some
/// with bitfields), arrays, pointers, typedefs, cv modifiers and forward
/// declarations, over a small name pool so that names collide.
fn random_btf(rng: &mut Rng, b: &mut BtfBuilder, first: u32, n: usize) -> Vec<u32> {
    let mut ids: Vec<u32> = (1..first).collect();
    for _ in 0..n {
        let name = rng.pick(&NAMES);
        let pick = |rng: &mut Rng, ids: &[u32]| {
            if ids.is_empty() || rng.chance(10) {
                0
            } else {
                ids[rng.below(ids.len() as u64) as usize]
            }
        };
        let id = match rng.below(11) {
            0 | 1 => b.int(
                rng.pick(&["int", "long", "char", "u32", "x"]),
                rng.pick(&[1, 2, 4, 8]),
                rng.chance(50),
            ),
            2 => {
                let vs: Vec<(&str, i64)> = (0..rng.below(4))
                    .map(|i| (rng.pick(&NAMES[..9]), i as i64 - 1))
                    .collect();
                b.enumeration(name, rng.pick(&[1, 4, 8]), &vs)
            }
            3..=5 => {
                let m = rng.below(5) as usize;
                let mut ms = Vec::new();
                let mut bit = 0;
                for _ in 0..m {
                    let width = if rng.chance(25) {
                        1 + rng.below(12) as u32
                    } else {
                        0
                    };
                    ms.push((rng.pick(&NAMES), pick(rng, &ids), bit, width));
                    bit += if width != 0 {
                        width
                    } else {
                        8 * rng.pick(&[1, 4, 8])
                    };
                    if rng.chance(10) {
                        bit += 3;
                    }
                }
                b.aggregate_bits(name, rng.chance(25), bit.div_ceil(8).max(1), &ms)
            }
            6 => {
                let e = pick(rng, &ids);
                let i = pick(rng, &ids);
                b.array(e, i, rng.below(6) as u32)
            }
            7 => {
                let t = pick(rng, &ids);
                b.ptr(t)
            }
            8 => {
                let t = pick(rng, &ids);
                let n = b.str(name);
                b.header(n, btf::KIND_TYPEDEF, 0, false, t)
            }
            9 => {
                let t = pick(rng, &ids);
                let k = rng.pick(&[btf::KIND_CONST, btf::KIND_VOLATILE, btf::KIND_TYPE_TAG]);
                b.header(0, k, 0, false, t)
            }
            _ => {
                let n = b.str(name);
                b.header(n, btf::KIND_FWD, 0, rng.chance(50), 0)
            }
        };
        ids.push(id);
    }
    ids
}

/// A local type for relocations: one of the target's own types (as an
/// object would describe it), reshaped at random.
fn random_local(rng: &mut Rng, target: &Btf, depth: u32) -> LocalType {
    for _ in 0..8 {
        let id = rng.below(target.len() as u64) as u32;
        if let Ok(t) = local_type(target, id) {
            return reshape(rng, t, depth);
        }
    }
    LocalType::int(rng.pick(&NAMES), rng.pick(&[1, 4, 8]), rng.chance(50))
}

fn reshape(rng: &mut Rng, t: LocalType, depth: u32) -> LocalType {
    let rename = |rng: &mut Rng, n: String| match rng.below(8) {
        0 => format!("{n}___flavor"),
        1 => rng.pick(&NAMES).to_owned(),
        _ => n,
    };
    match t {
        LocalType::Struct {
            name,
            union,
            size,
            mut members,
        } => {
            if rng.chance(30) && !members.is_empty() {
                let i = rng.below(members.len() as u64) as usize;
                members.remove(i);
            }
            for m in &mut members {
                m.name = rename(rng, std::mem::take(&mut m.name));
                if depth < 3 && rng.chance(20) {
                    m.ty = reshape(
                        rng,
                        std::mem::replace(&mut m.ty, LocalType::Void),
                        depth + 1,
                    );
                }
                if rng.chance(5) {
                    m.bitfield = Some(Bitfield {
                        bit_offset: m.offset * 8 + rng.below(8) as u32,
                        bits: 1 + rng.below(70) as u32,
                    });
                }
            }
            if rng.chance(10) {
                members.push(LocalMember {
                    name: rng.pick(&NAMES).to_owned(),
                    ty: LocalType::int("int", 4, true),
                    offset: rng.below(16) as u32,
                    bitfield: None,
                });
            }
            LocalType::Struct {
                name: rename(rng, name),
                union: if rng.chance(5) { !union } else { union },
                size: if rng.chance(10) { size + 8 } else { size },
                members,
            }
        }
        LocalType::Enum {
            name,
            size,
            mut values,
        } => {
            if rng.chance(20) {
                values.push((rng.pick(&NAMES).to_owned(), -5));
            }
            LocalType::Enum {
                name: rename(rng, name),
                size,
                values,
            }
        }
        LocalType::Int { name, size, signed } => LocalType::Int {
            name: rename(rng, name),
            size: if rng.chance(10) { size * 2 } else { size },
            signed: if rng.chance(10) { !signed } else { signed },
        },
        LocalType::Ptr(to) if depth < 3 => LocalType::Ptr(Box::new(reshape(rng, *to, depth + 1))),
        LocalType::Array { elem, len } if depth < 3 => LocalType::Array {
            elem: Box::new(reshape(rng, *elem, depth + 1)),
            len: if rng.chance(10) { len + 1 } else { len },
        },
        other => other,
    }
}

/// A random access path: usually valid for the local type, sometimes not.
fn random_access(rng: &mut Rng, kind: CoreKind, root: &LocalType) -> Vec<u32> {
    let code = kind.code();
    // (an enumerator kind indexes `access[0]` unchecked in the Rust build)
    if code != 10 && code != 11 && rng.chance(3) {
        return Vec::new();
    }
    if (6..=9).contains(&code) || code == 12 {
        return vec![if rng.chance(5) { 1 } else { 0 }];
    }
    if code == 10 || code == 11 {
        let n = match root {
            LocalType::Enum { values, .. } => values.len() as u64,
            _ => 1,
        };
        return vec![rng.below(n + 1) as u32];
    }
    let mut access = vec![if rng.chance(5) { 1 } else { 0 }];
    let mut cur = root;
    for _ in 0..1 + rng.below(4) {
        match cur {
            LocalType::Struct { members, .. } if !members.is_empty() => {
                let extra = u64::from(rng.chance(5));
                let i = rng.below(members.len() as u64 + extra) as usize;
                access.push(i as u32);
                match members.get(i) {
                    Some(m) => cur = &m.ty,
                    None => break,
                }
            }
            LocalType::Array { elem, len } => {
                access.push(rng.below(u64::from(*len) + 1) as u32);
                cur = elem;
            }
            _ => {
                if rng.chance(20) {
                    access.push(0);
                }
                break;
            }
        }
    }
    access
}

fn random_spec(rng: &mut Rng, target: &Btf) -> CoreSpec {
    let kind = CoreKind::from_code(rng.below(13) as u32).unwrap();
    let root = if rng.chance(15) {
        LocalType::enumeration(rng.pick(&NAMES), 4, &[("x", 1), ("len", -1)])
    } else {
        random_local(rng, target, 0)
    };
    let access = random_access(rng, kind, &root);
    CoreSpec { kind, root, access }
}

fn compare(
    rng: &mut Rng,
    vmlinux: &Btf,
    rvm: &rust::btf::Btf,
    modules: &[Btf],
    rmods: &[rust::btf::Btf],
    n: usize,
    seen: &mut std::collections::BTreeSet<String>,
) {
    let mods: Vec<&Btf> = modules.iter().collect();
    let rm: Vec<&rust::btf::Btf> = rmods.iter().collect();
    let mut ok = 0;
    for i in 0..n {
        let source = if !modules.is_empty() && rng.chance(30) {
            &modules[rng.below(modules.len() as u64) as usize]
        } else {
            vmlinux
        };
        let spec = random_spec(rng, source);
        let rs = rust_spec(&spec);
        let id = if rng.chance(50) {
            Some(rng.below(100) as u32)
        } else {
            None
        };
        let p = format!(
            "{:?} {:?} {:?} {:?} {:?}",
            spec.local_value_with(id),
            spec.candidates(vmlinux, 0),
            spec.target_value(vmlinux),
            spec.target_value_with(vmlinux, &mods, id),
            modules
                .iter()
                .map(|m| spec.candidates(m, m.base_len()))
                .collect::<Vec<_>>(),
        );
        let r = format!(
            "{:?} {:?} {:?} {:?} {:?}",
            rs.local_value_with(id),
            rs.candidates(rvm, 0),
            rs.target_value(rvm),
            rs.target_value_with(rvm, &rm, id),
            rmods
                .iter()
                .map(|m| rs.candidates(m, m.base_len()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(p, r, "case {i}: {spec:?}");
        if p.contains("Ok(") {
            ok += 1;
        }
        let outcome = format!("{:?}", spec.target_value_with(vmlinux, &mods, id));
        seen.insert(if outcome.starts_with("Ok") {
            "Ok".to_owned()
        } else {
            outcome
                .split(['(', ' '])
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        });
    }
    assert!(ok > n / 4, "{ok} of {n} computed a value");
}

#[test]
fn port_matches_rust_on_random_relocations() {
    let mut rng = Rng(0x00c0_7e00_1041);
    let mut seen = std::collections::BTreeSet::new();
    for round in 0..30 {
        let mut b = BtfBuilder::new();
        random_btf(&mut rng, &mut b, 1, 40);
        let blob = b.finish();
        let vm = Btf::parse(&blob).unwrap();
        let rvm = rust::btf::Btf::parse(&blob).unwrap();
        let mut modules = Vec::new();
        let mut rmods = Vec::new();
        for _ in 0..round % 3 {
            let mut mb = BtfBuilder::split(&vm);
            random_btf(&mut rng, &mut mb, vm.len() as u32, 15);
            let m = mb.finish();
            modules.push(Btf::parse_split(&m, &vm).unwrap());
            rmods.push(rust::btf::Btf::parse_split(&m, &rvm).unwrap());
        }
        compare(&mut rng, &vm, &rvm, &modules, &rmods, 200, &mut seen);
    }
    // values and every error the matcher gives
    for want in [
        "Ok",
        "Err NoMatch",
        "Err Ambiguous",
        "Err Bitfield",
        "Err BadAccess",
    ] {
        assert!(seen.contains(want), "{want} not reached: {seen:?}");
    }
}

#[test]
fn port_matches_rust_on_host_relocations() {
    let Ok(blob) = std::fs::read("/sys/kernel/btf/vmlinux") else {
        eprintln!("no /sys/kernel/btf/vmlinux: skipped");
        return;
    };
    let vm = Btf::parse(&blob).unwrap();
    let rvm = rust::btf::Btf::parse(&blob).unwrap();
    // relocations against the kernel's own structs, reshaped
    let mut rng = Rng(0x0011_05e7_1041);
    let structs: Vec<u32> = (0..vm.len() as u32)
        .filter(
            |&i| matches!(vm.get(i), Some(BtfType::Struct { members, .. }) if !members.is_empty()),
        )
        .collect();
    let mut ok = 0;
    for i in 0..400 {
        let id = structs[rng.below(structs.len() as u64) as usize];
        let Ok(t) = local_type(&vm, id) else {
            continue;
        };
        let root = reshape(&mut rng, t, 0);
        let kind = CoreKind::from_code(rng.below(13) as u32).unwrap();
        let access = random_access(&mut rng, kind, &root);
        let spec = CoreSpec { kind, root, access };
        let rs = rust_spec(&spec);
        let p = format!(
            "{:?} {:?}",
            spec.local_value_with(Some(5)),
            spec.target_value(&vm)
        );
        let r = format!(
            "{:?} {:?}",
            rs.local_value_with(Some(5)),
            rs.target_value(&rvm)
        );
        assert_eq!(p, r, "case {i}: {spec:?}");
        ok += usize::from(p.contains("Ok("));
    }
    assert!(ok > 100, "{ok}");
}

#[test]
fn port_matches_rust_on_patch() {
    let mut rng = Rng(0x00a7_c400_1041);
    let codes = [
        0x07u8, 0x04, 0xb7, 0x0f, 0x61, 0x62, 0x63, 0x79, 0x18, 0x00, 0x85, 0x95, 0xbf,
    ];
    for i in 0..5000 {
        let n = 1 + rng.below(3) as usize;
        let insns: Vec<RawInsn> = (0..n)
            .map(|_| {
                let (r1, r2) = (rng.next() as i16, rng.next() as i32);
                RawInsn::new(
                    rng.pick(&codes),
                    0,
                    0,
                    rng.pick(&[0, 8, -8, 32767, -32768, r1]),
                    rng.pick(&[0, 4, -1, i32::MIN, i32::MAX, r2]),
                )
            })
            .collect();
        let slot = rng.below(n as u64 + 1) as usize;
        let pick = |rng: &mut Rng, insns: &[RawInsn]| match rng.below(5) {
            0 => insns.get(slot).map_or(0, |i| i.imm as u32 as u64),
            1 => insns.get(slot).map_or(0, |i| i.imm as i64 as u64),
            2 => insns.get(slot).map_or(0, |i| i.off as i64 as u64),
            3 => rng.pick(&[0, 1, 8, 0xffff_ffff, u64::MAX, 1 << 40]),
            _ => rng.next(),
        };
        let expected = pick(&mut rng, &insns);
        let value = rng.pick(&[
            0,
            4,
            32767,
            32768,
            0x7fff_ffff,
            0x8000_0000,
            0xffff_ffff,
            1 << 33,
            u64::MAX,
            (-8i64) as u64,
        ]);
        let mut p = insns.clone();
        let mut r: Vec<rust::insn::RawInsn> = insns
            .iter()
            .map(|i| rust::insn::RawInsn {
                code: i.code,
                dst: i.dst,
                src: i.src,
                off: i.off,
                imm: i.imm,
            })
            .collect();
        let pr = format!("{:?}", patch(&mut p, slot, expected, value));
        let rr = format!("{:?}", rust::core::patch(&mut r, slot, expected, value));
        assert_eq!(pr, rr, "case {i}");
        let pv: Vec<(i16, i32)> = p.iter().map(|i| (i.off, i.imm)).collect();
        let rv: Vec<(i16, i32)> = r.iter().map(|i| (i.off, i.imm)).collect();
        assert_eq!(pv, rv, "case {i}");
    }
}
