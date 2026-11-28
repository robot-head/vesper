//! Standalone Stage 2 x86-64 ELF objects (SH7, #2209): Stage 2 builds
//! @vesper/stage2-native, which compiles scalar, aggregate and enum entries
//! to System V x86-64 objects, and a C caller links them and checks every
//! result.
use super::support::*;
use std::path::Path;

/// The package Stage 2 builds into the native object compiler.
const STAGE2_NATIVE_PACKAGE: &str = "packages/vesper-stage2-native";

pub fn check_stage2_native_object(stage2: &Compiler, dir: &Path) {
    let native_out = dir.join("stage2_native.wasm");
    let native_bytes = stage2
        .build(dir, &[STAGE2_NATIVE_PACKAGE, "/out/stage2_native.wasm"], &native_out)
        .unwrap_or_else(|e| panic!("{} building @vesper/stage2-native:\n{e}", stage2.label));
    let native = Compiler::new("stage2-native", native_bytes, None);
    use object::{Object, ObjectSection, ObjectSymbol};

    let package = dir.join("native-scalar");
    std::fs::create_dir_all(package.join("src")).unwrap();
    std::fs::write(
        package.join("Loom.toml"),
        "[package]\nname = \"@vesper-test/native-scalar\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        package.join("src/lib.ves"),
        "pub fn mix(x: u64, y: u64) -> u64 { wrapping_add(wrapping_mul(x, 5), y ^ 11) }\n\
         pub fn choose(x: u64, y: u64) -> u64 {\n\
             wrapping_add(\n\
                 if x < y { if x == 0 { y } else { wrapping_mul(x, 3) } } else { y ^ 8 },\n\
                 if y >= 2 && x != 0 { x } else { y }\n\
             )\n\
         }\n\
         fn twist(x: u64, y: u64) -> u64 { wrapping_mul(x ^ 5, wrapping_add(y, 1)) }\n\
         fn factorial(x: u64) -> u64 {\n\
             if x <= 1 { 1 } else { wrapping_mul(x, factorial(wrapping_sub(x, 1))) }\n\
         }\n\
         pub fn calls(x: u64, y: u64) -> u64 {\n\
             wrapping_add(twist(y, x), wrapping_add(twist(x, y), factorial(x & 7)))\n\
         }\n\
         fn stack_sum(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {\n\
             wrapping_add(twist(a, b), wrapping_add(twist(c, d), wrapping_add(twist(e, f), twist(g, h))))\n\
         }\n\
         pub fn stack_args(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64) -> u64 {\n\
             wrapping_add(stack_sum(h, g, f, e, d, c, b, a), if g < h { h } else { g })\n\
         }\n\
         fn offset(a: i64, b: i64) -> i64 {\n\
             if a < b { wrapping_add(a, -7) } else { wrapping_sub(a, -11) }\n\
         }\n\
         pub fn select_signed(a: i64, b: u64) -> i64 {\n\
             let right = wrapping_i64(b);\n\
             let a = if !(a > right) { offset(a, right) }\n\
                 else { wrapping_i64(wrapping_u64(a) ^ b) };\n\
             let _ = wrapping_add(a, 1);\n\
             a\n\
         }\n\
         pub fn mutate(a: i64, b: i64) -> i64 {\n\
             let mut x = a;\n\
             let original = x;\n\
             if b < 0 { x = offset(x, b); } else { x = wrapping_sub(x, b); };\n\
             if b == 0 { x = -20; };\n\
             let mut y = x;\n\
             y = wrapping_add(y, b);\n\
             x = offset(y, x);\n\
             wrapping_sub(x, original)\n\
         }\n\
         pub fn sum_odd(n: u64, seed: u64) -> u64 {\n\
             let mut i = n & 31;\n\
             let mut acc = seed;\n\
             loop {\n\
                 if i == 0 { break acc; };\n\
                 i = wrapping_sub(i, 1);\n\
                 if (i & 1) == 0 { continue; };\n\
                 acc = wrapping_add(acc, i);\n\
             }\n\
         }\n\
         pub fn range_score(lo: u64, hi: u64, stop: u64) -> u64 {\n\
             let mut score: u64 = 0;\n\
             for i in lo..=hi {\n\
                 if i == stop { return score; };\n\
                 score = wrapping_add(score, i);\n\
             };\n\
             score\n\
         }\n\
         fn invert_truth(value: bool) -> bool { !value }\n\
         pub fn bit_truth(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, flip: bool) -> bool {\n\
             let equal = ~a == b;\n\
             if flip { invert_truth(equal) } else { equal }\n\
         }\n\
         fn narrow_call(x: u8, a: u8, b: u8) -> i32 {\n\
             if a < x { widen_i32(~wrapping_add(a, b)) } else { widen_i32(wrapping_i8(x)) }\n\
         }\n\
         pub fn mixed_narrow(a: u8, b: i16, c: u32, d: u8, e: u8, f: u8, g: i8) -> i64 {\n\
             let x = wrapping_add(a, d);\n\
             let mut y = widen_i16(g);\n\
             if y < b { y = wrapping_add(y, b); };\n\
             let z = wrapping_mul(c, widen_u32(x));\n\
             wrapping_add(wrapping_add(widen_i64(y), widen_i64(z)), widen_i64(narrow_call(x, e, f)))\n\
         }\n\
         fn scale_reply(scale: bool, seed: i64) -> i64 {\n\
             match scale { true => wrapping_mul(seed, 13), false => wrapping_add(seed, 21) }\n\
         }\n\
         pub fn match_score(x: i64, seed: i64, scale: bool) -> i64 {\n\
             let value = match x {\n\
                 -9223372036854775808 => return seed,\n\
                 -9 | -8 => wrapping_add(seed, 3),\n\
                 0 => 17,\n\
                 9223372036854775807 => wrapping_add(seed, 5),\n\
                 fallback => wrapping_sub(fallback, seed),\n\
             };\n\
             scale_reply(scale, value)\n\
         }\n\
         pub fn integer_ops(x: i16, amount: u64) -> i16 {\n\
             let quotient = x / -5;\n\
             let remainder = x % -8;\n\
             let right = x >> (amount & 15);\n\
             let left = x << 14;\n\
             wrapping_add(wrapping_sub(quotient, remainder), left ^ right)\n\
         }\n\
         pub fn proven_arithmetic(x: i8) -> i64 {\n\
             let low: i8 = -128;\n\
             let product = (x & 1) * low;\n\
             let negative = -(x & 15);\n\
             let zero = product - product;\n\
             let constant = 7 * 3 + 1;\n\
             wrapping_add(widen_i64(negative),\n\
                          wrapping_add(widen_i64(product), wrapping_add(widen_i64(zero), constant)))\n\
         }\n\
         pub fn factored_bounds(x: i64, amount: i64) -> i8 {\n\
             let quotient = x / (((amount & 7) * 2) + 1);\n\
             let remainder = x % ((amount & 3) - 4);\n\
             let shifted = quotient >> (((amount & 3) * 2) + 1);\n\
             let low: i64 = -128;\n\
             narrow_i8(low + ((wrapping_add(shifted, remainder) & 31) * 4))\n\
         }\n\
         fn magnitude(a: f32, b: f32) -> f32 { sqrt(a * a + b * b) }\n\
         pub fn floating_call(scale: f64, mode: i64, a: f32, b: f32) -> f64 {\n\
             let mut result: f64 = widen_f64(magnitude(a, b));\n\
             if mode < 0 { result = result / scale; } else { result = result * scale; };\n\
             result + 2.5\n\
         }\n\
         fn apply_delta(inout value: u64, delta: u64, active: bool) {\n\
             if !active { return (); };\n\
             value = wrapping_add(value, delta);\n\
         }\n\
         pub fn procedure_value(seed: u64, delta: u64, active: bool) -> u64 {\n\
             let mut value = seed;\n\
             let prior = value;\n\
             let done = apply_delta(inout value, delta, active);\n\
             done;\n\
             apply_delta(value, 1, true);\n\
             wrapping_add(wrapping_mul(prior, 3), value)\n\
         }\n\
         fn tuple_tick(inout state: i64) -> i64 {\n\
             let prior = state;\n\
             state = wrapping_add(state, 1);\n\
             prior\n\
         }\n\
         fn twice_field(value: i64) -> i64 { wrapping_mul(value, 2) }\n\
         pub fn tuple_value(a: i64, b: i64, flag: bool) -> i64 {\n\
             let mut state = a;\n\
             let tuple = (tuple_tick(state), b, tuple_tick(state));\n\
             let alias = tuple;\n\
             if flag {\n\
                 let (first, _, last) = alias;\n\
                 wrapping_add(wrapping_mul(first, 5), last)\n\
             } else { wrapping_sub(twice_field(alias.1), state) }\n\
         }\n\
         copy struct FieldValues { left: i64, right: i64, bias: i64 }\n\
         pub fn record_value(a: i64, b: i64, flag: bool) -> i64 {\n\
             let mut state = a;\n\
             let record = FieldValues { right: tuple_tick(state), bias: b, left: tuple_tick(state) };\n\
             let alias = record;\n\
             if flag { wrapping_add(twice_field(alias.left), alias.bias) }\n\
             else { wrapping_sub(alias.right, alias.left) }\n\
         }\n\
         pub fn constant_after_branch(flag: bool) -> i64 {\n\
             let base = 9223372036854775799;\n\
             let step = if flag { 5 } else { 7 };\n\
             let next = base + 1;\n\
             wrapping_add(next, step)\n\
         }\n\
         fn step_scalars(inout byte: i8, inout active: bool, inout single: f32, inout double: f64, step: f32, scale: f64) {\n\
             byte = wrapping_sub(byte, 3);\n\
             active = !active;\n\
             single = single + step;\n\
             double = double * scale;\n\
         }\n\
         pub fn scalar_inout(inout byte: i8, inout active: bool, inout single: f32, inout double: f64, step: f32, scale: f64, change: bool) -> u64 {\n\
             if change { step_scalars(byte, active, single, double, step, scale); };\n\
             let mut local = byte;\n\
             let mut flipped = active;\n\
             let mut s = single;\n\
             let mut d = double;\n\
             step_scalars(local, flipped, s, d, step, scale);\n\
             if flipped { to_bits_f64(d) } else {\n\
                 let bits: u32 = to_bits_f32(s);\n\
                 wrapping_add(widen_u64(bits), wrapping_u64(widen_i64(local)))\n\
             }\n\
         }\n\
         copy struct NestedFields { pair: FieldValues, ready: bool }\n\
         pub fn nested_value(a: i64, b: i64, flag: bool) -> i64 {\n\
             let inner = FieldValues { bias: 3, left: a, right: b };\n\
             let outer = NestedFields { ready: flag, pair: inner };\n\
             let holder = (outer, a);\n\
             let (row, base) = holder;\n\
             let field = row.pair;\n\
             if row.ready { wrapping_add(field.left, base) } else {\n\
                 let right = (NestedFields { ready: true, pair: inner }).pair.right;\n\
                 wrapping_sub(right, row.pair.bias)\n\
             }\n\
         }\n\
         copy struct ComparableFields { sample: f64, tag: i8, ready: bool }\n\
         pub fn product_equality(a: u64, b: u64, x: u64, y: u64, flags: u64) -> bool {\n\
             let left = ComparableFields { sample: from_bits_f64(a), tag: wrapping_i8(x), ready: true };\n\
             let right = ComparableFields { ready: (flags & 2) != 0, tag: wrapping_i8(y), sample: from_bits_f64(b) };\n\
             let left = (left, ((), 3));\n\
             let right = (right, ((), 3));\n\
             if (flags & 1) != 0 { left == right } else { left != right }\n\
         }\n\
         copy struct ResultFields { tag: i8, single: f32, wide: f64 }\n\
         pub fn result_value(a: i64, b: u64, c: u64, d: u64, e: u64, f: u64, flag: bool, single: f32, wide: f64) -> (u64, ResultFields, (), i64) {\n\
             let state = ResultFields { wide: wide, single: single, tag: wrapping_i8(a) };\n\
             if flag { return (wrapping_add(wrapping_add(e, f), b), state, (), wrapping_sub(a, 3)); };\n\
             (twist(c, d), state, (), wrapping_add(a, 7))\n\
         }\n\
         fn literal_shift(value: f32) -> f32 { value + 0.25 }\n\
         pub fn float_literals(flag: bool) -> (f32, f32, f32, f32, f32, f32, f32) {\n\
             let even: f32 = 1.000000059604644775390625;\n\
             let odd: f32 = 1.000000178813934326171875;\n\
             let subnormal: f32 = 1.401298464324817e-45;\n\
             let underflow: f32 = -1.0e-50;\n\
             let finite: f32 = 3.4028234663852886e38;\n\
             let overflow: f32 = 3.4028236692093846e38;\n\
             let x: f32 = if flag { 1.5 } else { 1.75 };\n\
             (even, odd, subnormal, underflow, finite, overflow, literal_shift(x))\n\
         }\n\
         copy struct CallFields { tag: i8, sample: f32, active: bool }\n\
         fn sample_cells(inout state: i64, step: i64, sample: f32, active: bool) -> CallFields {\n\
             let prior = state;\n\
             state = wrapping_add(state, step);\n\
             CallFields { active: active, sample: sample, tag: wrapping_i8(prior) }\n\
         }\n\
         fn pair_cells(inout state: i64, step: i64) -> (i16, u64) {\n\
             let prior = state;\n\
             state = wrapping_add(state, step);\n\
             (wrapping_i16(prior), wrapping_u64(state))\n\
         }\n\
         pub fn aggregate_calls(seed: i64, choose: bool, sample: f32) -> u64 {\n\
             let mut state = seed;\n\
             let first = sample_cells(state, 2, sample, choose);\n\
             let (short, changed) = pair_cells(state, 5);\n\
             let last = sample_cells(state, 19, sample, !choose);\n\
             if first.active {\n\
                 let bits: u32 = to_bits_f32(first.sample);\n\
                 wrapping_add(widen_u64(bits), wrapping_u64(widen_i64(last.tag)))\n\
             } else {\n\
                 let tag_bits = wrapping_u64(widen_i64(first.tag));\n\
                 wrapping_add(wrapping_add(tag_bits, wrapping_u64(widen_i64(short))), changed)\n\
             }\n\
         }\n\
         fn remainder_helper(a: f64, b: f64) -> f64 { a % b }\n\
         pub fn float_remainders(a: f64, b: f64, x: f32, y: f32) -> f64 {\n\
             let first = a % b;\n\
             let second = remainder_helper(b, a);\n\
             (first + second) + widen_f64(x % y)\n\
         }\n\
         fn fmod(a: f64, b: f64) -> f64 { a % b }\n\
         pub fn floating_math(x: f64, y: f32) -> (f64, f64, f64, f64, f64, f64, f64, f32, f32, f32, f32, f32, f32, f32) {\n\
             let fused64 = fma(1.0000000000000002, 0.9999999999999998, -1.0);\n\
             let fused32: f32 = fma(1.00000011920928955078125, 0.99999988079071044921875, -1.0);\n\
             let minimum32 = min(-0.0, y);\n\
             (floor(x), ceil(x), trunc(x), round(x), min(-0.0, x), max(0.0, x), fused64,\n\
              floor(y), ceil(y), trunc(y), round(y), minimum32, max(0.0, y), fused32)\n\
         }\n\
         pub fn float_conversions(i: i16, u: u64, x: f32, y: f64) -> (u32, u64, u32, u64, i64, u64, i64, u64) {\n\
             let signed = saturating_i8(y);\n\
             let unsigned = saturating_u16(x);\n\
             (to_bits_f32(widen_f32(i)), to_bits_f64(widen_f64(i)),\n\
              to_bits_f32(nearest_f32(u)), to_bits_f64(nearest_f64(u)),\n\
              widen_i64(signed), widen_u64(unsigned), saturating_i64(x), saturating_u64(y))\n\
         }\n\
         pub fn integer_bits(x: i8, n: i8, y: u16, shift: u16, rotation: i64) -> (i64, u64, u64, u64, i64) {\n\
             let left = bit_shl(y, shift);\n\
             let flags = bit_xor(bit_or(bit_and(left, 255), 256), 3);\n\
             let tag: u32 = 2147483649;\n\
             (widen_i64(bit_shr(x, n)), widen_u64(flags), widen_u64(rotate_left(y, rotation)),\n\
              widen_u64(rotate_right(tag, 33)), widen_i64(!x))\n\
         }\n\
         copy struct NestedCallMarker {}\n\
         copy struct NestedCallBits { measurement: (f32, f64), tag: i8, marker: NestedCallMarker }\n\
         copy struct NestedCallRow { data: NestedCallBits, ready: bool }\n\
         fn nested_result(x: i64, y: f32, ready: bool) -> (NestedCallRow, ((), i16)) {\n\
             let data = NestedCallBits { tag: wrapping_i8(x), marker: NestedCallMarker {}, measurement: (y, widen_f64(y) + 0.5) };\n\
             if ready { return (NestedCallRow { ready: true, data: data }, ((), wrapping_i16(x))); };\n\
             (NestedCallRow { ready: false, data: data }, ((), wrapping_i16(wrapping_add(x, 1))))\n\
         }\n\
         pub fn nested_call_entry(x: i64, y: f32, ready: bool) -> (NestedCallRow, ((), i16), bool, i64) {\n\
             let (earlier, payload) = nested_result(x, y, ready);\n\
             let later = nested_result(wrapping_add(x, 1), y + 1.0, !ready);\n\
             let (later_row, _) = later;\n\
             let equal = earlier.data == later_row.data;\n\
             if earlier.ready { return (earlier, payload, equal, widen_i64(earlier.data.tag)); };\n\
             (earlier, payload, equal, widen_i64(later_row.data.tag))\n\
         }\n\
         pub fn product_inputs(a: u64, b: u64, c: u64, d: u64, row: NestedCallRow, sample: (i16, f32), delta: f64)\n\
             -> (NestedCallRow, (i16, f32), i64, f64, u64) {\n\
             let (tag, reading) = sample;\n\
             let signed = widen_i64(row.data.tag);\n\
             let mixed = if row.ready { wrapping_add(signed, widen_i64(tag)) }\n\
                 else { wrapping_sub(signed, widen_i64(tag)) };\n\
             let (_, wide) = row.data.measurement;\n\
             let scaled = (wide + delta) + widen_f64(reading);\n\
             let sum = wrapping_add(wrapping_add(a, b), wrapping_add(c, d));\n\
             (row, sample, mixed, scaled, sum)\n\
         }\n\
         pub fn product_caller(x: i64, y: f32, ready: bool) -> (NestedCallRow, (i16, f32), i64, f64, u64) {\n\
             let (row, _) = nested_result(x, y, ready);\n\
             let sample = (wrapping_i16(wrapping_add(x, 2)), y + 1.0);\n\
             if ready { return product_inputs(10, 20, 30, 40, row, sample, 1.5); };\n\
             product_inputs(1, 2, 3, 4, row, sample, -0.5)\n\
         }\n\
         fn adjust_sample(inout sample: f32, delta: f32) {\n\
             sample += delta;\n\
             sample *= 2.0;\n\
             sample -= 1.0;\n\
             sample /= 2.0;\n\
             sample %= 3.0;\n\
         }\n\
         pub fn compound_values(a: f64, b: f64, x: f32, choose: bool) -> (f64, f32, u64, i8) {\n\
             let mut wide = a;\n\
             wide += b;\n\
             wide -= 0.5;\n\
             wide *= 2.0;\n\
             wide /= 4.0;\n\
             wide %= 3.0;\n\
             let mut sample = x;\n\
             if choose { adjust_sample(inout sample, 1.25); } else { sample -= 0.25; };\n\
             let mut count: u64 = 6;\n\
             count += 4;\n\
             count *= 3;\n\
             count -= 1;\n\
             count /= 2;\n\
             count %= 6;\n\
             let mut signed: i8 = -9;\n\
             signed /= 2;\n\
             signed %= -3;\n\
             (wide, sample, count, signed)\n\
         }\n\
         pub fn overflowing_compound(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow]] {\n\
             let mut value = a;\n\
             value += b;\n\
             value\n\
         }\n\
         pub fn tuple_match_native(x: i8, y: u16, ready: bool) -> u64 {\n\
             let pair = (x, (y, ready));\n\
             match pair {\n\
                 (-1, (0, true)) | (-2, (1, false)) => 100,\n\
                 (0, (n, true)) | (1, (n, false)) => wrapping_add(200, widen_u64(n)),\n\
                 (_, (n, false)) => widen_u64(n),\n\
                 _ => { let (_, (n, _)) = pair; wrapping_add(1000, widen_u64(n)) },\n\
             }\n\
         }\n\
         copy enum NativeChoice { Empty, Number(i8, bool), Sample { raw: f32, count: u64 } }\n\
         pub fn enum_packer(pick: u64, tag: i8, raw: f32, ready: bool) -> NativeChoice {\n\
             if pick == 0 { return NativeChoice::Number(tag, ready); };\n\
             if pick == 1 { return NativeChoice::Sample { count: 9, raw: raw }; };\n\
             NativeChoice::Empty\n\
         }\n\
         pub fn enum_reader(value: NativeChoice) -> u64 {\n\
             match value {\n\
                 NativeChoice::Number(-1, true) => 100,\n\
                 NativeChoice::Number(tag, ready) => if ready { wrapping_u64(tag) } else { 200 },\n\
                 NativeChoice::Sample { count: n, raw: x } => wrapping_add(n, widen_u64(to_bits_f32(x))),\n\
                 NativeChoice::Empty => 300,\n\
             }\n\
         }\n\
         pub fn enum_pipeline(pick: u64, tag: i8, raw: f32, ready: bool) -> u64 {\n\
             enum_reader(enum_packer(pick, tag, raw, ready))\n\
         }\n\
         fn forwarded_source(tag: i8, sample: f32) -> CallFields {\n\
             CallFields { tag: tag, sample: sample, active: true }\n\
         }\n\
         fn forwarded_update(row: CallFields) -> CallFields {\n\
             CallFields { tag: wrapping_add(row.tag, 1), sample: row.sample + 1.0, active: !row.active }\n\
         }\n\
         fn forwarded_fields(a: CallFields, b: CallFields, prior: CallFields) -> (i64, f32, i64, f32, bool) {\n\
             let same = a.tag == b.tag && to_bits_f32(a.sample) == to_bits_f32(b.sample) && a.active == b.active;\n\
             (widen_i64(a.tag), a.sample, widen_i64(prior.tag), prior.sample, same && prior.active && !a.active)\n\
         }\n\
         pub fn forwarded_results(tag: i8, sample: f32) -> (i64, f32, i64, f32, bool) {\n\
             let prior = forwarded_source(tag, sample);\n\
             let alias = prior;\n\
             let updated = forwarded_update(alias);\n\
             forwarded_fields(updated, updated, prior)\n\
         }\n\
         fn scalar_option(tag: i8, present: bool) -> Option[i8] {\n\
             if present { return Option::Some(tag); }\n\
             Option::None\n\
         }\n\
         fn scalar_result(sample: f32, tag: i8) -> Result[f32, i16] {\n\
             if tag < 0 { return Result::Err(widen_i16(tag)); }\n\
             Result::Ok(sample)\n\
         }\n\
         pub fn generic_enum_pipeline(outcome: bool, sample: f32, tag: i8) -> u64 {\n\
             let option = scalar_option(tag, outcome);\n\
             let value = match option { Option::Some(n) => n, Option::None => 1 };\n\
             match scalar_result(sample, value) {\n\
                 Result::Ok(x) => widen_u64(to_bits_f32(x)),\n\
                 Result::Err(n) => wrapping_u64(n),\n\
             }\n\
         }\n\
         pub fn aggregate_join(swap: bool, x: i16, y: i16, sample: f32) -> (i16, f32, i16, u64) {\n\
             let selected: ((i16, f32), Option[i16]) = if swap {\n\
                 ((y, sample), Option::Some(x))\n\
             } else {\n\
                 let option: Option[i16] = if sample == 0.0 { Option::None } else { Option::Some(y) };\n\
                 ((x, sample), option)\n\
             };\n\
             let ((a, b), option) = selected;\n\
             let result: (i16, u64) = match option {\n\
                 Option::Some(n) => (n, 17),\n\
                 Option::None => (0, 9),\n\
             };\n\
             let (c, marker) = result;\n\
             (a, b, c, marker)\n\
         }\n\
         fn enum_plain_route(present: bool, tag: i8) -> u64 {\n\
             let option: Option[i8] = if present { Option::Some(tag) } else { Option::None };\n\
             match option { Option::None => 17, Option::Some(n) => wrapping_u64(n) }\n\
         }\n\
         fn enum_literal_route(present: bool, tag: i8) -> u64 {\n\
             let option: Option[i8] = if present { Option::Some(tag) } else { Option::None };\n\
             match option {\n\
                 Option::Some(-1) => 23,\n\
                 Option::Some(n) => wrapping_u64(n),\n\
                 Option::None => 29,\n\
             }\n\
         }\n\
         fn enum_observed_route(inout seen: u64, present: bool, tag: i8) -> u64 {\n\
             let observed: Option[i8] = if present { Option::Some(tag) } else { Option::None };\n\
             seen = wrapping_add(seen, 1);\n\
             match observed { Option::None => 39, Option::Some(n) => wrapping_u64(n) }\n\
         }\n\
         fn enum_direct_route(tag: i8) -> u64 {\n\
             match Option::Some(tag) { Option::None => 31, Option::Some(n) => wrapping_u64(n) }\n\
         }\n\
         pub fn enum_routes(inout seen: u64, present: bool, tag: i8) -> (u64, u64, u64, u64) {\n\
             (enum_direct_route(tag), enum_plain_route(present, tag),\n\
              enum_literal_route(present, tag), enum_observed_route(inout seen, present, tag))\n\
         }\n\
         fn unsupported(x: u64) -> u64 {\n\
             let value: Option[(u64, u64)] = Option::Some((x, x));\n\
             match value { Option::Some(_) => 7, _ => 9 }\n\
         }\n\
         pub fn decline(x: u64) -> u64 { unsupported(x) }\n",
    )
    .unwrap();
    let object = dir.join("native-scalar.o");
    let compile = |entry| {
        native
            .prepared
            .run(&Ctx {
                args: vec!["stage2-native", "native-scalar", "/out/native-scalar.o", entry],
                dirs: vec![(dir.into(), ".".into()), (dir.into(), "/out".into())],
                fuel: Some(u64::MAX),
                diag_base: Some(object.clone()),
                ..Ctx::default()
            })
            .expect("Stage 2 native object request")
    };
    assert_eq!(
        compile("mix"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let leaf = dir.join("native-mix.o");
    std::fs::rename(&object, &leaf).unwrap();
    assert_eq!(
        compile("choose"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let branch = dir.join("native-choose.o");
    std::fs::rename(&object, &branch).unwrap();
    assert_eq!(
        compile("calls"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let bytes = std::fs::read(&object).unwrap();
    let file = object::File::parse(bytes.as_slice()).unwrap();
    assert_eq!(file.format(), object::BinaryFormat::Elf);
    assert!(file.symbol_by_name("calls").unwrap().is_global());
    for name in ["twist", "factorial"] {
        let symbol = file.symbol_by_name(name).unwrap();
        assert!(symbol.is_definition() && !symbol.is_global());
    }
    assert_eq!(
        file.section_by_name(".text").unwrap().relocations().count(),
        4
    );
    let calls = dir.join("native-calls.o");
    std::fs::rename(&object, &calls).unwrap();
    assert_eq!(
        compile("stack_args"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let stack = dir.join("native-stack-args.o");
    std::fs::rename(&object, &stack).unwrap();
    assert_eq!(
        compile("select_signed"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let signed = dir.join("native-signed.o");
    std::fs::rename(&object, &signed).unwrap();
    assert_eq!(
        compile("mutate"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let mutable = dir.join("native-mutable.o");
    std::fs::rename(&object, &mutable).unwrap();
    assert_eq!(
        compile("sum_odd"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let loop_object = dir.join("native-loop.o");
    std::fs::rename(&object, &loop_object).unwrap();
    assert_eq!(
        compile("range_score"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let range_object = dir.join("native-range.o");
    std::fs::rename(&object, &range_object).unwrap();
    assert_eq!(
        compile("bit_truth"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let bool_object = dir.join("native-bool.o");
    std::fs::rename(&object, &bool_object).unwrap();
    assert_eq!(
        compile("mixed_narrow"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let narrow_object = dir.join("native-narrow.o");
    std::fs::rename(&object, &narrow_object).unwrap();
    assert_eq!(
        compile("match_score"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let match_object = dir.join("native-match.o");
    std::fs::rename(&object, &match_object).unwrap();
    assert_eq!(
        compile("integer_ops"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let integer_object = dir.join("native-integer.o");
    std::fs::rename(&object, &integer_object).unwrap();
    assert_eq!(
        compile("proven_arithmetic"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let arithmetic_object = dir.join("native-checked-arithmetic.o");
    std::fs::rename(&object, &arithmetic_object).unwrap();
    assert_eq!(
        compile("factored_bounds"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let bounds_object = dir.join("native-bounds.o");
    std::fs::rename(&object, &bounds_object).unwrap();
    assert_eq!(
        compile("floating_call"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let float_object = dir.join("native-float.o");
    std::fs::rename(&object, &float_object).unwrap();
    assert_eq!(
        compile("procedure_value"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let procedure_object = dir.join("native-procedure.o");
    std::fs::rename(&object, &procedure_object).unwrap();
    assert_eq!(
        compile("tuple_value"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let tuple_object = dir.join("native-tuple.o");
    std::fs::rename(&object, &tuple_object).unwrap();
    assert_eq!(
        compile("record_value"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let record_object = dir.join("native-record.o");
    std::fs::rename(&object, &record_object).unwrap();
    assert_eq!(
        compile("constant_after_branch"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let constant_object = dir.join("native-constant.o");
    std::fs::rename(&object, &constant_object).unwrap();
    assert_eq!(
        compile("scalar_inout"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let inout_object = dir.join("native-inout.o");
    std::fs::rename(&object, &inout_object).unwrap();
    assert_eq!(
        compile("nested_value"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let nested_object = dir.join("native-nested.o");
    std::fs::rename(&object, &nested_object).unwrap();
    assert_eq!(
        compile("product_equality"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let equality_object = dir.join("native-equality.o");
    std::fs::rename(&object, &equality_object).unwrap();
    assert_eq!(
        compile("result_value"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let result_object = dir.join("native-result.o");
    std::fs::rename(&object, &result_object).unwrap();
    assert_eq!(
        compile("float_literals"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let literals_object = dir.join("native-literals.o");
    std::fs::rename(&object, &literals_object).unwrap();
    assert_eq!(
        compile("aggregate_calls"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let aggregate_call_object = dir.join("native-aggregate-call.o");
    std::fs::rename(&object, &aggregate_call_object).unwrap();
    assert_eq!(
        compile("float_remainders"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let bytes = std::fs::read(&object).unwrap();
    let file = object::File::parse(bytes.as_slice()).unwrap();
    for name in ["fmod", "fmodf"] {
        let symbol = file.symbol_by_name(name).unwrap();
        assert!(symbol.is_undefined() && symbol.is_global());
    }
    assert!(file.symbol_by_name("remainder_helper").unwrap().is_local());
    let mut call_symbols = Vec::new();
    for (_, relocation) in file.section_by_name(".text").unwrap().relocations() {
        assert_eq!(
            relocation.flags(),
            object::RelocationFlags::Elf {
                r_type: object::elf::R_X86_64_PLT32
            }
        );
        let object::RelocationTarget::Symbol(target) = relocation.target() else {
            panic!("native call must target a symbol");
        };
        call_symbols.push(file.symbol_by_index(target).unwrap().name().unwrap());
    }
    assert_eq!(call_symbols, ["fmod", "remainder_helper", "fmodf", "fmod"]);
    assert_ne!(compile("fmod"), 0);
    assert_eq!(std::fs::read(&object).unwrap(), bytes);
    assert_ne!(compile("decline"), 0);
    assert!(read_stage2_sidecar_notes(&object).contains("unsupported x86-64 ELF entry"));
    assert_eq!(std::fs::read(&object).unwrap(), bytes);
    let remainder_object = dir.join("native-remainder.o");
    std::fs::rename(&object, &remainder_object).unwrap();
    assert_eq!(
        compile("floating_math"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let math_object = dir.join("native-float-math.o");
    std::fs::rename(&object, &math_object).unwrap();
    assert_eq!(
        compile("float_conversions"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let conversions_object = dir.join("native-float-conversions.o");
    std::fs::rename(&object, &conversions_object).unwrap();
    assert_eq!(
        compile("integer_bits"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let bits_object = dir.join("native-integer-bits.o");
    std::fs::rename(&object, &bits_object).unwrap();
    assert_eq!(
        compile("nested_call_entry"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let nested_call_object = dir.join("native-nested-call.o");
    std::fs::rename(&object, &nested_call_object).unwrap();
    assert_eq!(
        compile("product_inputs"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let product_inputs_object = dir.join("native-product-inputs.o");
    std::fs::rename(&object, &product_inputs_object).unwrap();
    assert_eq!(
        compile("product_caller"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let product_caller_object = dir.join("native-product-caller.o");
    std::fs::rename(&object, &product_caller_object).unwrap();
    assert_eq!(
        compile("compound_values"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let compound_bytes = std::fs::read(&object).unwrap();
    assert_ne!(compile("overflowing_compound"), 0);
    assert!(read_stage2_sidecar_notes(&object).contains("unsupported x86-64 ELF entry"));
    assert_eq!(std::fs::read(&object).unwrap(), compound_bytes);
    let compound_object = dir.join("native-compound-writes.o");
    std::fs::rename(&object, &compound_object).unwrap();
    assert_eq!(
        compile("tuple_match_native"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let tuple_match_object = dir.join("native-tuple-match.o");
    std::fs::rename(&object, &tuple_match_object).unwrap();
    assert_eq!(
        compile("enum_pipeline"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let enum_object = dir.join("native-enum-match.o");
    std::fs::rename(&object, &enum_object).unwrap();
    assert_eq!(
        compile("forwarded_results"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let forwarded_object = dir.join("native-forwarded-results.o");
    std::fs::rename(&object, &forwarded_object).unwrap();
    assert_eq!(
        compile("generic_enum_pipeline"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let generic_enum_object = dir.join("native-generic-enum.o");
    std::fs::rename(&object, &generic_enum_object).unwrap();
    assert_eq!(
        compile("aggregate_join"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );
    let aggregate_join_object = dir.join("native-aggregate-join.o");
    std::fs::rename(&object, &aggregate_join_object).unwrap();
    assert_eq!(
        compile("enum_routes"),
        0,
        "{:?}",
        read_stage2_sidecar_notes(&object)
    );

    let caller = dir.join("native-scalar-caller.c");
    let binary = dir.join("native-scalar-caller");
    std::fs::write(
        &caller,
        "#include <stdbool.h>\n\
         #include <stdint.h>\n\
         #include <math.h>\n\
         extern uint64_t mix(uint64_t, uint64_t);\n\
         extern uint64_t choose(uint64_t, uint64_t);\n\
         extern uint64_t calls(uint64_t, uint64_t);\n\
         extern uint64_t stack_args(uint64_t, uint64_t, uint64_t, uint64_t,\n\
                                    uint64_t, uint64_t, uint64_t, uint64_t);\n\
         extern int64_t select_signed(int64_t, uint64_t);\n\
         extern int64_t mutate(int64_t, int64_t);\n\
         extern uint64_t sum_odd(uint64_t, uint64_t);\n\
         extern uint64_t range_score(uint64_t, uint64_t, uint64_t);\n\
         extern bool bit_truth(uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, uint64_t, bool);\n\
         extern int64_t mixed_narrow(uint8_t, int16_t, uint32_t, uint8_t, uint8_t, uint8_t, int8_t);\n\
         extern int64_t match_score(int64_t, int64_t, bool);\n\
         extern int16_t integer_ops(int16_t, uint64_t);\n\
         extern int64_t proven_arithmetic(int8_t);\n\
         extern int8_t factored_bounds(int64_t, int64_t);\n\
         extern double floating_call(double, int64_t, float, float);\n\
         extern uint64_t procedure_value(uint64_t, uint64_t, bool);\n\
         extern int64_t tuple_value(int64_t, int64_t, bool);\n\
         extern int64_t record_value(int64_t, int64_t, bool);\n\
         extern int64_t constant_after_branch(bool);\n\
         extern uint64_t scalar_inout(int64_t *, uint64_t *, float *, double *, float, double, bool);\n\
         extern int64_t nested_value(int64_t, int64_t, bool);\n\
         extern bool product_equality(uint64_t, uint64_t, uint64_t, uint64_t, uint64_t);\n\
         extern uint64_t result_value(uint64_t *, int64_t, uint64_t, uint64_t, uint64_t,\n\
                                      uint64_t, uint64_t, bool, float, double);\n\
         extern uint64_t float_literals(uint64_t *, bool);\n\
         extern uint64_t aggregate_calls(int64_t, bool, float);\n\
         extern double float_remainders(double, double, float, float);\n\
         extern uint64_t floating_math(uint64_t *, double, float);\n\
         extern uint64_t float_conversions(uint64_t *, int16_t, uint64_t, float, double);\n\
         extern uint64_t integer_bits(uint64_t *, int8_t, int8_t, uint16_t, uint16_t, int64_t);\n\
         extern uint64_t nested_call_entry(uint64_t *, int64_t, float, bool);\n\
         extern uint64_t product_inputs(uint64_t *, uint64_t, uint64_t, uint64_t, uint64_t, const uint64_t *, const uint64_t *, double);\n\
         extern uint64_t product_caller(uint64_t *, int64_t, float, bool);\n\
         extern uint64_t compound_values(uint64_t *, double, double, float, bool);\n\
         extern uint64_t tuple_match_native(int8_t, uint16_t, bool);\n\
         extern uint64_t enum_pipeline(uint64_t, int8_t, float, bool);\n\
         extern uint64_t forwarded_results(uint64_t *, int8_t, float);\n\
         extern uint64_t generic_enum_pipeline(bool, float, int8_t);\n\
         extern uint64_t aggregate_join(uint64_t *, bool, int16_t, int16_t, float);\n\
         extern uint64_t enum_routes(uint64_t *, uint64_t *, bool, int8_t);\n\
         int main(void) {\n\
             if (mix(10, 4) != 65 || mix(UINT64_MAX, UINT64_MAX) != UINT64_MAX - 16) return 1;\n\
             if (choose(0, 4) != 8 || choose(2, 4) != 8 || choose(4, 1) != 10\n\
                 || choose(UINT64_MAX, 2) != 9 || choose(2, UINT64_MAX) != 8) return 2;\n\
             if (calls(0, 1) != 15 || calls(6, 2) != 778\n\
                 || calls(UINT64_MAX, UINT64_MAX) != 5040) return 3;\n\
             if (stack_args(1, 2, 3, 4, 5, 6, 7, 8) != 148\n\
                 || stack_args(8, 7, 6, 5, 4, 3, 2, 1) != 62) return 4;\n\
             if (select_signed(-5, 3) != -12 || select_signed(5, UINT64_MAX) != -6\n\
                 || select_signed(INT64_MIN, 0) != INT64_MAX - 6) return 5;\n\
             if (mutate(7, -2) != 2 || mutate(7, 2) != 11 || mutate(7, 0) != -16) return 6;\n\
             if (sum_odd(11, 5) != 30 || sum_odd(32, 42) != 42\n\
                 || sum_odd(31, UINT64_MAX) != 224) return 7;\n\
             if (range_score(7, 4, 1) != 0 || range_score(1, 8, 4) != 6\n\
                 || range_score(UINT64_MAX - 1, UINT64_MAX, 0) != UINT64_MAX - 2) return 8;\n\
             if (!bit_truth(0, UINT64_MAX, 0, 0, 0, 0, false)\n\
                 || bit_truth(UINT64_MAX, 0, 0, 0, 0, 0, true)\n\
                 || !bit_truth(9, 3, 0, 0, 0, 0, true)) return 9;\n\
             if (mixed_narrow(250, -500, UINT32_MAX, 20, 9, 5, -20) != UINT32_MAX + INT64_C(208)\n\
                 || mixed_narrow(120, 300, 60, 20, 200, 5, -100) != 8484) return 10;\n\
             if (match_score(INT64_MIN, -3, false) != -3 || match_score(-9, 4, true) != 91\n\
                 || match_score(-8, 4, true) != 91 || match_score(0, 4, false) != 38\n\
                 || match_score(INT64_MAX, 2, true) != 91 || match_score(10, 4, false) != 27) return 11;\n\
             if (integer_ops(INT16_MIN, 63) != 6552 || integer_ops(7, 0) != -16385) return 12;\n\
             if (proven_arithmetic(3) != -109 || proven_arithmetic(6) != 16\n\
                 || proven_arithmetic(-1) != -121) return 13;\n\
             if (factored_bounds(63, 0) != -120 || factored_bounds(-128, 3) != -4) return 14;\n\
             if (floating_call(2.0, -1, 3.0f, 4.0f) != 5.0\n\
                 || floating_call(2.0, 1, 3.0f, 4.0f) != 12.5) return 15;\n\
             if (procedure_value(1, 8, false) != 5 || procedure_value(10, 4, true) != 45\n\
                 || procedure_value(UINT64_MAX, 4, true) != 1) return 16;\n\
             if (tuple_value(3, 11, true) != 19 || tuple_value(3, 11, false) != 17\n\
                 || tuple_value(INT64_MAX, 0, true) != -5) return 17;\n\
             if (record_value(3, 11, true) != 19 || record_value(3, 11, false) != -1\n\
                 || record_value(INT64_MAX, 0, true) != 0) return 18;\n\
             if (constant_after_branch(true) != INT64_MAX - 2\n\
                 || constant_after_branch(false) != INT64_MAX) return 19;\n\
             int64_t byte = 1;\n\
             uint64_t active = 1;\n\
             struct { float value; uint32_t guard; } single = {1.0f, UINT32_C(0xdeadbeef)};\n\
             double dbl = 2.0;\n\
             uint64_t observed = scalar_inout(&byte, &active, &single.value, &dbl, 0.5f, 2.0, true);\n\
             if (byte != -2 || active != 0 || single.value != 1.5f || single.guard != UINT32_C(0xdeadbeef)\n\
                 || dbl != 4.0 || observed != UINT64_C(0x4020000000000000)) return 20;\n\
             active = 1;\n\
             observed = scalar_inout(&byte, &active, &single.value, &dbl, 0.5f, 2.0, false);\n\
             if (byte != -2 || active != 1 || single.value != 1.5f || dbl != 4.0\n\
                 || observed != UINT64_C(0x3ffffffb)) return 21;\n\
             if (nested_value(5, -3, true) != 10 || nested_value(5, -3, false) != -6\n\
                 || nested_value(INT64_MIN, 0, true) != 0) return 22;\n\
             const uint64_t nan = UINT64_C(0x7ff8000000000042);\n\
             if (!product_equality(0, UINT64_C(0x8000000000000000), 255, 511, 3)\n\
                 || product_equality(nan, nan, 1, 1, 3) || !product_equality(nan, nan, 1, 1, 2)\n\
                 || product_equality(0, 0, 1, 2, 3) || product_equality(0, 0, 1, 1, 1)) return 23;\n\
             uint64_t cells[] = {0, 0, UINT64_C(0x1234567800000000), 0, 9, 0, 123};\n\
             if (result_value(cells, -2, 4, 9, 6, 7, 8, true, -2.5f, 3.0) != 0\n\
                 || cells[0] != 19 || cells[1] != UINT64_MAX - 1\n\
                 || cells[2] != UINT64_C(0x12345678c0200000) || cells[3] != UINT64_C(0x4008000000000000)\n\
                 || cells[4] != 0 || cells[5] != UINT64_MAX - 4 || cells[6] != 123) return 24;\n\
             if (result_value(cells, -2, 4, 9, 6, 7, 8, false, 0.0f, -0.0) != 0\n\
                 || cells[0] != 84 || cells[1] != UINT64_MAX - 1\n\
                 || cells[2] != UINT64_C(0x1234567800000000) || cells[3] != UINT64_C(0x8000000000000000)\n\
                 || cells[4] != 0 || cells[5] != 5 || cells[6] != 123) return 25;\n\
             uint64_t literals[7] = {0};\n\
             if (float_literals(literals, true) != 0 || literals[0] != UINT64_C(0x3f800000)\n\
                 || literals[1] != UINT64_C(0x3f800002) || literals[2] != 1\n\
                 || literals[3] != UINT64_C(0x80000000) || literals[4] != UINT64_C(0x7f7fffff)\n\
                 || literals[5] != UINT64_C(0x7f800000) || literals[6] != UINT64_C(0x3fe00000)) return 26;\n\
             if (float_literals(literals, false) != 0 || literals[6] != UINT64_C(0x40000000)) return 27;\n\
             if (aggregate_calls(127, true, 1.5f) != UINT64_C(0x3fbfff86)\n\
                 || aggregate_calls(127, false, 1.5f) != 390 || aggregate_calls(-2, false, 0.0f) != 3) return 28;\n\
             if (float_remainders(7.5, 2.0, -4.5f, 3.0f) != 2.0\n\
                 || float_remainders(-7.5, 2.0, 4.5f, 3.0f) != 2.0\n\
                 || !isnan(float_remainders(7.5, 0.0, 4.5f, 3.0f))) return 29;\n\
             uint64_t math[14] = {0};\n\
             if (floating_math(math, -2.5, -0.5f) != 0\n\
                 || math[0] != UINT64_C(0xc008000000000000) || math[1] != UINT64_C(0xc000000000000000)\n\
                 || math[2] != math[1] || math[3] != math[0] || math[4] != UINT64_C(0xc004000000000000)\n\
                 || math[5] != 0 || math[6] != UINT64_C(0xb970000000000000)\n\
                 || math[7] != UINT64_C(0xbf800000) || math[8] != UINT64_C(0x80000000)\n\
                 || math[9] != math[8] || math[10] != math[7] || math[11] != UINT64_C(0xbf000000)\n\
                 || math[12] != 0 || math[13] != UINT64_C(0xa8800000)) return 30;\n\
             if (floating_math(math, 0.0, 0.0f) != 0 || math[4] != UINT64_C(0x8000000000000000)\n\
                 || math[5] != 0 || math[11] != UINT64_C(0x80000000) || math[12] != 0) return 31;\n\
             uint64_t conversions[8] = {0};\n\
             if (float_conversions(conversions, -257, UINT64_MAX, 65535.75f, -128.75) != 0\n\
                 || conversions[0] != UINT64_C(0xc3808000) || conversions[1] != UINT64_C(0xc070100000000000)\n\
                 || conversions[2] != UINT64_C(0x5f800000) || conversions[3] != UINT64_C(0x43f0000000000000)\n\
                 || (int64_t)conversions[4] != -128 || conversions[5] != UINT16_MAX\n\
                 || conversions[6] != UINT16_MAX || conversions[7] != 0) return 32;\n\
             if (float_conversions(conversions, 0, 0, NAN, INFINITY) != 0\n\
                 || conversions[4] != INT8_MAX || conversions[5] != 0\n\
                 || conversions[6] != 0 || conversions[7] != UINT64_MAX) return 33;\n\
             uint64_t bits[5] = {0};\n\
             if (integer_bits(bits, -80, -7, 0x8001, 17, -3) != 0\n\
                 || (int64_t)bits[0] != -40 || bits[1] != 257 || bits[2] != 0x3000\n\
                 || bits[3] != UINT64_C(0xc0000000) || bits[4] != 79) return 34;\n\
             uint64_t nested[10] = {0};\n\
             nested[3] = nested[9] = UINT64_MAX;\n\
             if (nested_call_entry(nested, 127, 1.25f, true) != 0\n\
                 || nested[0] != UINT64_C(0x3fa00000) || nested[1] != UINT64_C(0x3ffc000000000000)\n\
                 || nested[2] != 127 || nested[3] != 0 || nested[4] != 1 || nested[5] != 0\n\
                 || nested[6] != 127 || nested[7] != 0 || nested[8] != 127 || nested[9] != UINT64_MAX) return 35;\n\
             nested[3] = UINT64_MAX;\n\
             if (nested_call_entry(nested, -2, 0.25f, false) != 0\n\
                 || nested[0] != UINT64_C(0x3e800000) || nested[1] != UINT64_C(0x3fe8000000000000)\n\
                 || (int64_t)nested[2] != -2 || nested[3] != 0 || nested[4] != 0\n\
                 || nested[5] != 0 || nested[6] != UINT64_MAX || nested[7] != 0 || nested[8] != UINT64_MAX\n\
                 || nested[9] != UINT64_MAX) return 36;\n\
             uint64_t product[11] = {0};\n\
             product[10] = UINT64_MAX;\n\
             if (product_caller(product, 127, 1.25f, true) != 0\n\
                 || product[0] != UINT64_C(0x3fa00000) || product[1] != UINT64_C(0x3ffc000000000000)\n\
                 || product[2] != 127 || product[3] != 0 || product[4] != 1 || product[5] != 129\n\
                 || product[6] != UINT64_C(0x40100000) || product[7] != 256\n\
                 || product[8] != UINT64_C(0x4016000000000000) || product[9] != 100\n\
                 || product[10] != UINT64_MAX) return 37;\n\
             uint64_t row[12] = {UINT64_C(0x3fa00000), UINT64_C(0x3ffc000000000000), 127, 0, 1};\n\
             const uint64_t sample[2] = {129, UINT64_C(0x40100000)};\n\
             row[11] = UINT64_MAX;\n\
             uint64_t *out = &row[1];\n\
             if (product_inputs(out, 10, 20, 30, 40, row, sample, 1.5) != 0\n\
                 || row[0] != UINT64_C(0x3fa00000) || (uint32_t)out[0] != UINT32_C(0x3fa00000)\n\
                 || out[1] != UINT64_C(0x3ffc000000000000) || out[2] != 127 || out[3] != 0\n\
                 || out[4] != 1 || out[5] != 129 || out[6] != UINT64_C(0x40100000)\n\
                 || out[7] != 256 || out[8] != UINT64_C(0x4016000000000000) || out[9] != 100\n\
                 || out[10] != UINT64_MAX) return 38;\n\
             uint64_t compound[5] = {0};\n\
             compound[4] = UINT64_MAX;\n\
             if (compound_values(compound, 7.5, 2.0, 3.5f, true) != 0\n\
                 || compound[0] != UINT64_C(0x3ff8000000000000) || compound[1] != UINT64_C(0x3fa00000)\n\
                 || compound[2] != 2 || compound[3] != UINT64_MAX || compound[4] != UINT64_MAX) return 39;\n\
             if (compound_values(compound, 7.5, 2.0, 3.5f, false) != 0\n\
                 || compound[0] != UINT64_C(0x3ff8000000000000) || compound[1] != UINT64_C(0x40500000)\n\
                 || compound[2] != 2 || compound[3] != UINT64_MAX || compound[4] != UINT64_MAX) return 40;\n\
             if (tuple_match_native(-1, 0, true) != 100 || tuple_match_native(-2, 1, false) != 100\n\
                 || tuple_match_native(0, UINT16_MAX, true) != 65735 || tuple_match_native(1, 9, false) != 209\n\
                 || tuple_match_native(-1, 1, false) != 1 || tuple_match_native(127, 0, true) != 1000) return 41;\n\
             if (enum_pipeline(0, -1, 0.0f, true) != 100\n\
                 || enum_pipeline(0, -128, 1.25f, true) != UINT64_MAX - 127\n\
                 || enum_pipeline(0, -1, 0.0f, false) != 200\n\
                 || enum_pipeline(1, 0, -0.0f, false) != UINT64_C(0x80000009)\n\
                 || enum_pipeline(2, 0, 0.0f, false) != 300) return 42;\n\
             uint64_t forwarded[6] = {0};\n\
             forwarded[5] = UINT64_MAX;\n\
             if (forwarded_results(forwarded, 127, -0.0f) != 0\n\
                 || (int64_t)forwarded[0] != -128 || forwarded[1] != UINT64_C(0x3f800000)\n\
                 || forwarded[2] != 127 || forwarded[3] != UINT64_C(0x80000000)\n\
                 || forwarded[4] != 1 || forwarded[5] != UINT64_MAX) return 43;\n\
             if (generic_enum_pipeline(true, 1.0f, -128) != UINT64_MAX - 127\n\
                 || generic_enum_pipeline(true, 1.0f, -1) != UINT64_MAX\n\
                 || generic_enum_pipeline(true, -0.0f, 0) != UINT64_C(0x80000000)\n\
                 || generic_enum_pipeline(false, -0.0f, -128) != UINT64_C(0x80000000)) return 44;\n\
             uint64_t joined[5] = {0};\n\
             joined[4] = UINT64_MAX;\n\
             if (aggregate_join(joined, true, -32768, 32767, -0.0f) != 0\n\
                 || joined[0] != 32767 || (int64_t)joined[2] != -32768\n\
                 || joined[1] != UINT64_C(0x80000000) || joined[3] != 17 || joined[4] != UINT64_MAX) return 45;\n\
             if (aggregate_join(joined, false, -32768, 32767, 1.0f) != 0\n\
                 || (int64_t)joined[0] != -32768 || joined[2] != 32767\n\
                 || joined[1] != UINT64_C(0x3f800000) || joined[3] != 17 || joined[4] != UINT64_MAX) return 46;\n\
             if (aggregate_join(joined, false, -32768, 32767, -0.0f) != 0\n\
                 || (int64_t)joined[0] != -32768 || joined[2] != 0\n\
                 || joined[1] != UINT64_C(0x80000000) || joined[3] != 9 || joined[4] != UINT64_MAX) return 47;\n\
             uint64_t seen = UINT64_MAX;\n\
             uint64_t routes[4];\n\
             if (enum_routes(routes, &seen, true, -1) != 0 || seen != 0\n\
                 || routes[0] != UINT64_MAX || routes[1] != UINT64_MAX\n\
                 || routes[2] != 23 || routes[3] != UINT64_MAX) return 48;\n\
             if (enum_routes(routes, &seen, false, -1) != 0 || seen != 1\n\
                 || routes[0] != UINT64_MAX || routes[1] != 17 || routes[2] != 29 || routes[3] != 39) return 49;\n\
             return enum_routes(routes, &seen, true, -128) != 0 || seen != 2\n\
                 || routes[0] != UINT64_MAX - 127 || routes[1] != UINT64_MAX - 127\n\
                 || routes[2] != UINT64_MAX - 127 || routes[3] != UINT64_MAX - 127;\n\
         }\n",
    )
    .unwrap();
    let link = std::process::Command::new("cc")
        .args([
            &caller,
            &leaf,
            &branch,
            &calls,
            &stack,
            &signed,
            &mutable,
            &loop_object,
            &range_object,
            &bool_object,
            &narrow_object,
            &match_object,
            &integer_object,
            &arithmetic_object,
            &bounds_object,
            &float_object,
            &procedure_object,
            &tuple_object,
            &record_object,
            &constant_object,
            &inout_object,
            &nested_object,
            &equality_object,
            &result_object,
            &literals_object,
            &aggregate_call_object,
            &remainder_object,
            &math_object,
            &conversions_object,
            &bits_object,
            &nested_call_object,
            &product_inputs_object,
            &product_caller_object,
            &compound_object,
            &tuple_match_object,
            &enum_object,
            &forwarded_object,
            &generic_enum_object,
            &aggregate_join_object,
            &object,
        ])
        .arg("-lm")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("link native object");
    assert!(
        link.status.success(),
        "{}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert!(
        std::process::Command::new(binary)
            .status()
            .unwrap()
            .success()
    );
}
