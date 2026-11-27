//! Cross-target object and packet tests for the experimental AArch64 encoder.

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;

use object::{Architecture, BinaryFormat, Object, ObjectSection, SectionKind};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use vesper::{check, native, parser};

const RET: u32 = 0xd65f_03c0;
const CELL_BITS: u64 = u64::BITS as u64;
const CONSTANT: u64 = 0;
const ADD: u64 = 1;
const SHL: u64 = 9;
const COMPARE: u64 = 12;
const SELECT: u64 = 14;
const CALL: u64 = 38;
const ADD_IMMEDIATE: u64 = 52;
const AND_IMMEDIATE: u64 = 55;
const OR_IMMEDIATE: u64 = 56;
const XOR_IMMEDIATE: u64 = 57;
const COMPARE_IMMEDIATE: u64 = 58;
const EQUAL: u64 = 0;

#[test]
fn aarch64_leaf_encodings_and_operand_validation() {
    const ADD_X0_X0_X1: u32 = 0x8b01_0000;
    const MOVN_X0_0: u32 = 0x9280_0000;
    const MOV_X0_X7: u32 = 0xaa07_03e0;
    let as_bytes = |first: u32| [first.to_le_bytes(), RET.to_le_bytes()].concat();

    assert_eq!(
        binding::aarch64_integer_leaf(&[2, 2, ADD, 0, 1, CELL_BITS]),
        as_bytes(ADD_X0_X0_X1),
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[0, 0, CONSTANT, u64::MAX, 0, 0]),
        as_bytes(MOVN_X0_0),
    );
    assert_eq!(binding::aarch64_integer_leaf(&[8, 7]), as_bytes(MOV_X0_X7));
    assert!(binding::aarch64_integer_leaf(&[9, 8]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[2, 2, ADD, 0, 2, CELL_BITS]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[2, 2, ADD, 0, 1, u32::BITS as u64]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[0, 0, CALL, 0, 0, 0]).is_empty());
}

#[test]
fn aarch64_select_uses_comparison_flags_directly() {
    const CMP_X0_X1: u32 = 0xeb01_001f;
    const CMP_X0_4096: u32 = 0xf140_041f;
    const CMN_X0_1: u32 = 0xb100_041f;
    const CSEL_X0_X2_X3_EQ: u32 = 0x9a83_0040;
    for (op, right, compare) in [
        (COMPARE, 1, CMP_X0_X1),
        (COMPARE_IMMEDIATE, 4096, CMP_X0_4096),
        (COMPARE_IMMEDIATE, u64::from(u32::MAX), CMN_X0_1),
    ] {
        let words = [4, 5, op, 0, right, EQUAL, SELECT, 4, 2, 3];
        let instructions = [compare, CSEL_X0_X2_X3_EQ, RET];
        let expected: Vec<_> = instructions
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(binding::aarch64_integer_leaf(&words), expected);
    }
}

#[test]
fn aarch64_uses_short_immediate_encodings() {
    const ADD_X0_X0_4096: u32 = 0x9140_0400;
    const SUB_X0_X0_16773120: u32 = 0xd17f_fc00;
    const AND_X0_X0_COMPLEMENT_OF_128: u32 = 0x9278_f800;
    const ORR_X0_X0_SIGN_EXTENDED_I32_MIN: u32 = 0xb261_8000;
    const EOR_X0_X0_I32_MAX: u32 = 0xd240_7800;
    const MOV_ALTERNATING_BITS: u32 = 0xb201_f3e0;
    const MOV_REPEATED_BYTE_MASK: u32 = 0xb200_9fe0;
    const MOV_WRAPPED_RUN: u32 = 0xb241_a7e0;
    let as_bytes = |word: u32| [word.to_le_bytes(), RET.to_le_bytes()].concat();

    for (op, imm, encoded) in [
        (ADD_IMMEDIATE, 4096, ADD_X0_X0_4096),
        (ADD_IMMEDIATE, -16_773_120, SUB_X0_X0_16773120),
        (AND_IMMEDIATE, -129, AND_X0_X0_COMPLEMENT_OF_128),
        (OR_IMMEDIATE, i32::MIN, ORR_X0_X0_SIGN_EXTENDED_I32_MIN),
        (XOR_IMMEDIATE, i32::MAX, EOR_X0_X0_I32_MAX),
    ] {
        let input = [1, 1, op, 0, u64::from(imm as u32), CELL_BITS];
        assert_eq!(binding::aarch64_integer_leaf(&input), as_bytes(encoded));
    }
    for (value, encoded) in [
        (0xaaaa_aaaa_aaaa_aaaa, MOV_ALTERNATING_BITS),
        (0x00ff_00ff_00ff_00ff, MOV_REPEATED_BYTE_MASK),
        (0x8000_01ff_ffff_ffff, MOV_WRAPPED_RUN),
    ] {
        let input = [0, 0, CONSTANT, value, 0, 0];
        assert_eq!(binding::aarch64_integer_leaf(&input), as_bytes(encoded));
    }
    assert_eq!(
        binding::aarch64_integer_leaf(&[1, 1, SHL, 0, CELL_BITS, CELL_BITS]),
        RET.to_le_bytes(),
    );
}

#[test]
fn aarch64_leaves_reach_elf_and_macho_object_emission() {
    const LEAF: &str = "
        fn main(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {
            wrapping_add(wrapping_mul(a, h), b ^ c ^ d ^ e ^ f ^ g)
        }
    ";
    const FLOAT: &str = "fn main(x: f64) -> f64 { x + x }";
    const TRIPLES: [(&str, BinaryFormat); 2] = [
        ("aarch64-unknown-linux-gnu", BinaryFormat::Elf),
        ("aarch64-apple-darwin", BinaryFormat::MachO),
    ];
    static SELECTED: AtomicUsize = AtomicUsize::new(0);
    let checked = |source| {
        let parsed = parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        checked
    };
    let leaf = checked(LEAF);
    let float = checked(FLOAT);
    let compile = |checked: &check::Checked, triple| {
        native::object_with_options(&checked.world, "main", Some(triple), true)
            .unwrap()
            .0
    };
    let baselines = TRIPLES.map(|(triple, _)| (compile(&leaf, triple), compile(&float, triple)));
    assert!(
        native::machine::install_aarch64_encoder(|words| {
            let bytes = binding::aarch64_integer_leaf(words);
            if bytes.is_empty() {
                None
            } else {
                SELECTED.fetch_add(1, Relaxed);
                Some(bytes)
            }
        })
        .is_ok()
    );
    for ((triple, format), (baseline, float_baseline)) in TRIPLES.into_iter().zip(baselines) {
        let bytes = compile(&leaf, triple);
        assert!(
            bytes != baseline,
            "{triple} did not select the machine emitter"
        );
        assert!(
            compile(&float, triple) == float_baseline,
            "{triple} changed the fallback object"
        );

        let file = object::File::parse(bytes.as_slice()).unwrap();
        assert_eq!(file.architecture(), Architecture::Aarch64);
        assert_eq!(file.format(), format);
        let text = file
            .sections()
            .find(|section| section.kind() == SectionKind::Text)
            .unwrap();
        assert!(text.data().unwrap().ends_with(&RET.to_le_bytes()));
    }
    assert_eq!(SELECTED.load(Relaxed), TRIPLES.len());
}
