//! Cross-target object and packet tests for the experimental AArch64 encoder.

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/native_backend.rs"]
mod binding;

use object::{Architecture, BinaryFormat, Object, ObjectSection, SectionKind};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use vesper::{check, native, parser};

const RET: u32 = 0xd65f_03c0;
const BYTE_BITS: u64 = u8::BITS as u64;
const DWORD_BITS: u64 = u32::BITS as u64;
const CELL_BITS: u64 = u64::BITS as u64;
const NO_VALUE: u64 = u64::MAX;
const TRAP_VALUE: u64 = NO_VALUE - 1;
const CONSTANT: u64 = 0;
const ADD: u64 = 1;
const SHL: u64 = 9;
const COMPARE: u64 = 12;
const SELECT: u64 = 14;
const TRUNCATE: u64 = 15;
const ZERO_EXTEND: u64 = 16;
const SIGN_EXTEND: u64 = 17;
const LOAD: u64 = 23;
const STORE_BYTE: u64 = 24;
const STORE_WORD: u64 = 25;
const BRANCH_ZERO: u64 = 28;
const EARLY_RETURN: u64 = 29;
const STACK_SLOT: u64 = 30;
const STACK_LOAD: u64 = 32;
const STACK_STORE_QWORD: u64 = 36;
const RETURN_PAIR: u64 = 37;
const CALL: u64 = 38;
const CALL_ARGUMENT: u64 = 39;
const CALL_VOID: u64 = 40;
const CALL_PAIR: u64 = 41;
const CALL_HIGH: u64 = 42;
const TRAP: u64 = 45;
const SIGNED_DIV: u64 = 46;
const UNSIGNED_DIV: u64 = 47;
const SIGNED_REM: u64 = 48;
const SIGNED_MUL_HIGH: u64 = 50;
const UNSIGNED_MUL_HIGH: u64 = 51;
const ADD_IMMEDIATE: u64 = 52;
const AND_IMMEDIATE: u64 = 55;
const OR_IMMEDIATE: u64 = 56;
const XOR_IMMEDIATE: u64 = 57;
const COMPARE_IMMEDIATE: u64 = 58;
const COMPARE_BYTE_IMMEDIATE: u64 = 59;
const BLOCK_LABEL: u64 = 62;
const JUMP_LABEL: u64 = 64;
const BLOCK_RETURN: u64 = 66;
const BRANCH_BLOCK: u64 = 67;
const BRANCH_TABLE: u64 = 75;
const BRANCH_TABLE_TARGET: u64 = 76;
const EQUAL: u64 = 0;
const SIGNED_LESS: u64 = 2;
const UNSIGNED_LESS: u64 = 6;

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
    assert!(binding::aarch64_integer_leaf(&[2, 2, ADD, 0, 1, u128::BITS as u64]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[0, 0, CALL, 0, 0, 0]).is_empty());
}

#[test]
fn aarch64_call_packet_keeps_void_calls_and_rejects_malformed_arguments() {
    const BRANCH_AND_LINK: u32 = 0x9400_0000;
    const ARM64_CALL: u64 = 4;
    let packet = binding::aarch64_relocatable_block(&[
        2,
        0,
        CALL_ARGUMENT,
        1,
        0,
        0,
        CALL_ARGUMENT,
        0,
        0,
        0,
        CALL_VOID,
        17,
        2,
        0,
    ]);
    let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
    let (code, reference) = body.split_at(u64::from_le_bytes(*length) as usize);
    let (reference, remainder) = reference.as_chunks::<{ size_of::<u64>() }>();
    assert!(remainder.is_empty());
    let reference: Vec<_> = reference.iter().copied().map(u64::from_le_bytes).collect();
    let [offset, ARM64_CALL, 17] = reference.as_slice() else {
        panic!("unexpected relocation: {reference:?}");
    };
    assert_eq!(
        &code[*offset as usize..][..size_of::<u32>()],
        &BRANCH_AND_LINK.to_le_bytes(),
    );
    assert!(code.ends_with(&RET.to_le_bytes()));
    for malformed in [
        &[1, 0, CALL_ARGUMENT, 0, 0, 0][..],
        &[0, 0, CALL, 17, 1, 0],
        &[1, 2, CALL_ARGUMENT, 1, 0, 0, CALL, 17, 1, 0],
        &[1, 2, CALL_ARGUMENT, 0, 0, 0, CALL, 17, 1, 1],
    ] {
        assert!(binding::aarch64_relocatable_block(malformed).is_empty());
    }
}

#[test]
fn aarch64_calls_pair_adjacent_saves_and_keep_unclobbered_results_in_x0() {
    const STP_X0_X1_SP_8: u32 = 0xa900_87e0;
    const LDP_X0_X1_SP_8: u32 = 0xa940_87e0;
    const PAIR_OPCODE_MASK: u32 = 0xffc0_0000;
    const PAIR_OPCODE: u32 = 0xa900_0000;
    const LOAD_BIT: u32 = 0x0040_0000;
    const CALL_AND_FRAME_INSTRUCTIONS: usize = 6;
    let code = |tape: &[u64]| {
        let packet = binding::aarch64_relocatable_block(tape);
        let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
        let code = &body[..u64::from_le_bytes(*length) as usize];
        let (words, remainder) = code.as_chunks::<{ size_of::<u32>() }>();
        assert!(remainder.is_empty());
        words
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect::<Vec<_>>()
    };
    let paired = code(&[2, 3, CALL_VOID, 17, 0, 0, ADD, 0, 1, CELL_BITS]);
    assert!(paired.contains(&STP_X0_X1_SP_8));
    assert!(paired.contains(&LDP_X0_X1_SP_8));

    let distant = code(&[
        2,
        5,
        STACK_SLOT,
        512,
        8,
        0,
        STACK_STORE_QWORD,
        0,
        0,
        0,
        CALL_VOID,
        17,
        0,
        0,
        ADD,
        0,
        1,
        CELL_BITS,
    ]);
    assert!(
        distant
            .iter()
            .all(|word| { (word & (PAIR_OPCODE_MASK & !LOAD_BIT)) != PAIR_OPCODE })
    );

    let result_in_x0 = code(&[0, 0, CALL, 17, 0, 0]);
    assert_eq!(result_in_x0.len(), CALL_AND_FRAME_INSTRUCTIONS);
}

#[test]
fn aarch64_call_reuses_unmodified_argument_registers() {
    const MOV_X1_X0: u32 = 0xaa00_03e1;
    const BRANCH_AND_LINK: u32 = 0x9400_0000;
    const CALL_AND_FRAME_WITH_MOVE: usize = 7;
    let packet = binding::aarch64_relocatable_block(&[
        2,
        4,
        CALL_ARGUMENT,
        0,
        0,
        0,
        CALL_ARGUMENT,
        0,
        0,
        0,
        CALL,
        17,
        2,
        0,
    ]);
    let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
    let code = &body[..u64::from_le_bytes(*length) as usize];
    let expected = [MOV_X1_X0, BRANCH_AND_LINK].map(u32::to_le_bytes).concat();
    assert_eq!(code.len(), CALL_AND_FRAME_WITH_MOVE * size_of::<u32>());
    assert!(code.windows(expected.len()).any(|bytes| bytes == expected));
}

#[test]
fn aarch64_paired_returns_place_both_values_without_overwriting_either() {
    const MOV_X16_X1: u32 = 0xaa01_03f0;
    const MOV_X1_X0: u32 = 0xaa00_03e1;
    const MOV_X0_X16: u32 = 0xaa10_03e0;
    let expected = [MOV_X16_X1, MOV_X1_X0, MOV_X0_X16, RET]
        .map(u32::to_le_bytes)
        .concat();
    assert_eq!(
        binding::aarch64_integer_leaf(&[2, 1, RETURN_PAIR, 0, 0, 0]),
        expected,
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[2, NO_VALUE, BLOCK_LABEL, 0, 0, 2, BLOCK_RETURN, 1, 0, 0,]),
        expected,
    );
    assert!(binding::aarch64_integer_leaf(&[0, NO_VALUE, RETURN_PAIR, 0, 0, 0]).is_empty());
}

#[test]
fn aarch64_pair_calls_keep_the_high_result_when_the_low_result_is_unused() {
    const BRANCH_AND_LINK: u32 = 0x9400_0000;
    const MOV_X17_X1: u32 = 0xaa01_03f1;
    const MOV_X0_X17: u32 = 0xaa11_03e0;
    let input = [0, 1, CALL_PAIR, 17, 0, 0, CALL_HIGH, 0, 0, 0];
    assert!(binding::aarch64_integer_leaf(&input).is_empty());
    let packet = binding::aarch64_relocatable_block(&input);
    let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
    let code = &body[..u64::from_le_bytes(*length) as usize];
    let expected = [BRANCH_AND_LINK, MOV_X17_X1, MOV_X0_X17]
        .map(u32::to_le_bytes)
        .concat();
    assert!(code.windows(expected.len()).any(|bytes| bytes == expected));
    for malformed in [
        &[0, 0, CALL_PAIR, 17, 0, 0][..],
        &[0, 0, CALL_HIGH, 0, 0, 0],
        &[0, 1, CALL_PAIR, 17, 0, 0, CALL_HIGH, 1, 0, 0],
    ] {
        assert!(binding::aarch64_relocatable_block(malformed).is_empty());
    }
}

#[test]
fn aarch64_integer_division_checks_zero_and_signed_overflow() {
    const CBNZ_X1_OVER_TRAP: u32 = 0xb500_0041;
    const CBZ_X1_TO_SHARED_SDIV_TRAP: u32 = 0xb400_0081;
    const CBNZ_W1_OVER_TRAP: u32 = 0x3500_0041;
    const UDF: u32 = 0x0000_c11f;
    const CMN_X1_1: u32 = 0xb100_043f;
    const CCMP_X0_1: u32 = 0xfa41_0800;
    const BVC_OVER_TRAP: u32 = 0x5400_0047;
    const SDIV_X0_X0_X1: u32 = 0x9ac1_0c00;
    const UDIV_W0_W0_W1: u32 = 0x1ac1_0800;
    const SDIV_X16_X0_X1: u32 = 0x9ac1_0c10;
    const MSUB_X0_X16_X1_X0: u32 = 0x9b01_8200;
    let encode = |op, width, result| {
        let bytes = binding::aarch64_integer_leaf(&[2, result, op, 0, 1, width]);
        let (words, remainder) = bytes.as_chunks::<{ size_of::<u32>() }>();
        assert!(remainder.is_empty());
        words
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect::<Vec<_>>()
    };
    let signed_checks = [
        CBZ_X1_TO_SHARED_SDIV_TRAP,
        CMN_X1_1,
        CCMP_X0_1,
        BVC_OVER_TRAP,
        UDF,
    ];
    assert_eq!(
        encode(SIGNED_DIV, CELL_BITS, 2),
        [signed_checks.as_slice(), &[SDIV_X0_X0_X1, RET]].concat(),
    );
    assert_eq!(
        encode(SIGNED_DIV, CELL_BITS, 0),
        [signed_checks.as_slice(), &[RET]].concat(),
    );
    assert_eq!(
        encode(UNSIGNED_DIV, DWORD_BITS, 2),
        [CBNZ_W1_OVER_TRAP, UDF, UDIV_W0_W0_W1, RET],
    );
    assert_eq!(
        encode(SIGNED_REM, CELL_BITS, 2),
        [
            CBNZ_X1_OVER_TRAP,
            UDF,
            SDIV_X16_X0_X1,
            MSUB_X0_X16_X1_X0,
            RET
        ],
    );
    assert!(encode(SIGNED_DIV, BYTE_BITS, 2).is_empty());
}

#[test]
fn aarch64_explicit_traps_terminate_their_control_flow_paths() {
    const UDF: u32 = 0x0000_c11f;
    const CBZ_X0_OVER_UDF: u32 = 0xb400_0040;
    const CBZ_X2_TO_FINAL_TRAP: u32 = 0xb400_00a2;
    const MOV_X16_X1: u32 = 0xaa01_03f0;
    const MOV_X1_X0: u32 = 0xaa00_03e1;
    const MOV_X0_X16: u32 = 0xaa10_03e0;
    assert_eq!(
        binding::aarch64_integer_leaf(&[0, NO_VALUE, TRAP, 0, 0, 0]),
        UDF.to_le_bytes(),
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[
            1,
            0,
            BRANCH_ZERO,
            0,
            0,
            0,
            EARLY_RETURN,
            TRAP_VALUE,
            0,
            1
        ]),
        [CBZ_X0_OVER_UDF, UDF, RET].map(u32::to_le_bytes).concat(),
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[
            3,
            NO_VALUE,
            BRANCH_ZERO,
            2,
            0,
            0,
            EARLY_RETURN,
            1,
            0,
            3,
            TRAP,
            2,
            0,
            0,
        ]),
        [
            CBZ_X2_TO_FINAL_TRAP,
            MOV_X16_X1,
            MOV_X1_X0,
            MOV_X0_X16,
            RET,
            UDF
        ]
        .map(u32::to_le_bytes)
        .concat(),
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[
            0,
            NO_VALUE,
            BLOCK_LABEL,
            0,
            0,
            0,
            BLOCK_RETURN,
            TRAP_VALUE,
            0,
            0,
        ]),
        UDF.to_le_bytes(),
    );
    for malformed in [
        &[1, 0, TRAP, 0, 0, 0][..],
        &[0, NO_VALUE, TRAP, 3, 0, 0],
        &[0, NO_VALUE, TRAP, 0, 0, 0, CONSTANT, 1, 0, 0],
        &[1, 0, BRANCH_ZERO, 0, 0, 0, EARLY_RETURN, TRAP_VALUE, 7, 1],
    ] {
        assert!(binding::aarch64_integer_leaf(malformed).is_empty());
    }
}

#[test]
fn aarch64_narrow_arithmetic_and_extension() {
    const ADD_W0_W0_W1: u32 = 0x0b01_0000;
    const TRUNCATE_X0_TO_BYTE: u32 = 0xd340_1c00;
    const SIGN_EXTEND_BYTE_TO_X0: u32 = 0x9340_1c00;
    const SMULH_X0_X0_X1: u32 = 0x9b41_7c00;
    const UMULH_X0_X0_X1: u32 = 0x9bc1_7c00;
    let bytes = |words: &[u32]| {
        words
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    for (op, width, encoded) in [
        (ADD, DWORD_BITS, ADD_W0_W0_W1),
        (SIGNED_MUL_HIGH, CELL_BITS, SMULH_X0_X0_X1),
        (UNSIGNED_MUL_HIGH, CELL_BITS, UMULH_X0_X0_X1),
    ] {
        assert_eq!(
            binding::aarch64_integer_leaf(&[2, 2, op, 0, 1, width]),
            bytes(&[encoded, RET]),
        );
    }
    for (op, last) in [
        (ZERO_EXTEND, vec![RET]),
        (SIGN_EXTEND, vec![SIGN_EXTEND_BYTE_TO_X0, RET]),
    ] {
        let packet = [
            1, 2, TRUNCATE, 0, CELL_BITS, BYTE_BITS, op, 1, BYTE_BITS, CELL_BITS,
        ];
        let mut instructions = vec![TRUNCATE_X0_TO_BYTE];
        instructions.extend(last);
        assert_eq!(binding::aarch64_integer_leaf(&packet), bytes(&instructions));
    }
    assert!(binding::aarch64_integer_leaf(&[1, 1, TRUNCATE, 0, BYTE_BITS, CELL_BITS]).is_empty());
}

#[test]
fn aarch64_compares_narrow_signed_and_unsigned_immediates() {
    const TRUNCATE_X0_TO_BYTE: u32 = 0xd340_1c00;
    const CMP_X0_255: u32 = 0xf103_fc1f;
    const CSET_X0_LO: u32 = 0x9a9f_27e0;
    const SIGN_EXTEND_BYTE_TO_X17: u32 = 0x9340_1c11;
    const CMN_X17_1: u32 = 0xb100_063f;
    const CSET_X0_LT: u32 = 0x9a9f_a7e0;
    for (predicate, instructions) in [
        (UNSIGNED_LESS, vec![CMP_X0_255, CSET_X0_LO]),
        (
            SIGNED_LESS,
            vec![SIGN_EXTEND_BYTE_TO_X17, CMN_X17_1, CSET_X0_LT],
        ),
    ] {
        let packet = [
            1,
            2,
            TRUNCATE,
            0,
            CELL_BITS,
            BYTE_BITS,
            COMPARE_BYTE_IMMEDIATE,
            1,
            u64::from(u32::MAX),
            predicate,
        ];
        let expected: Vec<_> = [TRUNCATE_X0_TO_BYTE]
            .into_iter()
            .chain(instructions)
            .chain([RET])
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(binding::aarch64_integer_leaf(&packet), expected);
    }
}

#[test]
fn aarch64_memory_addressing_and_store_liveness() {
    const LDR_X0_X0_32760: u32 = 0xf97f_fc00;
    const LDUR_W0_X0_NEG256: u32 = 0xb850_0000;
    const MOV_X16_32768: u32 = 0xd290_0010;
    const LDR_W0_X0_X16: u32 = 0xb870_6800;
    const STURB_W1_X0_NEG1: u32 = 0x381f_f001;
    const STRH_W1_X0_2: u32 = 0x7900_0401;
    for (packet, instructions) in [
        ([1, 1, LOAD, 0, 32760, CELL_BITS], vec![LDR_X0_X0_32760]),
        (
            [1, 1, LOAD, 0, u64::from(-256_i32 as u32), DWORD_BITS],
            vec![LDUR_W0_X0_NEG256],
        ),
        (
            [1, 1, LOAD, 0, 32768, DWORD_BITS],
            vec![MOV_X16_32768, LDR_W0_X0_X16],
        ),
        (
            [2, NO_VALUE, STORE_BYTE, 0, 1, u64::from(u32::MAX)],
            vec![STURB_W1_X0_NEG1],
        ),
        ([2, NO_VALUE, STORE_WORD, 0, 1, 2], vec![STRH_W1_X0_2]),
        ([1, NO_VALUE, LOAD, 0, 0, CELL_BITS], vec![]),
    ] {
        let expected: Vec<_> = instructions
            .into_iter()
            .chain([RET])
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(binding::aarch64_integer_leaf(&packet), expected);
    }
    assert!(binding::aarch64_integer_leaf(&[2, 2, STORE_BYTE, 0, 1, 0]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[1, 1, LOAD, 0, NO_VALUE, CELL_BITS]).is_empty());
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
fn aarch64_branches_use_the_instruction_address_and_reject_escaping_values() {
    const CBZ_X0_FORWARD_TWO: u32 = 0xb400_0040;
    const CMP_X0_X1: u32 = 0xeb01_001f;
    const B_NE_FORWARD_TWO: u32 = 0x5400_0041;
    const MOV_X0_X1: u32 = 0xaa01_03e0;
    let bytes = |instructions: &[u32]| {
        instructions
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        binding::aarch64_integer_leaf(&[2, 1, BRANCH_ZERO, 0, 0, 0, EARLY_RETURN, 0, 0, 2]),
        bytes(&[CBZ_X0_FORWARD_TWO, RET, MOV_X0_X1, RET]),
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[
            2,
            1,
            COMPARE,
            0,
            1,
            EQUAL,
            BRANCH_ZERO,
            2,
            0,
            0,
            EARLY_RETURN,
            0,
            0,
            3,
        ]),
        bytes(&[CMP_X0_X1, B_NE_FORWARD_TWO, RET, MOV_X0_X1, RET]),
    );
    assert!(binding::aarch64_integer_leaf(&[1, 0, BRANCH_ZERO, 0, 0, 0]).is_empty());
    assert!(
        binding::aarch64_integer_leaf(&[
            1,
            2,
            BRANCH_ZERO,
            0,
            0,
            0,
            CONSTANT,
            42,
            0,
            0,
            EARLY_RETURN,
            2,
            0,
            1,
        ])
        .is_empty()
    );
    assert!(
        binding::aarch64_integer_leaf(&[1, 0, BRANCH_ZERO, 0, 0, 0, EARLY_RETURN, NO_VALUE, 0, 1])
            .is_empty()
    );
}

#[test]
fn aarch64_fixed_frames_restore_sp_on_each_return() {
    const SP_ALIGN: u64 = 16;
    const FRAME_SIZE: u64 = SP_ALIGN;
    const QWORD_BYTES: u64 = CELL_BITS / BYTE_BITS;
    const SUB_SP_16: u32 = 0xd100_43ff;
    const CBZ_X0_FORWARD_FIVE: u32 = 0xb400_00a0;
    const STR_X1_SP_8: u32 = 0xf900_07e1;
    const LDR_X0_SP_8: u32 = 0xf940_07e0;
    const ADD_SP_16: u32 = 0x9100_43ff;
    const MOV_X0_X1: u32 = 0xaa01_03e0;
    let packet = [
        2,
        1,
        STACK_SLOT,
        FRAME_SIZE,
        SP_ALIGN,
        0,
        BRANCH_ZERO,
        0,
        0,
        0,
        STACK_STORE_QWORD,
        1,
        0,
        QWORD_BYTES,
        STACK_LOAD,
        0,
        QWORD_BYTES,
        CELL_BITS,
        EARLY_RETURN,
        5,
        0,
        3,
    ];
    let expected: Vec<_> = [
        SUB_SP_16,
        CBZ_X0_FORWARD_FIVE,
        STR_X1_SP_8,
        LDR_X0_SP_8,
        ADD_SP_16,
        RET,
        MOV_X0_X1,
        ADD_SP_16,
        RET,
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect();
    assert_eq!(binding::aarch64_integer_leaf(&packet), expected);
    assert_eq!(
        binding::aarch64_integer_leaf(&[0, NO_VALUE, STACK_SLOT, FRAME_SIZE, SP_ALIGN, 0]),
        RET.to_le_bytes()
    );
    assert!(
        binding::aarch64_integer_leaf(&[
            0, 1, STACK_SLOT, FRAME_SIZE, SP_ALIGN, 0, STACK_LOAD, 0, FRAME_SIZE, CELL_BITS,
        ])
        .is_empty()
    );
    assert!(
        binding::aarch64_integer_leaf(&[0, NO_VALUE, STACK_SLOT, u64::MAX, SP_ALIGN, 0]).is_empty()
    );
}

#[test]
fn aarch64_labeled_control_flow_patches_backedges_and_checks_block_operands() {
    const SP_ALIGN: u64 = 16;
    const SUB_SP_16: u32 = 0xd100_43ff;
    const STR_X0_SP: u32 = 0xf900_03e0;
    const LDR_X0_SP: u32 = 0xf940_03e0;
    const CBZ_X0_FORWARD_TWO: u32 = 0xb400_0040;
    const B_BACK_TWO: u32 = 0x17ff_fffe;
    const CBZ_X0_BACK_ONE: u32 = 0xb4ff_ffe0;
    const MOV_X0_ONE: u32 = 0xd280_0020;
    const ADD_SP_16: u32 = 0x9100_43ff;
    const BRANCH_TARGET_WORD: usize = 28;
    const RETURN_VALUE_WORD: usize = 43;
    const HOME_SLOT_WORD: usize = 8;
    #[rustfmt::skip]
    let packet = [
        1, NO_VALUE,
        STACK_SLOT, SP_ALIGN, SP_ALIGN, 0,
        BLOCK_LABEL, 0, 0, 1,
        STACK_STORE_QWORD, 0, 0, 0,
        JUMP_LABEL, 1, 0, 0,
        BLOCK_LABEL, 1, 0, 1,
        STACK_LOAD, 0, 0, CELL_BITS,
        BRANCH_BLOCK, 6, 2, 0,
        JUMP_LABEL, 1, 0, 0,
        BLOCK_LABEL, 2, 0, 1,
        STACK_LOAD, 0, 0, CELL_BITS,
        BLOCK_RETURN, 10, 0, 0,
    ];
    let expected: Vec<_> = [
        SUB_SP_16,
        STR_X0_SP,
        LDR_X0_SP,
        CBZ_X0_FORWARD_TWO,
        B_BACK_TWO,
        LDR_X0_SP,
        ADD_SP_16,
        RET,
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect();
    assert_eq!(binding::aarch64_integer_leaf(&packet), expected);

    let mut crossed = packet;
    crossed[RETURN_VALUE_WORD] = 0;
    assert!(binding::aarch64_integer_leaf(&crossed).is_empty());
    let mut unknown = packet;
    unknown[BRANCH_TARGET_WORD] = 3;
    assert!(binding::aarch64_integer_leaf(&unknown).is_empty());

    const MOV_X8_X0: u32 = 0xaa00_03e8;
    const CBZ_X8_FORWARD_TWO: u32 = 0xb400_0048;
    const B_BACK_ONE: u32 = 0x17ff_ffff;
    const MOV_X0_X8: u32 = 0xaa08_03e0;
    let mut promoted = packet;
    promoted[HOME_SLOT_WORD] = 1;
    let expected: Vec<_> = [MOV_X8_X0, CBZ_X8_FORWARD_TWO, B_BACK_ONE, MOV_X0_X8, RET]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    assert_eq!(binding::aarch64_integer_leaf(&promoted), expected);

    #[rustfmt::skip]
    let conditional_backedge = [
        0, NO_VALUE,
        BLOCK_LABEL, 0, 0, 0,
        JUMP_LABEL, 1, 0, 0,
        BLOCK_LABEL, 1, 0, 0,
        JUMP_LABEL, 2, 0, 0,
        BLOCK_LABEL, 2, 0, 0,
        CONSTANT, 1, 0, 0,
        BRANCH_BLOCK, 5, 1, 0,
        JUMP_LABEL, 3, 0, 0,
        BLOCK_LABEL, 3, 0, 0,
        BLOCK_RETURN, NO_VALUE, 0, 0,
    ];
    let expected: Vec<_> = [MOV_X0_ONE, CBZ_X0_BACK_ONE, RET]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    assert_eq!(
        binding::aarch64_integer_leaf(&conditional_backedge),
        expected
    );
}

#[test]
fn aarch64_register_homes_preserve_snapshots_and_retry_on_stack() {
    const MOV_X8_X0: u32 = 0xaa00_03e8;
    const MOV_X0_X8: u32 = 0xaa08_03e0;
    const MOV_X8_X1: u32 = 0xaa01_03e8;
    const SUB_SP_64: u32 = 0xd101_03ff;
    const SLOT_ALIGNMENT: u64 = 16;
    const HOME_COUNT: u64 = 8;
    const QWORD_BYTES: u64 = CELL_BITS / BYTE_BITS;
    const FRAME_BYTES: u64 = HOME_COUNT * QWORD_BYTES;
    #[rustfmt::skip]
    let snapshot = [
        2, NO_VALUE,
        STACK_SLOT, FRAME_BYTES, SLOT_ALIGNMENT, 0,
        BLOCK_LABEL, 0, 1, 1,
        STACK_STORE_QWORD, 0, 0, 0,
        STACK_LOAD, 0, 0, CELL_BITS,
        STACK_STORE_QWORD, 1, 0, 0,
        BLOCK_RETURN, 5, 0, 0,
    ];
    let expected: Vec<_> = [MOV_X8_X0, MOV_X0_X8, MOV_X8_X1, RET]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    assert_eq!(binding::aarch64_integer_leaf(&snapshot), expected);

    let mut records = vec![
        [STACK_SLOT, FRAME_BYTES, SLOT_ALIGNMENT, 0],
        [BLOCK_LABEL, 0, 1, 1],
    ];
    let start = records.len() as u64;
    for constant in 0..=HOME_COUNT {
        records.push([CONSTANT, constant, 0, 0]);
    }
    let mut sum = start;
    for constant in start + 1..=start + HOME_COUNT {
        let next = records.len() as u64;
        records.push([ADD, sum, constant, CELL_BITS]);
        sum = next;
    }
    for home in 0..HOME_COUNT {
        records.push([STACK_STORE_QWORD, sum, 0, home * QWORD_BYTES]);
    }
    records.push([BLOCK_RETURN, sum, 0, 0]);
    let packet: Vec<_> = [0, NO_VALUE]
        .into_iter()
        .chain(records.into_iter().flatten())
        .collect();
    assert!(binding::aarch64_integer_leaf(&packet).starts_with(&SUB_SP_64.to_le_bytes()));
}

#[test]
fn aarch64_spills_live_values_and_limits_the_combined_frame() {
    const LIVE_VALUES: u64 = 20;
    const FULL_FRAME_BYTES: u64 = 4096;
    const SLOT_ALIGNMENT: u64 = 16;
    const SUB_SP_64: u32 = 0xd101_03ff;
    const ADD_SP_64: u32 = 0x9101_03ff;
    const STR_X12_SP: u32 = 0xf900_03ec;
    const LDR_X14_SP: u32 = 0xf940_03ee;

    let packet = |explicit_frame| {
        let mut records = Vec::new();
        if explicit_frame != 0 {
            records.push([STACK_SLOT, explicit_frame, SLOT_ALIGNMENT, 0]);
        }
        let start = records.len() as u64;
        for value in 0..LIVE_VALUES {
            records.push([CONSTANT, value, 0, 0]);
        }
        if explicit_frame != 0 {
            records.push([STACK_STORE_QWORD, start, 0, 0]);
        }
        let mut sum = start;
        for value in start + 1..start + LIVE_VALUES {
            let next = records.len() as u64;
            records.push([ADD, sum, value, CELL_BITS]);
            sum = next;
        }
        [0, sum]
            .into_iter()
            .chain(records.into_iter().flatten())
            .collect::<Vec<_>>()
    };
    let code = binding::aarch64_integer_leaf(&packet(0));
    let (words, remainder) = code.as_chunks::<{ size_of::<u32>() }>();
    assert!(remainder.is_empty());
    let words: Vec<_> = words.iter().copied().map(u32::from_le_bytes).collect();
    assert_eq!(words.first(), Some(&SUB_SP_64));
    assert!(words.ends_with(&[ADD_SP_64, RET]));
    assert!(words.contains(&STR_X12_SP));
    assert!(words.contains(&LDR_X14_SP));
    assert!(binding::aarch64_integer_leaf(&packet(FULL_FRAME_BYTES)).is_empty());
}

#[test]
fn aarch64_jump_tables_use_bounded_signed_halfword_offsets() {
    const CMP_W0_3: u32 = 0x7100_0c1f;
    const B_HS_DEFAULT: u32 = 0x5400_01e2;
    const CSEL_W17_ZERO_W0_HS: u32 = 0x1a80_23f1;
    const CSDB: u32 = 0xd503_229f;
    const ADR_X16_TABLE: u32 = 0x1000_0090;
    const LDRSH_X17_X16_W17: u32 = 0x78b1_5a11;
    const ADD_X17_X16_X17_SCALED: u32 = 0x8b11_0a11;
    const BR_X17: u32 = 0xd61f_0220;
    const FIRST_TWO_ENTRIES: u32 = 0x0002_0006;
    const LAST_ENTRY_AND_PAD: u32 = 4;
    const MOV_X0_17: u32 = 0xd280_0220;
    const MOV_X0_29: u32 = 0xd280_03a0;
    const MOV_X0_41: u32 = 0xd280_0520;
    const MOV_X0_99: u32 = 0xd280_0c60;
    const FIRST_TARGET_WORD: usize = 11;
    #[rustfmt::skip]
    let packet = [
        1, NO_VALUE,
        BLOCK_LABEL, 0, 0, 1,
        BRANCH_TABLE, 0, 3, 4,
        BRANCH_TABLE_TARGET, 3, 0, 0,
        BRANCH_TABLE_TARGET, 1, 0, 0,
        BRANCH_TABLE_TARGET, 2, 0, 0,
        BLOCK_LABEL, 1, 0, 1,
        CONSTANT, 17, 0, 0,
        BLOCK_RETURN, 7, 0, 0,
        BLOCK_LABEL, 2, 0, 1,
        CONSTANT, 29, 0, 0,
        BLOCK_RETURN, 10, 0, 0,
        BLOCK_LABEL, 3, 0, 1,
        CONSTANT, 41, 0, 0,
        BLOCK_RETURN, 13, 0, 0,
        BLOCK_LABEL, 4, 0, 1,
        CONSTANT, 99, 0, 0,
        BLOCK_RETURN, 16, 0, 0,
    ];
    let code = binding::aarch64_integer_leaf(&packet);
    let expected: Vec<_> = [
        CMP_W0_3,
        B_HS_DEFAULT,
        CSEL_W17_ZERO_W0_HS,
        CSDB,
        ADR_X16_TABLE,
        LDRSH_X17_X16_W17,
        ADD_X17_X16_X17_SCALED,
        BR_X17,
        FIRST_TWO_ENTRIES,
        LAST_ENTRY_AND_PAD,
        MOV_X0_17,
        RET,
        MOV_X0_29,
        RET,
        MOV_X0_41,
        RET,
        MOV_X0_99,
        RET,
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect();
    assert_eq!(code, expected);

    let mut unknown = packet;
    unknown[FIRST_TARGET_WORD] = 9;
    assert!(binding::aarch64_integer_leaf(&unknown).is_empty());
}

#[test]
fn aarch64_jump_tables_retry_with_full_offsets_for_distant_blocks() {
    const ARGUMENTS: u64 = 1;
    const SHORT_REACH_INSTRUCTIONS: usize = 1 << 15;
    const LDRSW_X17_X16_W17: u32 = 0xb8b1_5a11;
    const TABLE_LOAD_WORD: usize = 5;
    const FIRST_ENTRY_WORD: usize = 8;
    let mut records = vec![
        [BLOCK_LABEL, 0, 0, 1],
        [BRANCH_TABLE, 0, 1, 1],
        [BRANCH_TABLE_TARGET, 2, 0, 0],
        [BLOCK_LABEL, 1, 0, 1],
        [CONSTANT, 0, 0, 0],
    ];
    let mut value = ARGUMENTS + records.len() as u64 - 1;
    for _ in 0..SHORT_REACH_INSTRUCTIONS {
        let next = ARGUMENTS + records.len() as u64;
        records.push([ADD_IMMEDIATE, value, 1, CELL_BITS]);
        value = next;
    }
    records.push([BLOCK_RETURN, value, 0, 0]);
    records.push([BLOCK_LABEL, 2, 0, 1]);
    let result = ARGUMENTS + records.len() as u64;
    records.push([CONSTANT, 7, 0, 0]);
    records.push([BLOCK_RETURN, result, 0, 0]);

    let packet: Vec<_> = [ARGUMENTS, NO_VALUE]
        .into_iter()
        .chain(records.into_iter().flatten())
        .collect();
    let code = binding::aarch64_integer_leaf(&packet);
    let (words, remainder) = code.as_chunks::<{ size_of::<u32>() }>();
    assert!(remainder.is_empty());
    assert_eq!(words[TABLE_LOAD_WORD], LDRSW_X17_X16_W17.to_le_bytes());
    let distance = (SHORT_REACH_INSTRUCTIONS + 3) * size_of::<u32>();
    assert_eq!(words[FIRST_ENTRY_WORD], (distance as u32).to_le_bytes());
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
fn aarch64_integer_functions_reach_elf_and_macho_object_emission() {
    const LEAF: &str = "
        fn main(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {
            wrapping_add(wrapping_mul(a, h), b ^ c ^ d ^ e ^ f ^ g)
        }
    ";
    const FLOAT: &str = "fn main(x: f64) -> f64 { x + x }";
    const CALLER: &str = "
        fn mix(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {
            wrapping_add(wrapping_mul(a, h), b ^ c ^ d ^ e ^ f ^ g)
        }
        fn main(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {
            let first = mix(h, g, f, e, d, c, b, a);
            wrapping_add(mix(a, c, b, d, h, f, g, e), first ^ a)
        }
    ";
    const TRIPLES: [(&str, BinaryFormat); 2] = [
        ("aarch64-unknown-linux-gnu", BinaryFormat::Elf),
        ("aarch64-apple-darwin", BinaryFormat::MachO),
    ];
    static SELECTED: AtomicUsize = AtomicUsize::new(0);
    static NONLEAVES: AtomicUsize = AtomicUsize::new(0);
    let checked = |source| {
        let parsed = parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        checked
    };
    let leaf = checked(LEAF);
    let float = checked(FLOAT);
    let caller = checked(CALLER);
    let compile = |checked: &check::Checked, triple| {
        native::object_with_options(&checked.world, "main", Some(triple), false)
            .unwrap()
            .0
    };
    let baselines = TRIPLES.map(|(triple, _)| {
        (
            compile(&leaf, triple),
            compile(&float, triple),
            compile(&caller, triple),
        )
    });
    assert!(
        native::machine::install_aarch64_encoder(|words| {
            let bytes = binding::aarch64_relocatable_block(words);
            if bytes.is_empty() {
                None
            } else {
                SELECTED.fetch_add(1, Relaxed);
                let (length, body) = bytes.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
                if body.len() > u64::from_le_bytes(*length) as usize {
                    NONLEAVES.fetch_add(1, Relaxed);
                }
                Some(bytes)
            }
        })
        .is_ok()
    );
    for ((triple, format), (baseline, float_baseline, call_baseline)) in
        TRIPLES.into_iter().zip(baselines)
    {
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

        let caller_bytes = compile(&caller, triple);
        let call_relocations = |bytes: &[u8]| {
            let file = object::File::parse(bytes).unwrap();
            file.sections()
                .filter(|section| section.kind() == SectionKind::Text)
                .flat_map(|section| section.relocations())
                .map(|(_, reloc)| (reloc.flags(), reloc.addend()))
                .collect::<Vec<_>>()
        };
        let expected = call_relocations(&call_baseline);
        assert_eq!(expected.len(), 2, "{triple}");
        assert_eq!(call_relocations(&caller_bytes), expected, "{triple}");
    }
    assert_eq!(NONLEAVES.load(Relaxed), TRIPLES.len());
    assert_eq!(SELECTED.load(Relaxed), 3 * TRIPLES.len());
}
