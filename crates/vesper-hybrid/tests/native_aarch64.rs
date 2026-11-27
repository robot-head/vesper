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
const WORD_BITS: u64 = u16::BITS as u64;
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
const SHR_REGISTER: u64 = 21;
const LOAD: u64 = 23;
const STORE_BYTE: u64 = 24;
const STORE_WORD: u64 = 25;
const STORE_QWORD: u64 = 27;
const BRANCH_ZERO: u64 = 28;
const EARLY_RETURN: u64 = 29;
const STACK_SLOT: u64 = 30;
const STACK_LOAD: u64 = 32;
const STACK_STORE_BYTE: u64 = 33;
const STACK_STORE_QWORD: u64 = 36;
const RETURN_PAIR: u64 = 37;
const CALL: u64 = 38;
const CALL_ARGUMENT: u64 = 39;
const CALL_VOID: u64 = 40;
const CALL_PAIR: u64 = 41;
const CALL_HIGH: u64 = 42;
const SYMBOL_RELATIVE: u64 = 43;
const SYMBOL_GOT: u64 = 44;
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
const UNSIGNED_ADD_OVERFLOW: u64 = 68;
const SIGNED_ADD_OVERFLOW: u64 = 69;
const UNSIGNED_SUB_OVERFLOW: u64 = 70;
const SIGNED_MUL_OVERFLOW: u64 = 73;
const OVERFLOW_FLAG: u64 = 74;
const BRANCH_TABLE: u64 = 75;
const BRANCH_TABLE_TARGET: u64 = 76;
const ADD_CARRY: u64 = 77;
const SUB_BORROW: u64 = 78;
const FLOAT_ARGUMENT: u64 = 79;
const FLOAT_ADD: u64 = 81;
const FLOAT_COMPARE64: u64 = 89;
const BITS_TO_FLOAT: u64 = 93;
const FLOAT_TO_BITS: u64 = 94;
const FLOAT_RETURN: u64 = 96;
const CALL_FLOAT: u64 = 97;
const FLOAT_RETURN_LAYOUT: u64 = 3;
const ATOMIC_ADD: u64 = 102;
const ATOMIC_SUB: u64 = 103;
const ATOMIC_EXCHANGE: u64 = 104;
const FENCE: u64 = 105;
const BYTE_SWAP: u64 = 106;
const LEADING_ZEROS: u64 = 107;
const TRAILING_ZEROS: u64 = 108;
const POPULATION_COUNT: u64 = 109;
const REVERSE_BITS: u64 = 110;
const LEADING_SIGN_BITS: u64 = 111;
const ROTATE_RIGHT: u64 = 112;
const ROTATE_RIGHT_REGISTER: u64 = 113;
const ROTATE_LEFT_REGISTER: u64 = 114;
const FUNNEL_RIGHT: u64 = 115;
const AND_NOT: u64 = 116;
const OR_NOT: u64 = 117;
const XOR_NOT: u64 = 118;
const ABS: u64 = 119;
const ATOMIC_LOAD: u64 = 120;
const ATOMIC_STORE: u64 = 121;
const ATOMIC_CAS_BYTE: u64 = 122;
const ATOMIC_CAS_WORD: u64 = 123;
const ATOMIC_NAND: u64 = 127;
const ATOMIC_UNSIGNED_MIN: u64 = 130;
const ATOMIC_SIGNED_MAX: u64 = 133;
const CALL_FLOAT32: u64 = 134;
const EQUAL: u64 = 0;
const SIGNED_LESS: u64 = 2;
const UNSIGNED_LESS: u64 = 6;
const FLOAT_UNORDERED_OR_EQUAL: u64 = 5;

#[test]
fn aarch64_internal_floats_cross_the_register_bank_only_for_arithmetic() {
    const FMOV_D16_X0: u32 = 0x9e67_0010;
    const FMOV_D17_X1: u32 = 0x9e67_0031;
    const FADD_D16_D16_D17: u32 = 0x1e71_2a10;
    const FMOV_X0_D16: u32 = 0x9e66_0200;
    const FCMP_D16_D17: u32 = 0x1e71_2200;
    const CSET_X0_EQ: u32 = 0x9a9f_17e0;
    const CSINC_X0_X0_XZR_VC: u32 = 0x9a9f_7400;

    #[rustfmt::skip]
    let sum = [
        2, 5,
        BITS_TO_FLOAT, 0, 0, CELL_BITS,
        BITS_TO_FLOAT, 1, 0, CELL_BITS,
        FLOAT_ADD, 2, 3, CELL_BITS,
        FLOAT_TO_BITS, 4, 0, CELL_BITS,
    ];
    assert_eq!(
        binding::aarch64_integer_leaf(&sum),
        [FMOV_D16_X0, FMOV_D17_X1, FADD_D16_D16_D17, FMOV_X0_D16, RET]
            .map(u32::to_le_bytes)
            .concat()
    );
    #[rustfmt::skip]
    let compare = [
        2, 4,
        BITS_TO_FLOAT, 0, 0, CELL_BITS,
        BITS_TO_FLOAT, 1, 0, CELL_BITS,
        FLOAT_COMPARE64, 2, 3, FLOAT_UNORDERED_OR_EQUAL,
    ];
    assert_eq!(
        binding::aarch64_integer_leaf(&compare),
        [
            FMOV_D16_X0,
            FMOV_D17_X1,
            FCMP_D16_D17,
            CSET_X0_EQ,
            CSINC_X0_X0_XZR_VC,
            RET
        ]
        .map(u32::to_le_bytes)
        .concat()
    );
    assert!(binding::aarch64_integer_leaf(&[1, 1, BITS_TO_FLOAT, 0, 0, CELL_BITS]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[2, 2, FLOAT_ADD, 0, 1, CELL_BITS]).is_empty());
}

#[test]
fn aarch64_float_arguments_and_returns_support_linear_and_labeled_functions() {
    const FMOV_W0_S0: u32 = 0x1e26_0000;
    const FMOV_X0_D0: u32 = 0x9e66_0000;
    const FMOV_D0_X0: u32 = 0x9e67_0000;
    for (width, receive) in [(DWORD_BITS, FMOV_W0_S0), (CELL_BITS, FMOV_X0_D0)] {
        let expected = [receive, FMOV_D0_X0, RET].map(u32::to_le_bytes).concat();
        #[rustfmt::skip]
        let linear = [
            0, 0,
            FLOAT_ARGUMENT, 0, 0, width,
            FLOAT_RETURN, 0, 0, 0,
        ];
        #[rustfmt::skip]
        let labeled = [
            0, NO_VALUE,
            BLOCK_LABEL, 0, 0, FLOAT_RETURN_LAYOUT,
            FLOAT_ARGUMENT, 0, 0, width,
            BLOCK_RETURN, 1, 0, 0,
        ];
        assert_eq!(binding::aarch64_integer_leaf(&linear), expected);
        assert_eq!(binding::aarch64_integer_leaf(&labeled), expected);
    }
    assert!(binding::aarch64_integer_leaf(&[1, 0, FLOAT_RETURN, 0, 0, 0]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[0, 0, FLOAT_ARGUMENT, 1, 0, CELL_BITS]).is_empty());
}

#[test]
fn aarch64_float_calls_move_arguments_before_gp_shuffle() {
    // The float pass-through follows the call pattern in isa/aarch64/call.clif.
    const FMOV_D0_X1: u32 = 0x9e67_0020;
    const MOV_X1_X0: u32 = 0xaa00_03e1;
    const BL: u32 = 0x9400_0000;
    const FMOV_W16_S0: u32 = 0x1e26_0010;
    const FMOV_X16_D0: u32 = 0x9e66_0010;
    for (op, width, receive) in [
        (CALL_FLOAT32, DWORD_BITS, FMOV_W16_S0),
        (CALL_FLOAT, CELL_BITS, FMOV_X16_D0),
    ] {
        #[rustfmt::skip]
        let words = [
            1, 5,
            FLOAT_ARGUMENT, 0, 0, width,
            CALL_ARGUMENT, 0, 0, 0,
            CALL_ARGUMENT, 0, 0, 0,
            CALL_ARGUMENT, 1, 0, 0,
            op, 17, 3, 1,
            FLOAT_RETURN, 0, 0, 0,
        ];
        let packet = binding::aarch64_relocatable_block(&words);
        let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
        let (code, _) = body.split_at(u64::from_le_bytes(*length) as usize);
        let sequence = [FMOV_D0_X1, MOV_X1_X0, BL, receive]
            .map(u32::to_le_bytes)
            .concat();
        assert!(
            code.windows(sequence.len())
                .any(|window| window == sequence)
        );
    }
}

#[test]
fn aarch64_atomic_retry_loops_preserve_operands_and_unused_effects() {
    // The acquire/release pairs follow Cranelift's isa/aarch64/atomic-rmw.clif.
    const LDAXR_X2_X0: u32 = 0xc85f_fc02;
    const ADD_X16_X2_X1: u32 = 0x8b01_0050;
    const STLXR_W17_X16_X0: u32 = 0xc811_fc10;
    const LDAXRH_W16_X0: u32 = 0x485f_fc10;
    const SUB_W16_W16_W1: u32 = 0x4b01_0210;
    const STLXRH_W17_W16_X0: u32 = 0x4811_fc10;
    const LDAXRB_W2_X0: u32 = 0x085f_fc02;
    const STLXRB_W17_W1_X0: u32 = 0x0811_fc01;
    const CBNZ_W17_BACK_THREE: u32 = 0x35ff_ffb1;
    const CBNZ_W17_BACK_TWO: u32 = 0x35ff_ffd1;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const MOV_X0_X2: u32 = 0xaa02_03e0;

    let code = |words: &[u32]| {
        words
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        binding::aarch64_integer_leaf(&[2, 2, ATOMIC_ADD, 0, 1, CELL_BITS]),
        code(&[
            LDAXR_X2_X0,
            ADD_X16_X2_X1,
            STLXR_W17_X16_X0,
            CBNZ_W17_BACK_THREE,
            MOV_X0_X2,
            RET,
        ])
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[2, NO_VALUE, ATOMIC_SUB, 0, 1, WORD_BITS, FENCE, 0, 0, 0,]),
        code(&[
            LDAXRH_W16_X0,
            SUB_W16_W16_W1,
            STLXRH_W17_W16_X0,
            CBNZ_W17_BACK_THREE,
            DMB_ISH,
            RET,
        ])
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[2, 2, ATOMIC_EXCHANGE, 0, 1, BYTE_BITS]),
        code(&[
            LDAXRB_W2_X0,
            STLXRB_W17_W1_X0,
            CBNZ_W17_BACK_TWO,
            MOV_X0_X2,
            RET,
        ])
    );
    assert!(binding::aarch64_integer_leaf(&[2, 2, ATOMIC_ADD, 0, 1, u128::BITS.into()]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[0, 0, FENCE, 0, 0, 0]).is_empty());
}

#[test]
fn aarch64_wide_register_pairs_follow_the_target_calling_convention() {
    use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, types};
    use cranelift_codegen::isa::CallConv;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

    const MOV_X0_X1: u32 = 0xaa01_03e0;
    const MOV_X0_X2: u32 = 0xaa02_03e0;
    const MOV_X1_X2: u32 = 0xaa02_03e1;
    const MOV_X1_X3: u32 = 0xaa03_03e1;
    for (call_conv, expected) in [
        (CallConv::SystemV, [MOV_X0_X2, MOV_X1_X3, RET]),
        (CallConv::AppleAarch64, [MOV_X0_X1, MOV_X1_X2, RET]),
    ] {
        let mut function = Function::new();
        function.signature.call_conv = call_conv;
        function.signature.params = vec![AbiParam::new(types::I64), AbiParam::new(types::I128)];
        function.signature.returns.push(AbiParam::new(types::I128));
        let mut context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut function, &mut context);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let wide = builder.block_params(block)[1];
        builder.ins().return_(&[wide]);
        builder.finalize();

        let words = native::machine::aarch64_leaf_words(&function).unwrap();
        assert_eq!(
            binding::aarch64_integer_leaf(&words),
            expected.map(u32::to_le_bytes).concat()
        );
    }
}

#[test]
fn aarch64_call_argument_padding_emits_no_moves() {
    const SUB_SP_CALL_FRAME: u32 = 0xd102_43ff;
    const SAVE_LR: u32 = 0xf900_03fe;
    const BL: u32 = 0x9400_0000;
    const RESTORE_LR: u32 = 0xf940_03fe;
    const ADD_SP_CALL_FRAME: u32 = 0x9102_43ff;
    #[rustfmt::skip]
    let words = [
        4, NO_VALUE,
        CALL_ARGUMENT, 0, 0, 0,
        CALL_ARGUMENT, NO_VALUE, 0, 0,
        CALL_ARGUMENT, 2, 0, 0,
        CALL_ARGUMENT, 3, 0, 0,
        CALL_VOID, 17, 4, 0,
    ];
    let packet = binding::aarch64_relocatable_block(&words);
    let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
    assert_eq!(
        &body[..u64::from_le_bytes(*length) as usize],
        [
            SUB_SP_CALL_FRAME,
            SAVE_LR,
            BL,
            RESTORE_LR,
            ADD_SP_CALL_FRAME,
            RET
        ]
        .map(u32::to_le_bytes)
        .concat()
    );
}

#[test]
fn aarch64_atomic_minmax_and_nand_retry_at_the_load() {
    const LDAXRB_W2_X0: u32 = 0x085f_fc02;
    const SXTB_W17_W2: u32 = 0x1300_1c51;
    const CMP_W17_W1_SXTB: u32 = 0x6b21_823f;
    const CSEL_W16_W2_W1_GT: u32 = 0x1a81_c050;
    const STLXRB_W17_W16_X0: u32 = 0x0811_fc10;
    const CBNZ_W17_BACK_FIVE: u32 = 0x35ff_ff71;
    const MOV_X0_X2: u32 = 0xaa02_03e0;
    const LDAXR_X16_X0: u32 = 0xc85f_fc10;
    const AND_X16_X16_X1: u32 = 0x8a01_0210;
    const MVN_X16_X16: u32 = 0xaa30_03f0;
    const STLXR_W17_X16_X0: u32 = 0xc811_fc10;
    const CBNZ_W17_BACK_FOUR: u32 = 0x35ff_ff91;
    const LDAXRH_W16_X0: u32 = 0x485f_fc10;
    const CMP_W16_W1_UXTH: u32 = 0x6b21_221f;
    const CSEL_W16_W16_W1_LO: u32 = 0x1a81_3210;
    const STLXRH_W17_W16_X0: u32 = 0x4811_fc10;

    for (packet, instructions) in [
        (
            [2, 2, ATOMIC_SIGNED_MAX, 0, 1, BYTE_BITS],
            &[
                LDAXRB_W2_X0,
                SXTB_W17_W2,
                CMP_W17_W1_SXTB,
                CSEL_W16_W2_W1_GT,
                STLXRB_W17_W16_X0,
                CBNZ_W17_BACK_FIVE,
                MOV_X0_X2,
                RET,
            ][..],
        ),
        (
            [2, NO_VALUE, ATOMIC_NAND, 0, 1, CELL_BITS],
            &[
                LDAXR_X16_X0,
                AND_X16_X16_X1,
                MVN_X16_X16,
                STLXR_W17_X16_X0,
                CBNZ_W17_BACK_FOUR,
                RET,
            ],
        ),
        (
            [2, NO_VALUE, ATOMIC_UNSIGNED_MIN, 0, 1, WORD_BITS],
            &[
                LDAXRH_W16_X0,
                CMP_W16_W1_UXTH,
                CSEL_W16_W16_W1_LO,
                STLXRH_W17_W16_X0,
                CBNZ_W17_BACK_FOUR,
                RET,
            ],
        ),
    ] {
        let expected: Vec<_> = instructions
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(binding::aarch64_integer_leaf(&packet), expected);
    }
}

#[test]
fn aarch64_compare_and_swap_preserves_inputs_and_clears_failed_reservations() {
    // Cranelift's isa/aarch64/atomic-cas.clif covers overlapping allocation constraints.
    const LDAXRH_W3_X0: u32 = 0x485f_fc03;
    const CMP_W3_W1_UXTH: u32 = 0x6b21_207f;
    const BNE_TO_CLREX: u32 = 0x5400_0081;
    const STLXRH_W17_W2_X0: u32 = 0x4811_fc02;
    const CBNZ_W17_TO_LOAD: u32 = 0x35ff_ff91;
    const B_OVER_CLREX: u32 = 0x1400_0002;
    const CLREX: u32 = 0xd503_3f5f;
    const MOV_X0_X3: u32 = 0xaa03_03e0;
    const LDAXRB_W16_X0: u32 = 0x085f_fc10;
    const CMP_W16_W1_UXTB: u32 = 0x6b21_021f;
    const STLXRB_W17_W2_X0: u32 = 0x0811_fc02;
    assert_eq!(
        binding::aarch64_integer_leaf(&[3, 3, ATOMIC_CAS_WORD, 0, 1, 2]),
        [
            LDAXRH_W3_X0,
            CMP_W3_W1_UXTH,
            BNE_TO_CLREX,
            STLXRH_W17_W2_X0,
            CBNZ_W17_TO_LOAD,
            B_OVER_CLREX,
            CLREX,
            MOV_X0_X3,
            RET,
        ]
        .map(u32::to_le_bytes)
        .concat()
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[3, NO_VALUE, ATOMIC_CAS_BYTE, 0, 1, 2]),
        [
            LDAXRB_W16_X0,
            CMP_W16_W1_UXTB,
            BNE_TO_CLREX,
            STLXRB_W17_W2_X0,
            CBNZ_W17_TO_LOAD,
            B_OVER_CLREX,
            CLREX,
            RET,
        ]
        .map(u32::to_le_bytes)
        .concat()
    );
    assert!(binding::aarch64_integer_leaf(&[3, 3, ATOMIC_CAS_WORD, 0, 1, 3]).is_empty());
}

#[test]
fn aarch64_ordered_atomic_accesses_emit_ldar_and_stlr() {
    // The encodings follow Cranelift's isa/aarch64/atomic_{load,store}.clif.
    const LDARB_W0_X0: u32 = 0x08df_fc00;
    const LDARH_W0_X0: u32 = 0x48df_fc00;
    const LDAR_W0_X0: u32 = 0x88df_fc00;
    const LDAR_X0_X0: u32 = 0xc8df_fc00;
    const STLRB_W1_X0: u32 = 0x089f_fc01;
    const STLRH_W1_X0: u32 = 0x489f_fc01;
    const STLR_W1_X0: u32 = 0x889f_fc01;
    const STLR_X1_X0: u32 = 0xc89f_fc01;
    for (width, load, store) in [
        (BYTE_BITS, LDARB_W0_X0, STLRB_W1_X0),
        (WORD_BITS, LDARH_W0_X0, STLRH_W1_X0),
        (DWORD_BITS, LDAR_W0_X0, STLR_W1_X0),
        (CELL_BITS, LDAR_X0_X0, STLR_X1_X0),
    ] {
        assert_eq!(
            binding::aarch64_integer_leaf(&[1, 1, ATOMIC_LOAD, 0, 0, width]),
            [load, RET].map(u32::to_le_bytes).concat()
        );
        assert_eq!(
            binding::aarch64_integer_leaf(&[2, NO_VALUE, ATOMIC_STORE, 0, 1, width]),
            [store, RET].map(u32::to_le_bytes).concat()
        );
    }
    assert!(binding::aarch64_integer_leaf(&[1, 1, ATOMIC_LOAD, 0, 1, CELL_BITS]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[2, 2, ATOMIC_STORE, 0, 1, CELL_BITS]).is_empty());
}

#[test]
fn aarch64_discarded_atomic_load_keeps_the_acquire() {
    use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, MemFlags, types};
    use cranelift_codegen::isa::CallConv;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

    const LDAR_X16_X0: u32 = 0xc8df_fc10;
    let mut function = Function::new();
    function.signature.call_conv = CallConv::SystemV;
    function.signature.params.push(AbiParam::new(types::I64));
    let mut context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut function, &mut context);
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    builder.seal_block(block);
    let address = builder.block_params(block)[0];
    builder
        .ins()
        .atomic_load(types::I64, MemFlags::trusted(), address);
    builder.ins().return_(&[]);
    builder.finalize();

    let words = native::machine::aarch64_leaf_words(&function).unwrap();
    assert_eq!(
        binding::aarch64_integer_leaf(&words),
        [LDAR_X16_X0, RET].map(u32::to_le_bytes).concat()
    );
}

#[test]
fn aarch64_bit_operations_use_the_declared_width() {
    const CLZ_X0_X0: u32 = 0xdac0_1000;
    const ORR_W16_W0_BYTE_SENTINEL: u32 = 0x3218_0010;
    const RBIT_W16_W16: u32 = 0x5ac0_0210;
    const CLZ_W0_W16: u32 = 0x5ac0_1200;
    const FMOV_D16_X0: u32 = 0x9e67_0010;
    const CNT_V16_8B: u32 = 0x0e20_5a10;
    const ADDV_B16_V16_8B: u32 = 0x0e31_ba10;
    const UMOV_W0_V16_LOW_BYTE: u32 = 0x0e01_3e00;
    const RBIT_W0_W0: u32 = 0x5ac0_0000;
    const LSR_W0_WORD: u32 = 0x5310_7c00;
    for (op, width, instructions) in [
        (LEADING_ZEROS, CELL_BITS, &[CLZ_X0_X0, RET][..]),
        (
            TRAILING_ZEROS,
            BYTE_BITS,
            &[ORR_W16_W0_BYTE_SENTINEL, RBIT_W16_W16, CLZ_W0_W16, RET][..],
        ),
        (
            POPULATION_COUNT,
            CELL_BITS,
            &[
                FMOV_D16_X0,
                CNT_V16_8B,
                ADDV_B16_V16_8B,
                UMOV_W0_V16_LOW_BYTE,
                RET,
            ][..],
        ),
        (REVERSE_BITS, WORD_BITS, &[RBIT_W0_W0, LSR_W0_WORD, RET][..]),
    ] {
        let packet = [1, 1, op, 0, 0, width];
        let expected: Vec<_> = instructions
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(binding::aarch64_integer_leaf(&packet), expected);
        assert!(binding::aarch64_integer_leaf(&[1, 1, op, 0, 0, u128::BITS.into()]).is_empty());
    }
}

#[test]
fn aarch64_sign_counts_and_rotates_encode_narrow_and_wide_operands() {
    // Narrow sign and oversized rotate cases follow Cranelift's runtests/{cls,rotl,rotr}.clif.
    const CLS_X0_X0: u32 = 0xdac0_1400;
    const SXTB_W16_W0: u32 = 0x1300_1c10;
    const CLS_W0_W16: u32 = 0x5ac0_1600;
    const SUB_W0_BYTE_EXCESS: u32 = 0x5100_6000;
    const ROR_W0_DWORD_MINUS_ONE: u32 = 0x1380_7c00;
    const ROR_X0_X0_X1: u32 = 0x9ac1_2c00;
    const NEG_X17_X1: u32 = 0xcb01_03f1;
    const ROR_X0_X0_X17: u32 = 0x9ad1_2c00;
    const EXTR_X0_X1_X0_17: u32 = 0x93c0_4420;
    const UXTH_W16_W0: u32 = 0x5300_3c10;
    const ORR_W16_REPEATED_WORD: u32 = 0x2a10_4210;
    const UBFX_W0_W16_15_16: u32 = 0x530f_7a00;

    for (packet, instructions) in [
        (
            [1, 1, LEADING_SIGN_BITS, 0, 0, CELL_BITS],
            &[CLS_X0_X0, RET][..],
        ),
        (
            [1, 1, LEADING_SIGN_BITS, 0, 0, BYTE_BITS],
            &[SXTB_W16_W0, CLS_W0_W16, SUB_W0_BYTE_EXCESS, RET],
        ),
        (
            [1, 1, ROTATE_RIGHT, 0, u64::MAX, DWORD_BITS],
            &[ROR_W0_DWORD_MINUS_ONE, RET],
        ),
        (
            [1, 1, ROTATE_RIGHT, 0, WORD_BITS - 1, WORD_BITS],
            &[UXTH_W16_W0, ORR_W16_REPEATED_WORD, UBFX_W0_W16_15_16, RET],
        ),
        (
            [2, 2, ROTATE_RIGHT_REGISTER, 0, 1, CELL_BITS],
            &[ROR_X0_X0_X1, RET],
        ),
        (
            [2, 2, ROTATE_LEFT_REGISTER, 0, 1, CELL_BITS],
            &[NEG_X17_X1, ROR_X0_X0_X17, RET],
        ),
        ([2, 2, FUNNEL_RIGHT, 0, 1, 17], &[EXTR_X0_X1_X0_17, RET]),
    ] {
        let expected: Vec<_> = instructions
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(binding::aarch64_integer_leaf(&packet), expected);
    }
    assert!(binding::aarch64_integer_leaf(&[1, 1, LEADING_SIGN_BITS, 0, 1, CELL_BITS]).is_empty());
    assert!(binding::aarch64_integer_leaf(&[2, 2, FUNNEL_RIGHT, 0, 1, CELL_BITS]).is_empty());
}

#[test]
fn aarch64_bitwise_complements_use_one_instruction() {
    const BIC_X0_X0_X1: u32 = 0x8a21_0000;
    const ORN_W0_W0_W1: u32 = 0x2a21_0000;
    const EON_X0_X0_X1: u32 = 0xca21_0000;
    for (op, width, instruction) in [
        (AND_NOT, CELL_BITS, BIC_X0_X0_X1),
        (OR_NOT, DWORD_BITS, ORN_W0_W0_W1),
        (XOR_NOT, CELL_BITS, EON_X0_X0_X1),
    ] {
        assert_eq!(
            binding::aarch64_integer_leaf(&[2, 2, op, 0, 1, width]),
            [instruction, RET].map(u32::to_le_bytes).concat()
        );
    }
}

#[test]
fn aarch64_narrow_absolute_value_and_shifts_use_declared_bit_width() {
    // The width checks exercise the upper-bit case from Cranelift's runtests/iabs.clif.
    const TST_X0_SIGN: u32 = 0xf241_001f;
    const CSNEG_X0_X0_EQ: u32 = 0xda80_0400;
    const TST_W0_BYTE_SIGN: u32 = 0x7219_001f;
    const CSNEG_W0_W0_EQ: u32 = 0x5a80_0400;
    const UBFX_X0_BYTE: u32 = 0xd340_1c00;
    const UBFX_W16_WORD_SHIFT: u32 = 0x5300_0c30;
    const UXTH_W17_W0: u32 = 0x5300_3c11;
    const LSR_W0_W17_W16: u32 = 0x1ad0_2620;
    assert_eq!(
        binding::aarch64_integer_leaf(&[1, 1, ABS, 0, 0, CELL_BITS]),
        [TST_X0_SIGN, CSNEG_X0_X0_EQ, RET]
            .map(u32::to_le_bytes)
            .concat()
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[1, 1, ABS, 0, 0, BYTE_BITS]),
        [TST_W0_BYTE_SIGN, CSNEG_W0_W0_EQ, UBFX_X0_BYTE, RET]
            .map(u32::to_le_bytes)
            .concat()
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&[2, 2, SHR_REGISTER, 0, 1, WORD_BITS]),
        [UBFX_W16_WORD_SHIFT, UXTH_W17_W0, LSR_W0_W17_W16, RET]
            .map(u32::to_le_bytes)
            .concat()
    );
    assert!(binding::aarch64_integer_leaf(&[1, 1, ABS, 0, 1, CELL_BITS]).is_empty());
}

#[test]
fn aarch64_byte_swap_encodes_each_scalar_width() {
    // Adapted from Cranelift's isa/aarch64/bswap.clif at
    // https://github.com/bytecodealliance/wasmtime/tree/4c22e15bade0a8590d8272f5be85426cb134601b/cranelift/filetests/filetests/isa/aarch64
    const REV16_W0_W0: u32 = 0x5ac0_0400;
    const REV_W0_W0: u32 = 0x5ac0_0800;
    const REV_X0_X0: u32 = 0xdac0_0c00;
    const UBFX_X0_LOW_WORD: u32 = 0xd340_3c00;
    let packet = |width| [1, 1, BYTE_SWAP, 0, 0, width];
    for (width, expected) in [
        (WORD_BITS, &[REV16_W0_W0, UBFX_X0_LOW_WORD, RET][..]),
        (DWORD_BITS, &[REV_W0_W0, RET][..]),
        (CELL_BITS, &[REV_X0_X0, RET][..]),
    ] {
        let code: Vec<_> = expected
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(binding::aarch64_integer_leaf(&packet(width)), code);
    }
    assert!(binding::aarch64_integer_leaf(&packet(BYTE_BITS)).is_empty());
    assert!(binding::aarch64_integer_leaf(&packet(u128::BITS.into())).is_empty());
}

#[test]
fn aarch64_checked_integer_packets_materialize_results_and_flags() {
    const ADDS_X0_X0_X1: u32 = 0xab01_0000;
    const ADDS_XZR_X0_X1: u32 = 0xab01_001f;
    const SUB_X0_X0_X1: u32 = 0xcb01_0000;
    const CSET_X1_HS: u32 = 0x9a9f_37e1;
    const CSET_X0_HS: u32 = 0x9a9f_37e0;
    const SMULL_X16_W0_W1: u32 = 0x9b21_7c10;
    const CMP_X16_W16_SXTW: u32 = 0xeb30_c21f;
    const MOV_W0_W16: u32 = 0x2a10_03e0;
    const CSET_X1_NE: u32 = 0x9a9f_07e1;
    #[rustfmt::skip]
    let pair = |op, width| [
        2, 2,
        op, 0, 1, width,
        OVERFLOW_FLAG, 0, 0, 0,
        RETURN_PAIR, 3, 0, 0,
    ];
    assert_eq!(
        binding::aarch64_integer_leaf(&pair(UNSIGNED_ADD_OVERFLOW, CELL_BITS)),
        [ADDS_X0_X0_X1, CSET_X1_HS, RET]
            .map(u32::to_le_bytes)
            .concat()
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&pair(SIGNED_MUL_OVERFLOW, DWORD_BITS)),
        [
            SMULL_X16_W0_W1,
            CMP_X16_W16_SXTW,
            MOV_W0_W16,
            CSET_X1_NE,
            RET,
        ]
        .map(u32::to_le_bytes)
        .concat()
    );

    #[rustfmt::skip]
    let single = |op, result| [
        2, result,
        op, 0, 1, CELL_BITS,
        OVERFLOW_FLAG, 0, 0, 0,
    ];
    assert_eq!(
        binding::aarch64_integer_leaf(&single(UNSIGNED_ADD_OVERFLOW, 3)),
        [ADDS_XZR_X0_X1, CSET_X0_HS, RET]
            .map(u32::to_le_bytes)
            .concat()
    );
    assert_eq!(
        binding::aarch64_integer_leaf(&single(UNSIGNED_SUB_OVERFLOW, 2)),
        [SUB_X0_X0_X1, RET].map(u32::to_le_bytes).concat()
    );
    assert!(binding::aarch64_integer_leaf(&[0, 0, OVERFLOW_FLAG, 0, 0, 0]).is_empty());
    assert!(
        binding::aarch64_integer_leaf(&[2, 2, UNSIGNED_ADD_OVERFLOW, 0, 1, CELL_BITS]).is_empty()
    );
}

#[test]
fn aarch64_multiword_arithmetic_reuses_processor_carry() {
    const ADDS_X0_X0_X1: u32 = 0xab01_0000;
    const SUBS_X0_X0_X1: u32 = 0xeb01_0000;
    const ADC_X1_X2_X3: u32 = 0x9a03_0041;
    const SBC_X1_X2_X3: u32 = 0xda03_0041;
    #[rustfmt::skip]
    let tape = |low, high, flags| [
        4, 4,
        low, 0, 1, CELL_BITS,
        OVERFLOW_FLAG, 0, 0, 0,
        high, 2, 3, flags,
        RETURN_PAIR, 6, 0, 0,
    ];
    for (low, high, first, second) in [
        (
            UNSIGNED_ADD_OVERFLOW,
            ADD_CARRY,
            ADDS_X0_X0_X1,
            ADC_X1_X2_X3,
        ),
        (
            UNSIGNED_SUB_OVERFLOW,
            SUB_BORROW,
            SUBS_X0_X0_X1,
            SBC_X1_X2_X3,
        ),
    ] {
        assert_eq!(
            binding::aarch64_integer_leaf(&tape(low, high, 5)),
            [first, second, RET].map(u32::to_le_bytes).concat()
        );
        assert!(binding::aarch64_integer_leaf(&tape(low, high, 4)).is_empty());
    }
    assert!(binding::aarch64_integer_leaf(&tape(SIGNED_ADD_OVERFLOW, ADD_CARRY, 5)).is_empty());
}

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
    const SXTB_W16_W0: u32 = 0x1300_1c10;
    const SXTB_W17_W1: u32 = 0x1300_1c31;
    const CBZ_W17_TO_SHARED_SDIV_TRAP: u32 = 0x3400_0091;
    const CMN_W16_BYTE_MIN_MAGNITUDE: u32 = 0x3102_021f;
    const CCMN_W17_1_EQ: u32 = 0x3a41_0a20;
    const BNE_OVER_TRAP: u32 = 0x5400_0041;
    const SDIV_W0_W16_W17: u32 = 0x1ad1_0e00;
    const UBFX_X0_BYTE: u32 = 0xd340_1c00;
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
    let signed_byte_checks = [
        SXTB_W16_W0,
        SXTB_W17_W1,
        CBZ_W17_TO_SHARED_SDIV_TRAP,
        CMN_W16_BYTE_MIN_MAGNITUDE,
        CCMN_W17_1_EQ,
        BNE_OVER_TRAP,
        UDF,
    ];
    assert_eq!(
        encode(SIGNED_DIV, BYTE_BITS, 2),
        [
            signed_byte_checks.as_slice(),
            &[SDIV_W0_W16_W17, UBFX_X0_BYTE, RET]
        ]
        .concat(),
    );
    assert_eq!(
        encode(SIGNED_DIV, BYTE_BITS, 0),
        [signed_byte_checks.as_slice(), &[RET]].concat(),
    );
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
fn aarch64_uses_callee_saved_registers_for_values_live_across_calls() {
    const STP_X19_X20_SP: u32 = 0xa900_53f3;
    const LDP_X19_X20_SP: u32 = 0xa940_53f3;
    const MOV_X19_X0: u32 = 0xaa00_03f3;
    const MOV_X20_X1: u32 = 0xaa01_03f4;
    const MOV_X0_X19: u32 = 0xaa13_03e0;
    const MOV_X1_X20: u32 = 0xaa14_03e1;
    let code = |tape: &[u64]| {
        let packet = binding::aarch64_relocatable_block(tape);
        let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
        let instructions = &body[..u64::from_le_bytes(*length) as usize];
        let (words, remainder) = instructions.as_chunks::<{ size_of::<u32>() }>();
        assert!(remainder.is_empty());
        words
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect::<Vec<_>>()
    };
    let paired = code(&[
        2,
        0,
        CALL_VOID,
        17,
        0,
        0,
        CALL_VOID,
        17,
        0,
        0,
        RETURN_PAIR,
        1,
        0,
        0,
    ]);
    assert!(
        paired
            .windows(3)
            .any(|words| words == [STP_X19_X20_SP, MOV_X19_X0, MOV_X20_X1])
    );
    let results = paired.iter().position(|&word| word == MOV_X0_X19).unwrap();
    assert_eq!(paired[results + 1], MOV_X1_X20);
    assert!(paired[results + 2..].contains(&LDP_X19_X20_SP));

    const LOCAL_BYTES_WITH_NO_ROOM_FOR_CALLEE_SAVES: u64 = 3952;
    let large_frame = code(&[
        2,
        0,
        STACK_SLOT,
        LOCAL_BYTES_WITH_NO_ROOM_FOR_CALLEE_SAVES,
        16,
        0,
        STACK_STORE_QWORD,
        0,
        0,
        0,
        CALL_VOID,
        17,
        0,
        0,
        CALL_VOID,
        17,
        0,
        0,
        RETURN_PAIR,
        1,
        0,
        0,
    ]);
    assert!(!large_frame.contains(&MOV_X19_X0));
    assert!(large_frame.ends_with(&[RET]));
}

#[test]
fn aarch64_symbol_addresses_emit_paired_got_relocations() {
    const ADRP_X0: u32 = 0x9000_0000;
    const LDR_X0_FROM_X0: u32 = 0xf940_0000;
    const ARM64_GOT_PAGE: u64 = 6;
    const ARM64_GOT_LOAD: u64 = 7;
    const SYMBOL: u64 = 42;
    let expected = [ADRP_X0, LDR_X0_FROM_X0, RET]
        .map(u32::to_le_bytes)
        .concat();
    let relocations = [
        0,
        ARM64_GOT_PAGE,
        SYMBOL,
        size_of::<u32>() as u64,
        ARM64_GOT_LOAD,
        SYMBOL,
    ]
    .map(u64::to_le_bytes)
    .concat();
    for operation in [SYMBOL_RELATIVE, SYMBOL_GOT] {
        let request = [0, 0, operation, SYMBOL, 0, 0];
        assert!(binding::aarch64_integer_leaf(&request).is_empty());
        let packet = binding::aarch64_relocatable_block(&request);
        let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
        let (code, records) = body.split_at(u64::from_le_bytes(*length) as usize);
        assert_eq!(code, expected);
        assert_eq!(records, relocations);
    }
    assert!(binding::aarch64_relocatable_block(&[0, 0, SYMBOL_GOT, u64::MAX, 0, 0]).is_empty());
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
fn aarch64_comparison_flags_survive_unrelated_stack_stores() {
    const CELL_BYTES: u64 = CELL_BITS / BYTE_BITS;
    const SUB_SP_16: u32 = 0xd100_43ff;
    const CMP_X0_ZERO: u32 = 0xf100_001f;
    const STR_X1_SP: u32 = 0xf900_03e1;
    const B_NE_FORWARD_FOUR: u32 = 0x5400_0081;
    const MOV_X0_X1: u32 = 0xaa01_03e0;
    const ADD_SP_16: u32 = 0x9100_43ff;
    const CSET_X2_EQ: u32 = 0x9a9f_17e2;

    #[rustfmt::skip]
    let packet = |stored| [
        2, 0,
        STACK_SLOT, CELL_BYTES, CELL_BYTES, 0,
        COMPARE_IMMEDIATE, 0, 0, EQUAL,
        STACK_STORE_QWORD, stored, 0, 0,
        BRANCH_ZERO, 3, 0, 0,
        EARLY_RETURN, 1, 0, 5,
    ];
    let expected = [
        SUB_SP_16,
        CMP_X0_ZERO,
        STR_X1_SP,
        B_NE_FORWARD_FOUR,
        MOV_X0_X1,
        ADD_SP_16,
        RET,
        ADD_SP_16,
        RET,
    ];
    assert_eq!(
        binding::aarch64_integer_leaf(&packet(1)),
        expected.map(u32::to_le_bytes).concat()
    );
    let stored_comparison = binding::aarch64_integer_leaf(&packet(3));
    assert!(
        stored_comparison
            .as_chunks::<{ size_of::<u32>() }>()
            .0
            .contains(&CSET_X2_EQ.to_le_bytes())
    );
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
fn aarch64_promotes_narrow_cfg_cells_alongside_explicit_slots() {
    const CELL_BYTES: u64 = CELL_BITS / BYTE_BITS;
    const SLOT_BYTES: u64 = 10 * CELL_BYTES;
    const SUB_SP_96: u32 = 0xd101_83ff;
    const STR_X0_SP: u32 = 0xf900_03e0;
    const UXTB_W8_W0: u32 = 0x5300_1c08;
    const MOV_X0_X8: u32 = 0xaa08_03e0;
    const LDRB_W0_SP_16: u32 = 0x3940_43e0;
    const STR_X0_SP_16: u32 = 0xf900_0be0;
    const ADD_SP_96: u32 = 0x9101_83ff;

    #[rustfmt::skip]
    let packet = |op| [
        1, NO_VALUE,
        STACK_SLOT, CELL_BYTES, CELL_BYTES, 0,
        STACK_SLOT, SLOT_BYTES, 2 * CELL_BYTES, 0,
        BLOCK_LABEL, 0, 2, 1,
        STACK_STORE_QWORD, 0, 0, 0,
        op, 0, 1, 0,
        STACK_LOAD, 1, 0, BYTE_BITS,
        BLOCK_RETURN, 6, 0, 0,
    ];
    let expected = [SUB_SP_96, STR_X0_SP, UXTB_W8_W0, MOV_X0_X8, ADD_SP_96, RET];
    assert_eq!(
        binding::aarch64_integer_leaf(&packet(STACK_STORE_BYTE)),
        expected.map(u32::to_le_bytes).concat()
    );
    let mixed = [
        SUB_SP_96,
        STR_X0_SP,
        STR_X0_SP_16,
        LDRB_W0_SP_16,
        ADD_SP_96,
        RET,
    ];
    assert_eq!(
        binding::aarch64_integer_leaf(&packet(STACK_STORE_QWORD)),
        mixed.map(u32::to_le_bytes).concat()
    );
}

#[test]
fn aarch64_cfg_homes_survive_calls_in_callee_saved_registers() {
    const CELL_BYTES: u64 = CELL_BITS / BYTE_BITS;
    const STR_X19_SP: u32 = 0xf900_03f3;
    const LDR_X19_SP: u32 = 0xf940_03f3;
    const MOV_X19_X0: u32 = 0xaa00_03f3;
    const MOV_X0_X19: u32 = 0xaa13_03e0;
    const BL: u32 = 0x9400_0000;
    const CALL_TARGET: u64 = 17;
    #[rustfmt::skip]
    let tape = [
        1, NO_VALUE,
        STACK_SLOT, CELL_BYTES, CELL_BYTES, 0,
        BLOCK_LABEL, 0, 1, 1,
        STACK_STORE_QWORD, 0, 0, 0,
        CALL_VOID, CALL_TARGET, 0, 0,
        STACK_LOAD, 0, 0, CELL_BITS,
        BLOCK_RETURN, 5, 0, 0,
    ];
    let packet = binding::aarch64_relocatable_block(&tape);
    let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
    let code = &body[..u64::from_le_bytes(*length) as usize];
    assert!(code.windows(4 * size_of::<u32>()).any(|window| {
        window
            == [STR_X19_SP, MOV_X19_X0, BL, MOV_X0_X19]
                .map(u32::to_le_bytes)
                .concat()
    }));
    assert!(
        code.windows(size_of::<u32>())
            .any(|window| window == LDR_X19_SP.to_le_bytes())
    );
}

#[test]
fn aarch64_spilling_keeps_profitable_callee_saved_block_homes() {
    const LIVE_CONSTANTS: u64 = 20;
    const RELOADS: u64 = 3;
    const CELL_BYTES: u64 = CELL_BITS / BYTE_BITS;
    const TARGET: u64 = 17;
    const MOV_X19_X0: u32 = 0xaa00_03f3;
    const LDR_X19_SP: u32 = 0xf940_03f3;

    let mut records = vec![
        [STACK_SLOT, CELL_BYTES, CELL_BYTES, 0],
        [BLOCK_LABEL, 0, 1, 1],
        [STACK_STORE_QWORD, 0, 0, 0],
        [CALL_VOID, TARGET, 0, 0],
    ];
    let first = records.len() as u64 + 1;
    for i in 0..LIVE_CONSTANTS {
        records.push([CONSTANT, i, 0, 0]);
    }
    let mut sum = first;
    for value in first + 1..first + LIVE_CONSTANTS {
        let next = records.len() as u64 + 1;
        records.push([ADD, sum, value, CELL_BITS]);
        sum = next;
    }
    for _ in 0..RELOADS {
        let reload = records.len() as u64 + 1;
        records.push([STACK_LOAD, 0, 0, CELL_BITS]);
        records.push([ADD, sum, reload, CELL_BITS]);
        sum = reload + 1;
    }
    records.push([BLOCK_RETURN, sum, 0, 0]);
    let tape: Vec<_> = [1, NO_VALUE]
        .into_iter()
        .chain(records.into_iter().flatten())
        .collect();
    let packet = binding::aarch64_relocatable_block(&tape);
    let (length, body) = packet.split_first_chunk::<{ size_of::<u64>() }>().unwrap();
    let instructions = &body[..u64::from_le_bytes(*length) as usize];
    let (words, remainder) = instructions.as_chunks::<{ size_of::<u32>() }>();
    assert!(remainder.is_empty());
    assert!(words.contains(&MOV_X19_X0.to_le_bytes()));
    assert!(words.contains(&LDR_X19_SP.to_le_bytes()));
}

#[test]
fn aarch64_recreates_spilled_zero_as_a_store_source_and_arithmetic_operand() {
    const LIVE_CONSTANTS: u64 = 20;
    const STR_XZR_X0: u32 = 0xf900_001f;
    const MOV_X13_XZR: u32 = 0xaa1f_03ed;
    const ADD_X0_X13_5: u32 = 0x9100_15a0;

    let mut records = Vec::new();
    for value in 1..=LIVE_CONSTANTS {
        records.push([CONSTANT, value, 0, 0]);
    }
    let zero = records.len() as u64 + 1;
    records.push([CONSTANT, 0, 0, 0]);
    records.push([STORE_QWORD, 0, zero, 0]);
    let mut sum = records.len() as u64 + 1;
    records.push([ADD_IMMEDIATE, zero, 5, CELL_BITS]);
    for value in 1..=LIVE_CONSTANTS {
        let next = records.len() as u64 + 1;
        records.push([ADD, sum, value, CELL_BITS]);
        sum = next;
    }
    let tape: Vec<_> = [1, sum]
        .into_iter()
        .chain(records.into_iter().flatten())
        .collect();
    let code = binding::aarch64_integer_leaf(&tape);
    let (words, remainder) = code.as_chunks::<{ size_of::<u32>() }>();
    assert!(remainder.is_empty());
    assert!(
        words.windows(3).any(|window| {
            window == [STR_XZR_X0, MOV_X13_XZR, ADD_X0_X13_5].map(u32::to_le_bytes)
        })
    );
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
fn aarch64_scalar_functions_reach_elf_and_macho_object_emission() {
    const LEAF: &str = "
        fn main(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {
            wrapping_add(wrapping_mul(a, h), b ^ c ^ d ^ e ^ f ^ g)
        }
    ";
    const FLOAT: &str = "fn main(x: f64) -> f64 { x + x }";
    const TEXT: &str = r#"fn main() -> String { "vesper-aarch64-got" }"#;
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
    static RELOCATED: AtomicUsize = AtomicUsize::new(0);
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
    let text = checked(TEXT);
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
            compile(&text, triple),
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
                    RELOCATED.fetch_add(1, Relaxed);
                }
                Some(bytes)
            }
        })
        .is_ok()
    );
    for ((triple, format), (baseline, float_baseline, call_baseline, text_baseline)) in
        TRIPLES.into_iter().zip(baselines)
    {
        let bytes = compile(&leaf, triple);
        assert!(
            bytes != baseline,
            "{triple} did not select the machine emitter"
        );
        assert!(
            compile(&float, triple) != float_baseline,
            "{triple} did not select the float machine emitter"
        );

        let file = object::File::parse(bytes.as_slice()).unwrap();
        assert_eq!(file.architecture(), Architecture::Aarch64);
        assert_eq!(file.format(), format);
        let code_section = file
            .sections()
            .find(|section| section.kind() == SectionKind::Text)
            .unwrap();
        assert!(code_section.data().unwrap().ends_with(&RET.to_le_bytes()));

        let caller_bytes = compile(&caller, triple);
        let text_relocations = |bytes: &[u8]| {
            let file = object::File::parse(bytes).unwrap();
            file.sections()
                .filter(|section| section.kind() == SectionKind::Text)
                .flat_map(|section| section.relocations())
                .map(|(_, reloc)| (reloc.flags(), reloc.addend()))
                .collect::<Vec<_>>()
        };
        let expected = text_relocations(&call_baseline);
        assert_eq!(expected.len(), 2, "{triple}");
        assert_eq!(text_relocations(&caller_bytes), expected, "{triple}");

        let expected = text_relocations(&text_baseline);
        assert_eq!(expected.len(), 2, "{triple}");
        assert_eq!(
            text_relocations(&compile(&text, triple)),
            expected,
            "{triple}"
        );
    }
    assert_eq!(RELOCATED.load(Relaxed), 2 * TRIPLES.len());
    assert_eq!(SELECTED.load(Relaxed), 5 * TRIPLES.len());
}
