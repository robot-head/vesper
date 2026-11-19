//! One real compiler function family through the shim (#1126): the checked
//! integer arithmetic of `src/native.rs` (`checked_arithmetic`, `binary`,
//! `check_overflow`), emitted from Vesper (tests/fixtures/checked.ves) and
//! from the Rust lowering below, must give byte-identical objects for the
//! host and for every CI target. Also measures the per-instruction cost of
//! emitting through the shim.
mod support;

use std::time::{Duration, Instant};
use support::*;
use vesper_cranelift_sys::cranelift_codegen::ir::{
    self, AbiParam, InstBuilder, Signature, condcodes::IntCC, types,
};
use vesper_cranelift_sys::cranelift_codegen::{settings, settings::Configurable};
use vesper_cranelift_sys::cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use vesper_cranelift_sys::cranelift_module::{Linkage, Module};
use vesper_cranelift_sys::cranelift_object::{ObjectBuilder, ObjectModule};

#[derive(Clone, Copy, PartialEq)]
enum K {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}
impl K {
    const ALL: [K; 8] = [K::I8, K::I16, K::I32, K::I64, K::U8, K::U16, K::U32, K::U64];
    fn unsigned(self) -> bool {
        matches!(self, K::U8 | K::U16 | K::U32 | K::U64)
    }
    fn bits(self) -> u32 {
        match self {
            K::I8 | K::U8 => 8,
            K::I16 | K::U16 => 16,
            K::I32 | K::U32 => 32,
            K::I64 | K::U64 => 64,
        }
    }
    fn min(self) -> i64 {
        match self.bits() {
            8 => i8::MIN.into(),
            16 => i16::MIN.into(),
            32 => i32::MIN.into(),
            _ => i64::MIN,
        }
    }
    fn mask(self) -> u64 {
        match self.bits() {
            64 => u64::MAX,
            b => (1u64 << b) - 1,
        }
    }
    fn name(self) -> String {
        format!("{}{}", if self.unsigned() { "u" } else { "i" }, self.bits())
    }
}

/// The Rust lowering: src/native.rs `checked_arithmetic` and `binary` for
/// `+ - * /`, with `check_overflow`'s raise replaced by a trap (as in the
/// Vesper fixture).
struct Lower<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
}
impl Lower<'_, '_> {
    fn check_overflow(&mut self, overflow: ir::Value) {
        let good = self.b.create_block();
        let bad = self.b.create_block();
        self.b.ins().brif(overflow, bad, &[], good, &[]);
        self.b.switch_to_block(bad);
        self.b.ins().trap(ir::TrapCode::INTEGER_OVERFLOW);
        self.b.switch_to_block(good);
    }
    fn checked_arithmetic(&mut self, op: &str, kind: K, a: ir::Value, b: ir::Value) -> ir::Value {
        let overflow = match (op, kind) {
            ("+" | "-" | "*", K::I8 | K::I16 | K::I32) => {
                let r = match op {
                    "+" => self.b.ins().iadd(a, b),
                    "-" => self.b.ins().isub(a, b),
                    _ => self.b.ins().imul(a, b),
                };
                let up = i64::from(64 - kind.bits());
                let high = self.b.ins().ishl_imm(r, up);
                let canonical = self.b.ins().sshr_imm(high, up);
                Some(self.b.ins().icmp(IntCC::NotEqual, canonical, r))
            }
            ("/", K::I8 | K::I16 | K::I32) => {
                let min = self.b.ins().icmp_imm(IntCC::Equal, a, kind.min());
                let minus_one = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                Some(self.b.ins().band(min, minus_one))
            }
            ("+" | "*", K::U8 | K::U16 | K::U32) => {
                let r = if op == "+" {
                    self.b.ins().iadd(a, b)
                } else {
                    self.b.ins().imul(a, b)
                };
                Some(
                    self.b
                        .ins()
                        .icmp_imm(IntCC::UnsignedGreaterThan, r, kind.mask() as i64),
                )
            }
            ("-", K::U8 | K::U16 | K::U32 | K::U64) => {
                Some(self.b.ins().icmp(IntCC::UnsignedLessThan, a, b))
            }
            ("+", K::U64) => {
                let r = self.b.ins().iadd(a, b);
                Some(self.b.ins().icmp(IntCC::UnsignedLessThan, r, a))
            }
            ("*", K::U64) => {
                let high = self.b.ins().umulhi(a, b);
                Some(self.b.ins().icmp_imm(IntCC::NotEqual, high, 0))
            }
            ("+" | "-", K::I64) => {
                let r = if op == "+" {
                    self.b.ins().iadd(a, b)
                } else {
                    self.b.ins().isub(a, b)
                };
                let x = self.b.ins().bxor(a, r);
                let y = if op == "+" {
                    self.b.ins().bxor(b, r)
                } else {
                    self.b.ins().bxor(a, b)
                };
                let both = self.b.ins().band(x, y);
                Some(self.b.ins().icmp_imm(IntCC::SignedLessThan, both, 0))
            }
            ("*", K::I64) => {
                let r = self.b.ins().imul(a, b);
                let high = self.b.ins().smulhi(a, b);
                let sign = self.b.ins().sshr_imm(r, 63);
                Some(self.b.ins().icmp(IntCC::NotEqual, high, sign))
            }
            ("/", K::I64) => {
                let min = self.b.ins().icmp_imm(IntCC::Equal, a, i64::MIN);
                let minus_one = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                Some(self.b.ins().band(min, minus_one))
            }
            _ => None,
        };
        if let Some(overflow) = overflow {
            self.check_overflow(overflow);
        }
        self.binary(op, a, b, kind.unsigned())
    }
    fn binary(&mut self, op: &str, a: ir::Value, b: ir::Value, unsigned: bool) -> ir::Value {
        match op {
            "+" => self.b.ins().iadd(a, b),
            "-" => self.b.ins().isub(a, b),
            "*" => self.b.ins().imul(a, b),
            _ if unsigned => self.b.ins().udiv(a, b),
            _ => {
                let min = self.b.ins().icmp_imm(IntCC::Equal, a, i64::MIN);
                let minus_one = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                let overflow = self.b.ins().band(min, minus_one);
                let wrap = self.b.create_block();
                let divide = self.b.create_block();
                let join = self.b.create_block();
                self.b.append_block_param(join, types::I64);
                self.b.ins().brif(overflow, wrap, &[], divide, &[]);
                self.b.switch_to_block(wrap);
                self.b.ins().jump(join, &[a.into()]);
                self.b.switch_to_block(divide);
                let result = self.b.ins().sdiv(a, b);
                self.b.ins().jump(join, &[result.into()]);
                self.b.switch_to_block(join);
                self.b.block_params(join)[0]
            }
        }
    }
}

fn module(triple: &str, opt: &str) -> ObjectModule {
    let mut flags = settings::builder();
    if !opt.is_empty() {
        flags.set("opt_level", opt).unwrap();
    }
    let isa = if triple.is_empty() {
        vesper_cranelift_sys::cranelift_native::builder().unwrap()
    } else {
        vesper_cranelift_sys::cranelift_codegen::isa::lookup_by_name(triple).unwrap()
    }
    .finish(settings::Flags::new(flags))
    .unwrap();
    let libcalls = vesper_cranelift_sys::cranelift_module::default_libcall_names();
    ObjectModule::new(ObjectBuilder::new(isa, "checked", libcalls).unwrap())
}

fn rust_family(triple: &str, opt: &str) -> Vec<u8> {
    let mut m = module(triple, opt);
    let mut sig = Signature::new(m.isa().default_call_conv());
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    let mut ctx = m.make_context();
    let mut fctx = FunctionBuilderContext::new();
    for kind in K::ALL {
        for (op, name) in [("+", "add"), ("-", "sub"), ("*", "mul"), ("/", "div")] {
            let id = m
                .declare_function(
                    &format!("checked_{name}_{}", kind.name()),
                    Linkage::Export,
                    &sig,
                )
                .unwrap();
            ctx.func.signature = sig.clone();
            {
                let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                let entry = b.create_block();
                b.append_block_params_for_function_params(entry);
                b.switch_to_block(entry);
                let (x, y) = (b.block_params(entry)[0], b.block_params(entry)[1]);
                let r = Lower { b: &mut b }.checked_arithmetic(op, kind, x, y);
                b.ins().return_(&[r]);
                b.seal_all_blocks();
                b.finalize();
            }
            m.define_function(id, &mut ctx).unwrap();
            m.clear_context(&mut ctx);
        }
    }
    m.finish().emit().unwrap()
}

fn vesper_family(entry: &str) -> Vec<u8> {
    let target = host_target().unwrap();
    let checked = checked_with(&fixture("checked.ves"), target);
    let id = jit(&checked, entry);
    assert_ne!(id, 0, "{entry}: the Vesper lowering failed");
    vesper_cranelift_sys::take_published(id).unwrap()
}

#[test]
fn vesper_and_rust_lowerings_emit_identical_objects() {
    if host_target().is_none() {
        eprintln!("no .vsys target for this host; comparison unavailable");
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = vesper_cranelift_sys::live_handles();
    for (entry, triple, opt) in [
        ("checked_none", "", ""),
        ("checked_speed", "", "speed"),
        ("checked_x64_linux", "x86_64-unknown-linux-gnu", "speed"),
        (
            "checked_aarch64_linux",
            "aarch64-unknown-linux-gnu",
            "speed",
        ),
        ("checked_aarch64_macos", "aarch64-apple-darwin", "speed"),
        ("checked_x64_windows", "x86_64-pc-windows-msvc", "speed"),
    ] {
        let vesper = vesper_family(entry);
        let rust = rust_family(triple, opt);
        assert!(rust.len() > 1000, "{entry}: {} bytes", rust.len());
        assert!(
            vesper == rust,
            "{entry}: the objects differ ({} vs {} bytes)",
            vesper.len(),
            rust.len()
        );
    }
    assert_eq!(vesper_cranelift_sys::live_handles(), before);
}

/// Emission of `n` chained iadds directly through `FunctionBuilder`.
fn rust_direct(n: u64) -> u32 {
    let mut sig = Signature::new(vesper_cranelift_sys::cranelift_codegen::isa::CallConv::SystemV);
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    let mut func = ir::Function::with_name_signature(ir::UserFuncName::default(), sig);
    let mut fctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fctx);
    let entry = b.create_block();
    let x = b.append_block_param(entry, types::I64);
    b.switch_to_block(entry);
    let mut v = x;
    for _ in 0..n {
        v = b.ins().iadd(v, x);
    }
    v.as_u32()
}

/// The same through the C ABI (`vcl_binary`), called from Rust.
fn rust_through_shim(n: u64) -> u32 {
    use vesper_cranelift_sys::*;
    // SAFETY: every handle comes from the call before it, and null slices
    // have length 0.
    unsafe {
        let m = vcl_module_new(
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
        );
        let sig = vcl_sig_new(m, 0);
        vcl_sig_param(m, sig, 4);
        vcl_sig_return(m, sig, 4);
        let b = vcl_builder_new(m, sig);
        let entry = vcl_create_block(b);
        let x = vcl_append_block_param(b, entry, 4);
        vcl_switch_to_block(b, entry);
        let mut v = x;
        for _ in 0..n {
            v = vcl_binary(b, 1, v, x);
        }
        vcl_builder_free(b);
        vcl_module_free(m);
        v
    }
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed())
}

/// The emission paths: Rust calling `FunctionBuilder` directly, Rust
/// calling the C ABI, and Vesper (JIT) calling the raw import, the typed
/// wrappers, or `vcl_emit` with 512 instructions per call.
const PATHS: [&str; 5] = ["direct", "shim", "raw", "wrapped", "batched"];

/// A prepared run of `n` chained iadds along `path`.
fn prepare(path: &str, n: u64) -> Box<dyn Fn() -> u64> {
    match path {
        "direct" => Box::new(move || u64::from(rust_direct(n))),
        "shim" => Box::new(move || u64::from(rust_through_shim(n))),
        _ => {
            let entry = format!(
                "pub fn bench_entry() -> u64 eff [Ffi[Cranelift]] {{\n    emit_{path}({n})\n}}\n"
            );
            let source = format!("{}\n{entry}", fixture("checked.ves"));
            let checked = checked_with(&source, host_target().unwrap());
            Box::new(move || jit(&checked, "bench_entry"))
        }
    }
}

/// Nanoseconds per emitted instruction along each path: a run of N minus a
/// run of 1, divided by N - 1, which removes the JIT's compile time. Set
/// VESPER_CRANELIFT_BENCH=1 for the report's runs (1,000,000 instead of
/// 20,000, fastest of 5). For instruction counts, run one path under callgrind with
/// VESPER_CRANELIFT_BENCH_PATH=<path> VESPER_CRANELIFT_BENCH_N=<n> at two
/// values of n and divide the difference.
#[test]
fn emission_overhead_through_the_shim() {
    if host_target().is_none() {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = std::env::var_os("VESPER_CRANELIFT_BENCH_PATH") {
        let path = path.to_str().unwrap();
        let n: u64 =
            std::env::var("VESPER_CRANELIFT_BENCH_N").map_or(20_000, |v| v.parse().unwrap());
        assert_eq!(prepare(path, n)(), n, "{path}");
        return;
    }
    let big = std::env::var_os("VESPER_CRANELIFT_BENCH").is_some();
    let n: u64 = if big { 1_000_000 } else { 20_000 };
    let reps = if big { 5 } else { 1 };
    let mut report = String::from("ns per iadd:");
    for path in PATHS {
        let (run_n, run_1) = (prepare(path, n), prepare(path, 1));
        // The value is the last value number: n iadds after the parameter.
        // The fastest of `reps` runs of each, to keep JIT compile noise out.
        let fastest = |run: &dyn Fn() -> u64, want: u64| {
            (0..reps)
                .map(|_| {
                    let (v, t) = time(run);
                    assert_eq!(v, want, "{path}");
                    t
                })
                .min()
                .unwrap()
        };
        let t_n = fastest(&*run_n, n);
        let t_1 = fastest(&*run_1, 1);
        let ns = t_n.saturating_sub(t_1).as_secs_f64() * 1e9 / (n - 1) as f64;
        report.push_str(&format!(" {path} {ns:.1}"));
    }
    eprintln!("{report}");
    if big {
        println!("{report}");
    }
}
