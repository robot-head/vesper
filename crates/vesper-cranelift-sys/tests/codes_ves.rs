//! `packages/vesper-cranelift/src/codes.ves` spells the shim's integer
//! codes (src/codes.rs) as value enums for the codegen packages: each
//! table's enum has one member per entry, named the CamelCase of the
//! entry, with the same number.
mod support;

use std::collections::BTreeMap;
use support::*;
use vesper_cranelift_sys::codes;

/// `enum Name: _ { Member = n, .. }` blocks of `text`, by enum name.
fn value_enums(text: &str) -> BTreeMap<String, BTreeMap<String, u32>> {
    let mut out = BTreeMap::new();
    let mut rest = text;
    while let Some(at) = rest.find("pub enum ") {
        rest = &rest[at + "pub enum ".len()..];
        let name = rest[..rest.find(':').unwrap()].trim().to_string();
        let body = &rest[rest.find('{').unwrap() + 1..rest.find('}').unwrap()];
        let mut members = BTreeMap::new();
        for line in body.lines() {
            let line = line.trim().trim_end_matches(',');
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let (member, value) = line.split_once(" = ").unwrap();
            members.insert(member.to_string(), value.parse().unwrap());
        }
        out.insert(name, members);
    }
    out
}

fn camel(op: &str) -> String {
    op.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn codes_ves_matches_the_rust_tables() {
    let text =
        std::fs::read_to_string(repo().join("packages/vesper-cranelift/src/codes.ves")).unwrap();
    let enums = value_enums(&text);
    let tables: &[(&str, &[(u32, &str)])] = &[
        ("Bin", codes::BINARY),
        ("BinImm", codes::BINARY_IMM),
        ("Un", codes::UNARY),
        ("Cv", codes::CONVERT),
        ("Overflow", codes::OVERFLOW),
        ("Load", codes::LOAD),
        ("Store", codes::STORE),
        ("Ternary", codes::TERNARY),
        ("IntCc", codes::INTCC),
        ("FloatCc", codes::FLOATCC),
        ("Linkage", codes::LINKAGE),
        ("CallConv", codes::CALLCONV),
        ("Trap", codes::TRAP),
        ("MemFlag", codes::MEMFLAG),
        ("Ext", codes::EXT),
        ("RelocTarget", codes::RELOC_TARGET),
    ];
    for (name, table) in tables {
        let mut want: BTreeMap<String, u32> = table
            .iter()
            .map(|(code, op)| {
                // `None` would shadow `Option::None` inside the package.
                let member = if *name == "Ext" && *op == "none" {
                    "Unextended".to_string()
                } else {
                    camel(op)
                };
                (member, *code)
            })
            .collect();
        if *name == "Trap" {
            // 16 + n is `TrapCode::user(n)` (`codes::trap`).
            assert!(codes::trap(16 + 1).is_some() && codes::trap(15).is_none());
            want.insert("UserBase".to_string(), 16);
        }
        assert_eq!(enums.get(*name), Some(&want), "enum {name}");
    }
    let lanes: BTreeMap<String, u32> = codes::TYPE_LANES
        .iter()
        .map(|(code, lane, _)| (lane.to_string(), *code))
        .collect();
    assert_eq!(enums.get("TyCode"), Some(&lanes), "enum TyCode");
    // `codes::ty`: the lane count's log2 sits in bits 8..16.
    assert_eq!(
        codes::ty(1 | (4 << 8)),
        Some(cranelift_codegen::ir::types::I8X16)
    );
    let shape: BTreeMap<String, u32> =
        [("Shift".to_string(), 8), ("LaneMask".to_string(), 255)].into();
    assert_eq!(enums.get("Lanes"), Some(&shape), "enum Lanes");
    assert_eq!(enums.len(), tables.len() + 2, "an enum without a table");
}
