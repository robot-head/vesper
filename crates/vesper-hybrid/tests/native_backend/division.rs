use super::binding;
use cranelift_codegen::{
    ir::{AbiParam, InstBuilder, Opcode, Type, types},
    settings,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;

const MACHINE_CONSTANT: u64 = 0;
const MACHINE_TRUNCATE: u64 = 15;
const MACHINE_SIGNED_DIV: u64 = 46;
const MACHINE_UNSIGNED_DIV: u64 = 47;
const MACHINE_SIGNED_REM: u64 = 48;
const MACHINE_UNSIGNED_REM: u64 = 49;

fn division_bytes(op: u64, width: Type, divisor: u64) -> Vec<u8> {
    let width = u64::from(width.bits());
    if width == u64::from(u64::BITS) {
        return binding::x86_64_integer_block(&[
            1,
            2,
            MACHINE_CONSTANT,
            divisor,
            0,
            0,
            op,
            0,
            1,
            width,
        ]);
    }
    binding::x86_64_integer_block(&[
        1,
        3,
        MACHINE_TRUNCATE,
        0,
        u64::from(u64::BITS),
        width,
        MACHINE_CONSTANT,
        divisor,
        0,
        0,
        op,
        1,
        2,
        width,
    ])
}

fn uses_hardware_division(code: &[u8]) -> bool {
    const UNARY_GROUP: u8 = 0xf7;
    const REGISTER_DIVISION_MODRM: u8 = 0xf0;
    const REGISTER_DIVISION_MASK: u8 = 0xf0;
    code.windows(2).any(|pair| {
        pair[0] == UNARY_GROUP && pair[1] & REGISTER_DIVISION_MASK == REGISTER_DIVISION_MODRM
    })
}

#[test]
fn division_by_power_of_two_matches_cranelift() {
    type Entry = unsafe extern "C" fn(u64) -> u64;
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(settings::builder()))
        .unwrap();
    let mut module = JITModule::new(JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    ));
    let alignment = u64::from(module.isa().function_alignment().minimum);
    for width in [types::I32, types::I64] {
        let sign = 1u64 << (width.bits() - 1);
        for (opcode, machine) in [
            (Opcode::Sdiv, MACHINE_SIGNED_DIV),
            (Opcode::Udiv, MACHINE_UNSIGNED_DIV),
            (Opcode::Srem, MACHINE_SIGNED_REM),
            (Opcode::Urem, MACHINE_UNSIGNED_REM),
        ] {
            let mut divisors = vec![1u64, 2, 256, sign / 2, sign];
            if matches!(opcode, Opcode::Sdiv | Opcode::Srem) {
                divisors.extend([2u64.wrapping_neg(), 256u64.wrapping_neg()]);
            }
            if opcode == Opcode::Srem {
                divisors.push(u64::MAX);
            }
            for divisor in divisors {
                let bytes = division_bytes(machine, width, divisor);
                assert!(!bytes.is_empty(), "{opcode} {width} {divisor:#x}");
                assert!(
                    !uses_hardware_division(&bytes),
                    "{opcode} {width} {divisor:#x}"
                );

                let mut context = module.make_context();
                context
                    .func
                    .signature
                    .params
                    .push(AbiParam::new(types::I64));
                context
                    .func
                    .signature
                    .returns
                    .push(AbiParam::new(types::I64));
                let mut state = FunctionBuilderContext::new();
                let mut b = FunctionBuilder::new(&mut context.func, &mut state);
                let entry = b.create_block();
                b.append_block_params_for_function_params(entry);
                b.switch_to_block(entry);
                let input = b.block_params(entry)[0];
                let value = if width == types::I32 {
                    b.ins().ireduce(width, input)
                } else {
                    input
                };
                let rhs = b.ins().iconst(width, divisor as i64);
                let result = match opcode {
                    Opcode::Sdiv => b.ins().sdiv(value, rhs),
                    Opcode::Udiv => b.ins().udiv(value, rhs),
                    Opcode::Srem => b.ins().srem(value, rhs),
                    _ => b.ins().urem(value, rhs),
                };
                let result = if width == types::I32 {
                    b.ins().uextend(types::I64, result)
                } else {
                    result
                };
                b.ins().return_(&[result]);
                b.seal_all_blocks();
                b.finalize();

                let signature = &context.func.signature;
                let reference = module.declare_anonymous_function(signature).unwrap();
                let candidate = module.declare_anonymous_function(signature).unwrap();
                module.define_function(reference, &mut context).unwrap();
                module
                    .define_function_bytes(candidate, alignment, &bytes, &[])
                    .unwrap();
                module.finalize_definitions().unwrap();
                let [reference, candidate] = [reference, candidate].map(|id| unsafe {
                    std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(id))
                });
                for input in [
                    0,
                    1,
                    17,
                    255,
                    256,
                    257,
                    sign - 1,
                    sign,
                    sign + 1,
                    u64::MAX - 256,
                    u64::MAX,
                ] {
                    assert_eq!(
                        unsafe { candidate(input) },
                        unsafe { reference(input) },
                        "{opcode} {width} {input:#x} {divisor:#x}"
                    );
                }
            }
        }
    }
    unsafe { module.free_memory() };
}

#[test]
fn trapping_and_general_divisors_keep_hardware_division() {
    for width in [types::I32, types::I64] {
        for divisor in [0, 3, u64::MAX] {
            let bytes = division_bytes(MACHINE_SIGNED_DIV, width, divisor);
            assert!(uses_hardware_division(&bytes), "{width} {divisor:#x}");
        }
    }
}
