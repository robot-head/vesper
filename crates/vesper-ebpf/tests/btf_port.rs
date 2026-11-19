//! The port's BTF reader against the Rust reader, result for result: a
//! blob with every kind, split BTF on top of it, the host's vmlinux and
//! module BTF when present, and thousands of mutated and truncated copies.
//! Both builds must give the same error message, or the same types, names,
//! by-name index and base length. Port wave P3a lane 6 (#1041).
#![cfg(feature = "vesper-port")]

mod common;

use common::Rng;
use vesper_ebpf::btf::{self, Btf, BtfBuilder};
use vesper_ebpf_rust as rust;

/// Everything the reader produced, as text: the types, every name of every
/// type and its by-name ids, and the base length.
fn summary_port(b: &Btf) -> String {
    let mut s = format!("{:?} base {}\n", b.types, b.base_len());
    for t in &b.types {
        let n = b.name(t.name_off());
        s += &format!("{n:?} {:?}\n", b.by_name(n));
    }
    s
}

fn summary_rust(b: &rust::btf::Btf) -> String {
    let mut s = format!("{:?} base {}\n", b.types, b.base_len());
    for t in &b.types {
        let n = b.name(t.name_off());
        s += &format!("{n:?} {:?}\n", b.by_name(n));
    }
    s
}

fn port(blob: &[u8], base: Option<&Btf>) -> String {
    let r = match base {
        Some(b) => Btf::parse_split(blob, b),
        None => Btf::parse(blob),
    };
    match r {
        Ok(b) => summary_port(&b),
        Err(e) => format!("error {e}"),
    }
}

fn rust_side(blob: &[u8], base: Option<&rust::btf::Btf>) -> String {
    let r = match base {
        Some(b) => rust::btf::Btf::parse_split(blob, b),
        None => rust::btf::Btf::parse(blob),
    };
    match r {
        Ok(b) => summary_rust(&b),
        Err(e) => format!("error {e}"),
    }
}

/// A blob with every kind, bitfields, signed and 64-bit enums, and names
/// that need every UTF-8 length.
fn every_kind() -> Vec<u8> {
    let mut b = BtfBuilder::new();
    let int = b.int("int", 4, true);
    let u8t = b.int("unsigned char", 1, false);
    let p = b.ptr(int);
    let arr = b.array(u8t, int, 16);
    let s = b.structure(
        "sk_buff",
        false,
        24,
        &[("len", int, 0), ("data", p, 64), ("cb", arr, 128)],
    );
    b.aggregate_bits("flags", false, 4, &[("a", int, 0, 3), ("b", int, 3, 5)]);
    b.structure("u", true, 8, &[("x", int, 0), ("y", p, 0)]);
    b.enumeration("e", 4, &[("A", 0), ("B", -1)]);
    b.enumeration("ü_enum", 4, &[("Ω", 7), ("中", 8), ("𝄞", 9)]);
    let fwd_name = b.str("fwd_union");
    b.header(fwd_name, btf::KIND_FWD, 0, true, 0);
    let td_name = b.str("u32_t");
    let td = b.header(td_name, btf::KIND_TYPEDEF, 0, false, int);
    b.header(0, btf::KIND_VOLATILE, 0, false, td);
    b.header(0, btf::KIND_CONST, 0, false, td);
    b.header(0, btf::KIND_RESTRICT, 0, false, p);
    let proto = b.func_proto(int, &[("ctx", p), ("n", int)]);
    b.func("prog", proto, btf::FUNC_GLOBAL);
    let v = b.var("counter", int, btf::VAR_GLOBAL_ALLOCATED);
    b.datasec(".data", 4, &[(v, 0, 4)]);
    let f = b.str("double");
    b.header(f, btf::KIND_FLOAT, 0, false, 8);
    let tag = b.str("user");
    let dt = b.header(tag, btf::KIND_DECL_TAG, 0, false, s);
    let _ = dt;
    b.word(u32::MAX); // component -1
    b.header(tag, btf::KIND_TYPE_TAG, 0, false, p);
    let e64 = b.str("big");
    b.header(e64, btf::KIND_ENUM64, 2, true, 8);
    let x = b.str("X");
    b.word(x);
    b.word(0xffff_fffe);
    b.word(0xffff_ffff);
    let y = b.str("Y");
    b.word(y);
    b.word(1);
    b.word(2);
    // a pointer that carries a name word: not a name (BtfType::name_off)
    let junk = b.str("ignored");
    b.header(junk, btf::KIND_PTR, 0, false, s);
    b.finish()
}

fn split_on(base: &Btf) -> Vec<u8> {
    let mut b = BtfBuilder::split(base);
    let int = base.by_name("int")[0];
    let s = b.structure(
        "module_struct",
        false,
        8,
        &[("len", int, 0), ("again", int, 32)],
    );
    b.ptr(s);
    b.structure("sk_buff", false, 4, &[("len", int, 0)]);
    b.finish()
}

fn mutate(rng: &mut Rng, blob: &[u8]) -> Vec<u8> {
    let mut m = blob.to_vec();
    match rng.below(6) {
        0 => m.truncate(rng.below(m.len() as u64 + 1) as usize),
        1 => {
            // a header field
            let at = 4 + 4 * rng.below(5) as usize;
            let v = rng.pick(&[0u32, 1, 23, 24, 25, 4096, u32::MAX, m.len() as u32]);
            m[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        2 => {
            // the version or magic
            let at = rng.below(3) as usize;
            m[at] = rng.next() as u8;
        }
        _ => {
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(m.len() as u64) as usize;
                m[at] = match rng.below(4) {
                    0 => 0,
                    1 => 0xff,
                    2 => m[at] ^ (1 << rng.below(8)),
                    _ => rng.next() as u8,
                };
            }
        }
    }
    m
}

#[test]
fn port_matches_rust_on_every_kind_and_split_btf() {
    let blob = every_kind();
    assert_eq!(port(&blob, None), rust_side(&blob, None));
    assert!(
        !port(&blob, None).starts_with("error"),
        "{}",
        port(&blob, None)
    );
    let base = Btf::parse(&blob).unwrap();
    let rbase = rust::btf::Btf::parse(&blob).unwrap();
    let split = split_on(&base);
    assert_eq!(port(&split, Some(&base)), rust_side(&split, Some(&rbase)));
    assert!(!port(&split, Some(&base)).starts_with("error"));
    // split on split is refused by both
    let sb = Btf::parse_split(&split, &base).unwrap();
    let rsb = rust::btf::Btf::parse_split(&split, &rbase).unwrap();
    assert_eq!(port(&split, Some(&sb)), rust_side(&split, Some(&rsb)));
}

#[test]
fn port_matches_rust_on_mutated_btf() {
    let mut rng = Rng(0x00b7_f00d_1041);
    let blob = every_kind();
    let base = Btf::parse(&blob).unwrap();
    let rbase = rust::btf::Btf::parse(&blob).unwrap();
    let split = split_on(&base);
    let mut errors = std::collections::BTreeSet::new();
    for i in 0..6000 {
        let (m, on_base) = if i % 3 == 2 {
            (mutate(&mut rng, &split), true)
        } else {
            (mutate(&mut rng, &blob), false)
        };
        let (p, r) = if on_base {
            (port(&m, Some(&base)), rust_side(&m, Some(&rbase)))
        } else {
            (port(&m, None), rust_side(&m, None))
        };
        assert_eq!(p, r, "mutation {i}");
        if let Some(e) = p.strip_prefix("error ") {
            let kind: String = e.chars().take_while(|c| !c.is_ascii_digit()).collect();
            errors.insert(kind);
        }
    }
    // every error the reader can give was reached
    assert_eq!(errors.len(), 11, "{errors:?}");
}

#[test]
fn port_matches_rust_on_host_btf() {
    let Ok(vmlinux) = std::fs::read("/sys/kernel/btf/vmlinux") else {
        eprintln!("no /sys/kernel/btf/vmlinux: skipped");
        return;
    };
    assert_eq!(port(&vmlinux, None), rust_side(&vmlinux, None));
    let base = Btf::parse(&vmlinux).unwrap();
    let rbase = rust::btf::Btf::parse(&vmlinux).unwrap();
    let mut modules = 0;
    for e in std::fs::read_dir("/sys/kernel/btf").unwrap().flatten() {
        if e.file_name() == "vmlinux" || modules >= 8 {
            continue;
        }
        let Ok(blob) = std::fs::read(e.path()) else {
            continue;
        };
        assert_eq!(
            port(&blob, Some(&base)),
            rust_side(&blob, Some(&rbase)),
            "{:?}",
            e.path()
        );
        modules += 1;
    }
}
