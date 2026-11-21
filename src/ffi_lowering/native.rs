//! Native lowering of foreign operations (design §13.3-13.4). A child module
//! of `crate::native`, entered from `Codegen::perform` for effects of
//! imported `.vsys` libraries. No `unsafe` Rust: the only machine code that
//! is not Cranelift output is the two generated `RDPKRU`/`WRPKRU` leaf
//! functions of the `isolated(mpk)` trampoline.
use super::*;
use crate::ffi_lowering::{self as ffi, Handler};
use cranelift_module::DataDescription;
use vesper_ffi::sys::{AbiType, Access, ParameterType, Pointee, PointerAccess, Scalar, ValueType};
use vesper_ffi::{Endian, FieldKind, WireLayout};

/// `xor ecx, ecx; rdpkru; ret` — returns PKRU in eax (System V).
const RDPKRU: [u8; 6] = [0x31, 0xc9, 0x0f, 0x01, 0xee, 0xc3];
/// `mov eax, edi; xor ecx, ecx; xor edx, edx; wrpkru; ret`.
const WRPKRU: [u8; 10] = [0x89, 0xf8, 0x31, 0xc9, 0x31, 0xd2, 0x0f, 0x01, 0xef, 0xc3];
const RDPKRU_SYMBOL: &str = "vesper_bridge_rdpkru";
const WRPKRU_SYMBOL: &str = "vesper_bridge_wrpkru";

/// The `Ok` payload type of a `Result`.
fn result_ok(ty: &Type) -> Result<Type, String> {
    match ty {
        Type::Named(n, args) if n == "Result" && args.len() == 2 => Ok(args[0].clone()),
        other => Err(format!("a CStr call's result {other} is not a Result")),
    }
}

/// Native source memory or a by-value scalar.
#[derive(Clone, Copy)]
enum Source {
    Memory(ir::Value),
    Scalar(ir::Value),
}
/// Bytes to copy back into source memory after the call.
struct CopyBack {
    guard: Option<ir::Value>,
    buffer: ir::Value,
    target: ir::Value,
    shape: Shape,
}
#[derive(Clone)]
enum Shape {
    Scalar(Scalar),
    Struct(WireLayout, Layout),
    Slice(Pointee, Layout, usize),
}

/// A marshalled C buffer and how to copy it back, when the callee writes it.
type Marshalled = Option<(ir::Value, Shape)>;

fn c_type(bits: u8) -> ir::Type {
    match bits {
        8 => types::I8,
        16 => types::I16,
        32 => types::I32,
        _ => types::I64,
    }
}
fn abi_param(t: AbiType, pointer: ir::Type) -> AbiParam {
    match t {
        AbiType::Pointer => AbiParam::new(pointer),
        AbiType::Integer(s) => {
            let p = AbiParam::new(c_type(s.bits()));
            if s.bits() == 64 {
                p
            } else if s.signed() {
                p.sext()
            } else {
                p.uext()
            }
        }
    }
}
/// Structs whose source layout already is their C layout: only 64-bit
/// native-endian integers, in order, without padding or nesting.
fn identical_struct(w: &WireLayout, little: bool) -> bool {
    w.fields().iter().enumerate().all(|(i, f)| match f.kind() {
        FieldKind::Integer(int) => {
            int.bits == 64
                && f.offset() == 8 * i
                && match int.endian {
                    Endian::Native => true,
                    Endian::Little => little,
                    Endian::Big => !little,
                }
        }
        _ => false,
    })
}
fn identical(p: &Pointee, little: bool) -> bool {
    match p {
        Pointee::Scalar(s) => s.bits() == 64,
        Pointee::Struct(w) => identical_struct(w.layout(), little),
    }
}

/// What a foreign call calls: an imported symbol, or (a callback type's
/// call) the C function pointer it was handed.
enum Callee {
    Direct(ir::FuncRef),
    Indirect(ir::SigRef, ir::Value),
}

impl<M: Module> Codegen<'_, '_, M> {
    pub(super) fn foreign_perform(
        &mut self,
        effect: &str,
        op: &str,
        args: &[Arg],
    ) -> Result<ir::Value, String> {
        let plan = ffi::op_plan(effect, op)?;
        let signature = self
            .world
            .effects
            .get(effect)
            .and_then(|e| e.ops.iter().find(|f| f.name == op))
            .ok_or("unknown foreign operation")?
            .clone();
        let descriptors = plan.function.parameters();
        if args.len() != signature.params.len() || descriptors.len() != signature.params.len() {
            return Err(format!(
                "foreign call {}::{} has the wrong number of arguments",
                plan.library,
                plan.function.symbol()
            ));
        }
        // SH4 swap point (#1287): the `ffi` hook may emit this call
        // (`codegen_swap::ffi`); none is installed by default. A callback
        // type's call is not offered: the steps name a symbol.
        if !plan.function.is_indirect()
            && let Some(v) = self.foreign_perform_swapped(&plan, &signature, args)?
        {
            return Ok(v);
        }
        let pointer = self.m.target_config().pointer_type();
        let mut c_args = vec![];
        let mut copies = vec![];
        let mut restores = vec![];
        // Per `CStr` argument: its malloc'd buffer, and the offset of its
        // first NUL byte (or -1).
        let mut strings: Vec<(ir::Value, ir::Value)> = vec![];
        for ((arg, param), descriptor) in args.iter().zip(&signature.params).zip(descriptors) {
            if let ParameterType::CStr = descriptor.ty() {
                let (data, len) = self.foreign_view(&arg.expr, 1)?;
                strings.push(self.foreign_cstr(data, len)?);
                c_args.push(strings.last().expect("just pushed").0);
                continue;
            }
            if let ParameterType::Slice { element, .. } = descriptor.ty()
                && crate::seq::is_slice_type(&param.ty)
            {
                // A source `Slice[T]` whose packed stride is T's C size: lent
                // to C in place as (data + start * stride, len), no copy.
                let (lent, len) = self.foreign_view(&arg.expr, element.size())?;
                c_args.push(lent);
                c_args.push(len);
                continue;
            }
            let value = self.expr(&arg.expr)?;
            let aggregate = Layout::of(self.world, &param.ty)?.aggregate();
            let source = if param.pass == Pass::Inout {
                let place = self.address(&arg.expr)?;
                if !aggregate && let ExprKind::Name(n) = &arg.expr.kind {
                    let var = *self.vars.get(n).ok_or("unknown mutable foreign argument")?;
                    restores.push((var, place, param.ty.clone()));
                }
                Source::Memory(place)
            } else if aggregate {
                Source::Memory(value)
            } else {
                Source::Scalar(value)
            };
            match descriptor.ty() {
                ParameterType::Value(v) => c_args.push(self.foreign_value(v, source)?),
                // The handle struct's one field is the C handle word, for
                // every mode: C never replaces the handle itself.
                ParameterType::Handle { .. } => c_args.push(self.foreign_word(source)),
                ParameterType::CStr => unreachable!("C strings are lowered above"),
                ParameterType::Pointer {
                    pointee,
                    access,
                    nullable,
                } => {
                    let (payload, guard, payload_ty) =
                        self.foreign_option(source, &param.ty, *nullable)?;
                    let writes = *access != PointerAccess::In;
                    let (ptr, copy) =
                        self.foreign_pointee(pointee, payload, &payload_ty, writes)?;
                    if let Some((buffer, shape)) = copy {
                        let Source::Memory(target) = payload else {
                            return Err("writable foreign pointer lacks a place".into());
                        };
                        copies.push(CopyBack {
                            guard,
                            buffer,
                            target,
                            shape,
                        });
                    }
                    c_args.push(self.foreign_guard(guard, ptr, pointer));
                }
                ParameterType::Slice {
                    element,
                    access,
                    nullable,
                } => {
                    let (payload, guard, payload_ty) =
                        self.foreign_option(source, &param.ty, *nullable)?;
                    let writes = *access == Access::Inout;
                    let (ptr, len, copy) =
                        self.foreign_slice(element, payload, &payload_ty, writes)?;
                    if let Some((buffer, shape)) = copy {
                        let Source::Memory(target) = payload else {
                            return Err("writable foreign slice lacks a place".into());
                        };
                        copies.push(CopyBack {
                            guard,
                            buffer,
                            target,
                            shape,
                        });
                    }
                    let ptr = self.foreign_guard(guard, ptr, pointer);
                    c_args.push(ptr);
                    let len = self.b.ins().iconst(types::I64, len as i64);
                    c_args.push(self.foreign_guard(guard, len, types::I64));
                }
            }
        }
        let abi = plan.function.abi();
        // Apple arm64 passes variadic arguments on the stack: zero words
        // fill the remaining argument registers first (`sys::Variadic`).
        if let Some(v) = abi.variadic {
            for _ in 0..v.padding {
                let zero = self.b.ins().iconst(types::I64, 0);
                c_args.insert(v.first - v.padding, zero);
            }
        }
        let mut sig = self.m.make_signature();
        for t in &abi.parameters {
            sig.params.push(abi_param(*t, pointer));
        }
        if let Some(t) = abi.result {
            sig.returns.push(abi_param(t, pointer));
        }
        // A callback type's call goes through the pointer in its first
        // argument (the handle word); every other call imports its symbol.
        let callee = if plan.function.is_indirect() {
            let pointer = c_args.remove(0);
            Callee::Indirect(self.b.import_signature(sig), pointer)
        } else {
            let symbol = plan.function.link_symbol();
            self.foreign_symbol_free(symbol, Linkage::Import)?;
            let callee = self
                .m
                .declare_function(symbol, Linkage::Import, &sig)
                .map_err(|e| format!("foreign symbol '{symbol}': {e}"))?;
            Callee::Direct(self.m.declare_func_in_func(callee, self.b.func))
        };
        // A `CStr` argument holding a NUL byte refuses the whole call:
        // `Err(ForeignInvalid { raw: offset })` of the first such argument.
        let refusal = if strings.is_empty() {
            None
        } else {
            let layout = Layout::of(self.world, &signature.result)?;
            let slot = self.alloc(layout.size);
            let mut offset = self.b.ins().iconst(types::I64, -1);
            for (_, at) in strings.iter().rev() {
                let found = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::SignedGreaterThanOrEqual, *at, 0);
                offset = self.b.ins().select(found, *at, offset);
            }
            let refused = self
                .b
                .ins()
                .icmp_imm(IntCC::SignedGreaterThanOrEqual, offset, 0);
            let call_block = self.b.create_block();
            let refuse_block = self.b.create_block();
            let join = self.b.create_block();
            self.branch(refused, refuse_block, &[], call_block, &[]);
            self.b.switch_to_block(refuse_block);
            self.b.seal_block(refuse_block);
            let (err, err_fields) = layout.variant("Err").ok_or("Result lacks Err")?;
            let err_offset = err_fields.first().ok_or("Err lacks a payload")?.offset;
            let tag = self.b.ins().iconst(types::I64, err as i64);
            self.b.ins().store(ir::MemFlags::new(), tag, slot, 0);
            self.b
                .ins()
                .store(ir::MemFlags::new(), offset, slot, err_offset);
            self.jump(join, &[]);
            self.b.switch_to_block(call_block);
            self.b.seal_block(call_block);
            Some((layout, slot, join))
        };
        let saved = if plan.handler == Handler::IsolatedMpk {
            Some(self.foreign_isolate(&plan.library)?)
        } else {
            None
        };
        let call = match callee {
            Callee::Direct(f) => self.b.ins().call(f, &c_args),
            Callee::Indirect(sig, pointer) => self.b.ins().call_indirect(sig, pointer, &c_args),
        };
        let raw = self.b.inst_results(call).first().copied();
        if let Some(saved) = saved {
            let wrpkru = self.foreign_helper(WRPKRU_SYMBOL, &[types::I32], None, &WRPKRU)?;
            self.b.ins().call(wrpkru, &[saved]);
        }
        let os_error = if plan.function.reads_errno() {
            Some(self.foreign_os_error()?)
        } else {
            None
        };
        for copy in copies {
            self.foreign_copy_back(copy)?;
        }
        for (var, ptr, ty) in restores {
            let value = self.load(ptr, &ty)?;
            self.def_var(var, value);
            if let Some(destination) = self.pointer(var) {
                self.b
                    .ins()
                    .store(ir::MemFlags::new(), value, destination, 0);
            }
        }
        // The source result, from the inside out: the `ForeignVal` mapping,
        // the `(value, os_error)` pair, then `Ok` of a call that was made.
        let mut ty = signature.result.clone();
        if refusal.is_some() {
            ty = result_ok(&ty)?;
        }
        let value_ty = match (&ty, os_error) {
            (Type::Tuple(ts), Some(_)) if ts.len() == 2 => ts[0].clone(),
            (_, Some(_)) => return Err("errno result is not a pair".into()),
            (t, None) => t.clone(),
        };
        let mut value = self.foreign_result(plan.function.result(), raw, &value_ty)?;
        if let Some(code) = os_error {
            let layout = Layout::of(self.world, &ty)?;
            let LayoutKind::Product(fields) = &layout.kind else {
                return Err("errno result is not a pair".into());
            };
            let (first, second) = (fields[0].offset, fields[1].offset);
            let pair = self.alloc(layout.size);
            let at = self.offset(pair, first);
            self.store(at, value, &value_ty)?;
            let at = self.offset(pair, second);
            self.store(at, code, &Type::named("u32"))?;
            value = pair;
        }
        if let Some((layout, slot, join)) = refusal {
            let (ok, ok_fields) = layout.variant("Ok").ok_or("Result lacks Ok")?;
            let ok_offset = ok_fields.first().ok_or("Ok lacks a payload")?.offset;
            let tag = self.b.ins().iconst(types::I64, ok as i64);
            self.b.ins().store(ir::MemFlags::new(), tag, slot, 0);
            let at = self.offset(slot, ok_offset);
            self.store(at, value, &ty)?;
            self.jump(join, &[]);
            self.b.switch_to_block(join);
            self.b.seal_block(join);
            value = slot;
        }
        for (buffer, _) in strings {
            let free = self.foreign_libc("free", 1, false)?;
            self.b.ins().call(free, &[buffer]);
        }
        Ok(value)
    }
    /// The `(data + start * stride, len)` of a source `Slice` argument. A
    /// never-grown sequence has no buffer (data 0); C still gets a valid,
    /// aligned pointer.
    fn foreign_view(
        &mut self,
        expr: &Expr,
        stride: usize,
    ) -> Result<(ir::Value, ir::Value), String> {
        let view = self.expr(expr)?;
        let handle = self.b.ins().load(types::I64, ir::MemFlags::new(), view, 0);
        let start = self.b.ins().load(types::I64, ir::MemFlags::new(), view, 8);
        let len = self.b.ins().load(types::I64, ir::MemFlags::new(), view, 16);
        let data = self
            .b
            .ins()
            .load(types::I64, ir::MemFlags::new(), handle, 16);
        let offset = self.b.ins().imul_imm(start, stride as i64);
        let lent = self.b.ins().iadd(data, offset);
        let dummy = self.alloc(8);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, data, 0);
        Ok((self.b.ins().select(empty, dummy, lent), len))
    }
    /// A NUL-terminated `malloc` copy of `len` bytes at `data`, and the
    /// offset of the first NUL byte among them (-1 when there is none).
    fn foreign_cstr(
        &mut self,
        data: ir::Value,
        len: ir::Value,
    ) -> Result<(ir::Value, ir::Value), String> {
        let malloc = self.foreign_libc("malloc", 1, true)?;
        let memcpy = self.foreign_libc("memcpy", 3, true)?;
        let memchr = self.foreign_libc("memchr", 3, true)?;
        let size = self.b.ins().iadd_imm(len, 1);
        let call = self.b.ins().call(malloc, &[size]);
        let buffer = self.b.inst_results(call)[0];
        self.b
            .ins()
            .trapz(buffer, ir::TrapCode::unwrap_user(ffi::CSTR_ALLOC_TRAP));
        self.b.ins().call(memcpy, &[buffer, data, len]);
        let end = self.b.ins().iadd(buffer, len);
        let nul = self.b.ins().iconst(types::I8, 0);
        self.b.ins().store(ir::MemFlags::new(), nul, end, 0);
        let zero = self.b.ins().iconst(types::I64, 0);
        let call = self.b.ins().call(memchr, &[data, zero, len]);
        let found = self.b.inst_results(call)[0];
        let missing = self.b.ins().icmp_imm(IntCC::Equal, found, 0);
        let at = self.b.ins().isub(found, data);
        let none = self.b.ins().iconst(types::I64, -1);
        Ok((buffer, self.b.ins().select(missing, none, at)))
    }
    /// A C library function of 64-bit word parameters (and result), as the
    /// sequence runtime declares it.
    fn foreign_libc(
        &mut self,
        name: &str,
        params: usize,
        returns: bool,
    ) -> Result<ir::FuncRef, String> {
        let mut sig = self.m.make_signature();
        for _ in 0..params {
            sig.params.push(AbiParam::new(types::I64));
        }
        if returns {
            sig.returns.push(AbiParam::new(types::I64));
        }
        let id = self
            .m
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| format!("C library function '{name}': {e}"))?;
        Ok(self.m.declare_func_in_func(id, self.b.func))
    }
    /// The calling thread's OS error code as a 64-bit word: `errno` on
    /// Unix targets, `GetLastError()` on Windows.
    fn foreign_os_error(&mut self) -> Result<ir::Value, String> {
        let (symbol, location) = ffi::os_error_symbol(self.m.isa().triple())?;
        let mut sig = self.m.make_signature();
        sig.returns.push(AbiParam::new(if location {
            types::I64
        } else {
            types::I32
        }));
        let id = self
            .m
            .declare_function(symbol, Linkage::Import, &sig)
            .map_err(|e| format!("OS error symbol '{symbol}': {e}"))?;
        let callee = self.m.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(callee, &[]);
        let result = self.b.inst_results(call)[0];
        let code = if location {
            self.b
                .ins()
                .load(types::I32, ir::MemFlags::new(), result, 0)
        } else {
            result
        };
        Ok(self.b.ins().uextend(types::I64, code))
    }
    fn foreign_guard(
        &mut self,
        guard: Option<ir::Value>,
        value: ir::Value,
        ty: ir::Type,
    ) -> ir::Value {
        match guard {
            Some(is_some) => {
                let zero = self.b.ins().iconst(ty, 0);
                self.b.ins().select(is_some, value, zero)
            }
            None => value,
        }
    }
    fn foreign_narrow(&mut self, v: ir::Value, bits: u8) -> ir::Value {
        if bits == 64 {
            v
        } else {
            self.b.ins().ireduce(c_type(bits), v)
        }
    }
    fn foreign_widen(&mut self, v: ir::Value, bits: u8, signed: bool) -> ir::Value {
        if bits == 64 {
            v
        } else if signed {
            self.b.ins().sextend(types::I64, v)
        } else {
            self.b.ins().uextend(types::I64, v)
        }
    }
    fn foreign_word(&mut self, source: Source) -> ir::Value {
        match source {
            Source::Scalar(v) => v,
            Source::Memory(p) => self.b.ins().load(types::I64, ir::MemFlags::new(), p, 0),
        }
    }
    fn foreign_value(&mut self, v: &ValueType, source: Source) -> Result<ir::Value, String> {
        match v {
            ValueType::Scalar(s) => {
                let word = self.foreign_word(source);
                Ok(self.foreign_narrow(word, s.bits()))
            }
            ValueType::Bool => {
                let word = self.foreign_word(source);
                Ok(self.b.ins().ireduce(types::I8, word))
            }
            ValueType::Enum(e) => {
                let Source::Memory(p) = source else {
                    return Err("foreign enum argument is not in memory".into());
                };
                let tag = self.b.ins().load(types::I64, ir::MemFlags::new(), p, 0);
                let variants = e.variants();
                let first = variants.first().map_or(0, |(_, v)| *v);
                let mut value = self.b.ins().iconst(types::I64, first as i64);
                for (i, (_, raw)) in variants.iter().enumerate().skip(1) {
                    let hit = self.b.ins().icmp_imm(IntCC::Equal, tag, i as i64);
                    let candidate = self.b.ins().iconst(types::I64, *raw as i64);
                    value = self.b.ins().select(hit, candidate, value);
                }
                Ok(self.foreign_narrow(value, e.scalar().bits()))
            }
            ValueType::Unit
            | ValueType::Address { .. }
            | ValueType::Handle { .. }
            | ValueType::NonNullHandle { .. } => {
                Err("foreign parameter type has no native lowering".into())
            }
        }
    }
    /// For a nullable parameter, the `Some` payload and its presence flag.
    fn foreign_option(
        &mut self,
        source: Source,
        ty: &Type,
        nullable: bool,
    ) -> Result<(Source, Option<ir::Value>, Type), String> {
        if !nullable {
            return Ok((source, None, ty.clone()));
        }
        let Type::Named(n, args) = ty else {
            return Err("nullable foreign argument is not an Option".into());
        };
        if n != "Option" || args.len() != 1 {
            return Err("nullable foreign argument is not an Option".into());
        }
        let Source::Memory(p) = source else {
            return Err("Option foreign argument is not in memory".into());
        };
        let layout = Layout::of(self.world, ty)?;
        let (index, fields) = layout.variant("Some").ok_or("Option lacks Some")?;
        let offset = fields.first().ok_or("Some lacks a payload")?.offset;
        let tag = self.b.ins().load(types::I64, ir::MemFlags::new(), p, 0);
        let is_some = self.b.ins().icmp_imm(IntCC::Equal, tag, index as i64);
        let payload = self.b.ins().iadd_imm(p, i64::from(offset));
        Ok((Source::Memory(payload), Some(is_some), args[0].clone()))
    }
    /// The C pointer of a single-element pointee, and the copy-back buffer
    /// when the callee may write it and it was marshalled.
    fn foreign_pointee(
        &mut self,
        pointee: &Pointee,
        source: Source,
        ty: &Type,
        writes: bool,
    ) -> Result<(ir::Value, Marshalled), String> {
        let little = self.m.isa().endianness() == ir::Endianness::Little;
        match pointee {
            Pointee::Scalar(s) => {
                let buffer = self.alloc(8);
                let word = self.foreign_word(source);
                let v = self.foreign_narrow(word, s.bits());
                self.b.ins().store(ir::MemFlags::new(), v, buffer, 0);
                Ok((buffer, writes.then_some((buffer, Shape::Scalar(*s)))))
            }
            Pointee::Struct(w) => {
                let Source::Memory(p) = source else {
                    return Err("foreign struct argument is not in memory".into());
                };
                if identical(pointee, little) {
                    return Ok((p, None));
                }
                let native = Layout::of(self.world, ty)?;
                let buffer = self
                    .alloc(u32::try_from(w.layout().size()).map_err(|_| "wire struct too large")?);
                self.foreign_write_struct(p, &native, w.layout(), buffer, 0)?;
                Ok((
                    buffer,
                    writes.then(|| (buffer, Shape::Struct(w.layout().clone(), native))),
                ))
            }
        }
    }
    fn foreign_slice(
        &mut self,
        element: &Pointee,
        source: Source,
        ty: &Type,
        writes: bool,
    ) -> Result<(ir::Value, usize, Marshalled), String> {
        let little = self.m.isa().endianness() == ir::Endianness::Little;
        let n = match ty {
            Type::Tuple(ts) => ts.len(),
            _ => 0,
        };
        if n == 0 {
            // A non-null, zero-length slice.
            return Ok((self.alloc(8), 0, None));
        }
        let Source::Memory(p) = source else {
            return Err("foreign slice argument is not in memory".into());
        };
        if identical(element, little) {
            return Ok((p, n, None));
        }
        let native = Layout::of(self.world, ty)?;
        let bytes = n
            .checked_mul(element.size())
            .and_then(|b| u32::try_from(b).ok())
            .ok_or("foreign slice buffer is too large")?;
        let buffer = self.alloc(bytes);
        self.foreign_write_slice(element, p, &native, buffer, n)?;
        Ok((
            buffer,
            n,
            writes.then(|| (buffer, Shape::Slice(element.clone(), native, n))),
        ))
    }
    fn foreign_write_slice(
        &mut self,
        element: &Pointee,
        native_ptr: ir::Value,
        native: &Layout,
        buffer: ir::Value,
        n: usize,
    ) -> Result<(), String> {
        let LayoutKind::Product(fields) = &native.kind else {
            return Err("foreign slice is not a tuple".into());
        };
        for (i, f) in fields.iter().enumerate().take(n) {
            let at = i * element.size();
            match element {
                Pointee::Scalar(s) => {
                    let v =
                        self.b
                            .ins()
                            .load(types::I64, ir::MemFlags::new(), native_ptr, f.offset);
                    let v = self.foreign_narrow(v, s.bits());
                    self.b
                        .ins()
                        .store(ir::MemFlags::new(), v, buffer, at as i32);
                }
                Pointee::Struct(w) => {
                    let inner = self.b.ins().iadd_imm(native_ptr, i64::from(f.offset));
                    self.foreign_write_struct(inner, &f.layout, w.layout(), buffer, at)?;
                }
            }
        }
        Ok(())
    }
    fn foreign_read_slice(
        &mut self,
        element: &Pointee,
        buffer: ir::Value,
        native_ptr: ir::Value,
        native: &Layout,
        n: usize,
    ) -> Result<(), String> {
        let LayoutKind::Product(fields) = &native.kind else {
            return Err("foreign slice is not a tuple".into());
        };
        for (i, f) in fields.iter().enumerate().take(n) {
            let at = i * element.size();
            match element {
                Pointee::Scalar(s) => {
                    let v =
                        self.b
                            .ins()
                            .load(c_type(s.bits()), ir::MemFlags::new(), buffer, at as i32);
                    let v = self.foreign_widen(v, s.bits(), s.signed());
                    self.b
                        .ins()
                        .store(ir::MemFlags::new(), v, native_ptr, f.offset);
                }
                Pointee::Struct(w) => {
                    let inner = self.b.ins().iadd_imm(native_ptr, i64::from(f.offset));
                    self.foreign_read_struct(buffer, at, w.layout(), inner, &f.layout)?;
                }
            }
        }
        Ok(())
    }
    fn foreign_swap(&mut self, v: ir::Value, int: &vesper_ffi::Integer) -> ir::Value {
        let little = self.m.isa().endianness() == ir::Endianness::Little;
        let swap = match int.endian {
            Endian::Native => false,
            Endian::Little => !little,
            Endian::Big => little,
        };
        if swap && int.bits > 8 {
            self.b.ins().bswap(v)
        } else {
            v
        }
    }
    fn foreign_write_struct(
        &mut self,
        native_ptr: ir::Value,
        native: &Layout,
        wire: &WireLayout,
        buffer: ir::Value,
        base: usize,
    ) -> Result<(), String> {
        for f in wire.fields() {
            let at = i32::try_from(base + f.offset()).map_err(|_| "wire offset too large")?;
            let field = || {
                native
                    .field(f.name())
                    .ok_or_else(|| format!("source struct lacks wire field '{}'", f.name()))
            };
            match f.kind() {
                FieldKind::Integer(int) => {
                    let nf = field()?;
                    let bits = u8::try_from(int.bits).map_err(|_| "wire integer too wide")?;
                    let v =
                        self.b
                            .ins()
                            .load(types::I64, ir::MemFlags::new(), native_ptr, nf.offset);
                    let v = self.foreign_narrow(v, bits);
                    let v = self.foreign_swap(v, int);
                    self.b.ins().store(ir::MemFlags::new(), v, buffer, at);
                }
                FieldKind::Bytes => {
                    let nf = field()?;
                    let LayoutKind::Product(bytes) = &nf.layout.kind else {
                        return Err("ByteArray field is not a tuple".into());
                    };
                    for (j, b) in bytes.iter().enumerate().take(f.size()) {
                        let v = self.b.ins().load(
                            types::I64,
                            ir::MemFlags::new(),
                            native_ptr,
                            nf.offset + b.offset,
                        );
                        let v = self.b.ins().ireduce(types::I8, v);
                        self.b
                            .ins()
                            .store(ir::MemFlags::new(), v, buffer, at + j as i32);
                    }
                }
                // Buffers are zeroed when allocated.
                FieldKind::Padding => {}
                FieldKind::Struct(inner) => {
                    let nf = field()?;
                    let p = self.b.ins().iadd_imm(native_ptr, i64::from(nf.offset));
                    self.foreign_write_struct(p, &nf.layout, inner, buffer, base + f.offset())?;
                }
            }
        }
        Ok(())
    }
    fn foreign_read_struct(
        &mut self,
        buffer: ir::Value,
        base: usize,
        wire: &WireLayout,
        native_ptr: ir::Value,
        native: &Layout,
    ) -> Result<(), String> {
        for f in wire.fields() {
            let at = i32::try_from(base + f.offset()).map_err(|_| "wire offset too large")?;
            let field = || {
                native
                    .field(f.name())
                    .ok_or_else(|| format!("source struct lacks wire field '{}'", f.name()))
            };
            match f.kind() {
                FieldKind::Integer(int) => {
                    let nf = field()?;
                    let bits = u8::try_from(int.bits).map_err(|_| "wire integer too wide")?;
                    let v = self
                        .b
                        .ins()
                        .load(c_type(bits), ir::MemFlags::new(), buffer, at);
                    let v = self.foreign_swap(v, int);
                    let v = self.foreign_widen(v, bits, int.signed);
                    self.b
                        .ins()
                        .store(ir::MemFlags::new(), v, native_ptr, nf.offset);
                }
                FieldKind::Bytes => {
                    let nf = field()?;
                    let LayoutKind::Product(bytes) = &nf.layout.kind else {
                        return Err("ByteArray field is not a tuple".into());
                    };
                    for (j, b) in bytes.iter().enumerate().take(f.size()) {
                        let v = self.b.ins().load(
                            types::I8,
                            ir::MemFlags::new(),
                            buffer,
                            at + j as i32,
                        );
                        let v = self.b.ins().uextend(types::I64, v);
                        self.b.ins().store(
                            ir::MemFlags::new(),
                            v,
                            native_ptr,
                            nf.offset + b.offset,
                        );
                    }
                }
                FieldKind::Padding => {}
                FieldKind::Struct(inner) => {
                    let nf = field()?;
                    let p = self.b.ins().iadd_imm(native_ptr, i64::from(nf.offset));
                    self.foreign_read_struct(buffer, base + f.offset(), inner, p, &nf.layout)?;
                }
            }
        }
        Ok(())
    }
    fn foreign_copy_back(&mut self, copy: CopyBack) -> Result<(), String> {
        let join = match copy.guard {
            Some(is_some) => {
                let then = self.b.create_block();
                let join = self.b.create_block();
                self.branch(is_some, then, &[], join, &[]);
                self.b.switch_to_block(then);
                Some(join)
            }
            None => None,
        };
        match &copy.shape {
            Shape::Scalar(s) => {
                let v = self
                    .b
                    .ins()
                    .load(c_type(s.bits()), ir::MemFlags::new(), copy.buffer, 0);
                let v = self.foreign_widen(v, s.bits(), s.signed());
                self.b.ins().store(ir::MemFlags::new(), v, copy.target, 0);
            }
            Shape::Struct(wire, native) => {
                self.foreign_read_struct(copy.buffer, 0, wire, copy.target, native)?;
            }
            Shape::Slice(element, native, n) => {
                self.foreign_read_slice(element, copy.buffer, copy.target, native, *n)?;
            }
        }
        if let Some(join) = join {
            self.jump(join, &[]);
            self.b.switch_to_block(join);
        }
        Ok(())
    }
    /// The source value of a foreign result. Bool and enum results are
    /// validated: an out-of-range raw value becomes `Err(ForeignInvalid)`.
    fn foreign_result(
        &mut self,
        result: &ValueType,
        raw: Option<ir::Value>,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        let raw = || raw.ok_or("foreign call returned no value");
        match result {
            ValueType::Unit => Ok(self.b.ins().iconst(types::I64, 0)),
            ValueType::Scalar(s) => {
                let v = raw()?;
                Ok(self.foreign_widen(v, s.bits(), s.signed()))
            }
            ValueType::Bool => {
                let v = raw()?;
                let word = self.b.ins().uextend(types::I64, v);
                let valid = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::UnsignedLessThanOrEqual, word, 1);
                self.foreign_validated(ty, valid, word, word)
            }
            ValueType::Enum(e) => {
                let v = raw()?;
                let word = self.foreign_widen(v, e.scalar().bits(), false);
                let mut valid = self.b.ins().iconst(types::I8, 0);
                let mut index = self.b.ins().iconst(types::I64, 0);
                for (i, (_, value)) in e.variants().iter().enumerate() {
                    let hit = self.b.ins().icmp_imm(IntCC::Equal, word, *value as i64);
                    let candidate = self.b.ins().iconst(types::I64, i as i64);
                    index = self.b.ins().select(hit, candidate, index);
                    valid = self.b.ins().bor(valid, hit);
                }
                self.foreign_validated(ty, valid, index, word)
            }
            ValueType::Address { .. } => Err("foreign address results have no lowering".into()),
            // The library promised a handle: `NULL` breaks the promise.
            ValueType::NonNullHandle { .. } => {
                let word = raw()?;
                self.b
                    .ins()
                    .trapz(word, ir::TrapCode::unwrap_user(ffi::NULL_HANDLE_TRAP));
                let layout = Layout::of(self.world, ty)?;
                if !layout.aggregate() {
                    return Ok(word);
                }
                let ptr = self.alloc(layout.size);
                self.b.ins().store(ir::MemFlags::new(), word, ptr, 0);
                Ok(ptr)
            }
            ValueType::Handle { nullable, .. } => {
                let word = raw()?;
                let present = self.b.ins().icmp_imm(IntCC::NotEqual, word, 0);
                if !nullable {
                    // `NULL` for a non-null handle is `Err(ForeignInvalid { raw: 0 })`.
                    return self.foreign_validated(ty, present, word, word);
                }
                let layout = Layout::of(self.world, ty)?;
                let (some, fields) = layout.variant("Some").ok_or("Option lacks Some")?;
                let offset = fields.first().ok_or("Some lacks a payload")?.offset;
                let (none, _) = layout.variant("None").ok_or("Option lacks None")?;
                let ptr = self.alloc(layout.size);
                let some = self.b.ins().iconst(types::I64, some as i64);
                let none = self.b.ins().iconst(types::I64, none as i64);
                let tag = self.b.ins().select(present, some, none);
                self.b.ins().store(ir::MemFlags::new(), tag, ptr, 0);
                self.b.ins().store(ir::MemFlags::new(), word, ptr, offset);
                Ok(ptr)
            }
        }
    }
    /// `Ok(ok)` when `valid`, else `Err(ForeignInvalid { raw })`. Both
    /// payloads are one machine word at the same offset.
    fn foreign_validated(
        &mut self,
        ty: &Type,
        valid: ir::Value,
        ok: ir::Value,
        raw: ir::Value,
    ) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, ty)?;
        let (ok_index, ok_fields) = layout.variant("Ok").ok_or("Result lacks Ok")?;
        let ok_offset = ok_fields.first().ok_or("Ok lacks a payload")?.offset;
        let (err_index, err_fields) = layout.variant("Err").ok_or("Result lacks Err")?;
        let err_offset = err_fields.first().ok_or("Err lacks a payload")?.offset;
        if ok_offset != err_offset {
            return Err("Result payload offsets differ".into());
        }
        let ptr = self.alloc(layout.size);
        let ok_tag = self.b.ins().iconst(types::I64, ok_index as i64);
        let err_tag = self.b.ins().iconst(types::I64, err_index as i64);
        let tag = self.b.ins().select(valid, ok_tag, err_tag);
        self.b.ins().store(ir::MemFlags::new(), tag, ptr, 0);
        let payload = self.b.ins().select(valid, ok, raw);
        self.b
            .ins()
            .store(ir::MemFlags::new(), payload, ptr, ok_offset);
        Ok(ptr)
    }
    /// A local leaf function defined from generated machine code, declared
    /// and defined once per module.
    fn foreign_helper(
        &mut self,
        name: &str,
        params: &[ir::Type],
        result: Option<ir::Type>,
        code: &[u8],
    ) -> Result<ir::FuncRef, String> {
        let defined = self.foreign_symbol_free(name, Linkage::Local)?;
        let mut sig = self.m.make_signature();
        sig.call_conv = cranelift_codegen::isa::CallConv::SystemV;
        for p in params {
            sig.params.push(AbiParam::new(*p));
        }
        if let Some(r) = result {
            sig.returns.push(AbiParam::new(r));
        }
        let id = self
            .m
            .declare_function(name, Linkage::Local, &sig)
            .map_err(|e| e.to_string())?;
        if !defined {
            self.m
                .define_function_bytes(id, 16, code, &[])
                .map_err(|e| e.to_string())?;
        }
        Ok(self.m.declare_func_in_func(id, self.b.func))
    }
    /// Whether function symbol `name` is already declared with `linkage`.
    /// A foreign import must not bind to a Vesper function (for example an
    /// entry exported under the same name), and a generated helper must not
    /// bind to a foreign import: both are errors, never a silent rebinding.
    fn foreign_symbol_free(&self, name: &str, linkage: Linkage) -> Result<bool, String> {
        let declarations = self.m.declarations();
        match declarations.get_name(name) {
            None => Ok(false),
            Some(cranelift_module::FuncOrDataId::Func(id))
                if declarations.get_function_decl(id).linkage == linkage =>
            {
                Ok(true)
            }
            Some(_) => Err(format!(
                "foreign symbol '{name}' collides with another symbol of this object; rename the Vesper function or the .vsys import"
            )),
        }
    }
    /// Revoke the library's protection key; returns the saved PKRU.
    fn foreign_isolate(&mut self, library: &str) -> Result<ir::Value, String> {
        let isa = self.m.isa();
        if isa.triple().architecture != target_lexicon::Architecture::X86_64
            || isa.triple().operating_system != target_lexicon::OperatingSystem::Linux
        {
            return Err("isolated(mpk) trampolines are generated for Linux x86-64 only".into());
        }
        let symbol = ffi::isolation_symbol(library);
        let defined = match self.m.declarations().get_name(&symbol) {
            Some(cranelift_module::FuncOrDataId::Data(_)) => true,
            Some(_) => {
                return Err(format!(
                    "isolation symbol '{symbol}' collides with a function of this object"
                ));
            }
            None => false,
        };
        let data = self
            .m
            .declare_data(&symbol, Linkage::Export, true, false)
            .map_err(|e| e.to_string())?;
        if !defined {
            let mut description = DataDescription::new();
            description.define(vec![0u8; 4].into_boxed_slice());
            description.set_align(4);
            self.m
                .define_data(data, &description)
                .map_err(|e| e.to_string())?;
        }
        let global = self.m.declare_data_in_func(data, self.b.func);
        let pointer = self.m.target_config().pointer_type();
        let address = self.b.ins().symbol_value(pointer, global);
        let mask = self
            .b
            .ins()
            .load(types::I32, ir::MemFlags::new(), address, 0);
        let unset = self.b.ins().icmp_imm(IntCC::Equal, mask, 0);
        let host = self.b.ins().band_imm(mask, 0b11);
        let host = self.b.ins().icmp_imm(IntCC::NotEqual, host, 0);
        let bad = self.b.ins().bor(unset, host);
        self.b
            .ins()
            .trapnz(bad, ir::TrapCode::unwrap_user(ffi::ISOLATION_TRAP));
        let rdpkru = self.foreign_helper(RDPKRU_SYMBOL, &[], Some(types::I32), &RDPKRU)?;
        let call = self.b.ins().call(rdpkru, &[]);
        let saved = self.b.inst_results(call)[0];
        let revoked = self.b.ins().bor(saved, mask);
        let wrpkru = self.foreign_helper(WRPKRU_SYMBOL, &[types::I32], None, &WRPKRU)?;
        self.b.ins().call(wrpkru, &[revoked]);
        Ok(saved)
    }
}

// ---------------------------------------------------------------------------
// SH4 swap point (#1287): `foreign_perform` in steps through the `ffi` hook
// of `codegen_swap` (none is installed by default). Only the requests and
// their word encodings live here; a declined `BEGIN` runs the Rust
// emission above unchanged.

use crate::codegen_swap::{NO_VALUE, ffi as swap, word};

fn scalar_word(s: Scalar) -> u64 {
    u64::from(s.bits()) | u64::from(s.signed()) << 8
}
fn value_words(v: &ValueType, out: &mut Vec<u64>) {
    match v {
        ValueType::Unit => out.push(0),
        ValueType::Scalar(s) => out.extend([1, scalar_word(*s)]),
        ValueType::Bool => out.push(2),
        ValueType::Enum(e) => {
            out.extend([3, scalar_word(e.scalar()), e.variants().len() as u64]);
            out.extend(e.variants().iter().map(|(_, raw)| *raw));
        }
        ValueType::Address { .. } => out.push(4),
        ValueType::Handle { nullable, .. } => out.extend([5, u64::from(*nullable)]),
        ValueType::NonNullHandle { .. } => out.push(6),
    }
}
fn integer_word(int: &vesper_ffi::Integer) -> u64 {
    u64::from(int.bits) | u64::from(int.signed) << 8
}
fn endian_word(e: Endian) -> u64 {
    match e {
        Endian::Native => 0,
        Endian::Little => 1,
        Endian::Big => 2,
    }
}
/// `W` with the source offsets of `native` (`foreign_write_struct`'s
/// field lookups, and its errors).
fn wire_words(wire: &WireLayout, native: &Layout, out: &mut Vec<u64>) -> Result<(), String> {
    out.extend([wire.size() as u64, wire.fields().len() as u64]);
    for f in wire.fields() {
        let field = || {
            native
                .field(f.name())
                .ok_or_else(|| format!("source struct lacks wire field '{}'", f.name()))
        };
        match f.kind() {
            FieldKind::Integer(int) => {
                let nf = field()?;
                u8::try_from(int.bits).map_err(|_| "wire integer too wide")?;
                out.extend([
                    0,
                    f.offset() as u64,
                    integer_word(int),
                    endian_word(int.endian),
                    nf.offset as i64 as u64,
                ]);
            }
            FieldKind::Bytes => {
                let nf = field()?;
                let LayoutKind::Product(bytes) = &nf.layout.kind else {
                    return Err("ByteArray field is not a tuple".into());
                };
                let n = bytes.len().min(f.size());
                out.extend([1, f.offset() as u64, n as u64]);
                out.extend(
                    bytes
                        .iter()
                        .take(n)
                        .map(|b| (nf.offset + b.offset) as i64 as u64),
                );
            }
            FieldKind::Padding => out.extend([2, f.offset() as u64]),
            FieldKind::Struct(inner) => {
                let nf = field()?;
                out.extend([3, f.offset() as u64, nf.offset as i64 as u64]);
                wire_words(inner, &nf.layout, out)?;
            }
        }
    }
    Ok(())
}
/// `P`; a struct pointee's source offsets come from `native` when its
/// layout is not already the C layout (`identical`), else they are unused.
fn pointee_words(
    p: &Pointee,
    native: Option<&Layout>,
    little: bool,
    out: &mut Vec<u64>,
) -> Result<(), String> {
    match p {
        Pointee::Scalar(s) => out.extend([0, scalar_word(*s)]),
        Pointee::Struct(w) => {
            out.push(1);
            match native {
                Some(n) if !identical(p, little) => wire_words(w.layout(), n, out)?,
                _ => wire_shape_words(w.layout(), out),
            }
        }
    }
    Ok(())
}
/// `W` without source offsets (an identical struct is passed in place).
fn wire_shape_words(wire: &WireLayout, out: &mut Vec<u64>) {
    out.extend([wire.size() as u64, wire.fields().len() as u64]);
    for f in wire.fields() {
        match f.kind() {
            FieldKind::Integer(int) => out.extend([
                0,
                f.offset() as u64,
                integer_word(int),
                endian_word(int.endian),
                0,
            ]),
            FieldKind::Bytes => out.extend([1, f.offset() as u64, 0]),
            FieldKind::Padding => out.extend([2, f.offset() as u64]),
            FieldKind::Struct(inner) => {
                out.extend([3, f.offset() as u64, 0]);
                wire_shape_words(inner, out);
            }
        }
    }
}
fn source_words(source: Source) -> [u64; 2] {
    match source {
        Source::Scalar(v) => [0, word(v)],
        Source::Memory(p) => [1, word(p)],
    }
}
fn value_of_word(w: u64) -> Result<ir::Value, String> {
    u32::try_from(w)
        .map(ir::Value::from_u32)
        .map_err(|_| format!("ffi hook answered {w}, which is not a value"))
}

impl<M: Module> Codegen<'_, '_, M> {
    /// One `ffi` request of a call the hook took (`BEGIN` handled): a
    /// decline here fails the compile.
    fn foreign_request(&mut self, op: u32, args: &[u64]) -> Result<Vec<u64>, String> {
        crate::codegen_swap::request(
            crate::codegen_swap::Family::Ffi,
            &mut self.b,
            &mut *self.m,
            self.world,
            self.program,
            crate::codegen_swap::Site::NONE,
            op,
            args,
            None,
        )?
        .ok_or_else(|| format!("the ffi hook declined step {op} of a call it began"))
    }
    /// The Option payload of a nullable argument, as `foreign_option`
    /// reads it: `(payload source, Some tag, payload offset, payload type)`.
    fn foreign_option_words(
        &mut self,
        source: Source,
        ty: &Type,
        nullable: bool,
    ) -> Result<(Source, u64, u64, Type), String> {
        if !nullable {
            return Ok((source, NO_VALUE, 0, ty.clone()));
        }
        let Type::Named(n, args) = ty else {
            return Err("nullable foreign argument is not an Option".into());
        };
        if n != "Option" || args.len() != 1 {
            return Err("nullable foreign argument is not an Option".into());
        }
        let Source::Memory(_) = source else {
            return Err("Option foreign argument is not in memory".into());
        };
        let layout = Layout::of(self.world, ty)?;
        let (index, fields) = layout.variant("Some").ok_or("Option lacks Some")?;
        let offset = fields.first().ok_or("Some lacks a payload")?.offset;
        Ok((source, index as u64, offset as i64 as u64, args[0].clone()))
    }
    /// `foreign_perform` through the `ffi` hook: `None` when no hook is in
    /// effect or it declined the call.
    pub(super) fn foreign_perform_swapped(
        &mut self,
        plan: &ffi::OpPlan,
        signature: &Function,
        args: &[Arg],
    ) -> Result<Option<ir::Value>, String> {
        if crate::codegen_swap::hook(crate::codegen_swap::Family::Ffi).is_none() {
            return Ok(None);
        }
        let descriptors = plan.function.parameters();
        let handler = match plan.handler {
            Handler::Direct => 0,
            Handler::IsolatedMpk => 1,
        };
        if crate::codegen_swap::request(
            crate::codegen_swap::Family::Ffi,
            &mut self.b,
            &mut *self.m,
            self.world,
            self.program,
            crate::codegen_swap::Site::NONE,
            swap::BEGIN,
            &[handler, descriptors.len() as u64],
            None,
        )?
        .is_none()
        {
            return Ok(None);
        }
        let little = u64::from(self.m.isa().endianness() == ir::Endianness::Little);
        let mut c_args = vec![];
        let mut copies: Vec<Vec<u64>> = vec![];
        let mut restores = vec![];
        let mut strings: Vec<(u64, u64)> = vec![];
        for (i, ((arg, param), descriptor)) in args
            .iter()
            .zip(&signature.params)
            .zip(descriptors)
            .enumerate()
        {
            let mut words = vec![i as u64];
            match descriptor.ty() {
                ParameterType::CStr => {
                    let view = self.expr(&arg.expr)?;
                    words.extend([2, word(view), little, 2]);
                }
                ParameterType::Slice { element, .. } if crate::seq::is_slice_type(&param.ty) => {
                    let view = self.expr(&arg.expr)?;
                    words.extend([2, word(view), little, 5, element.size() as u64]);
                }
                ty => {
                    let value = self.expr(&arg.expr)?;
                    let aggregate = Layout::of(self.world, &param.ty)?.aggregate();
                    let source = if param.pass == Pass::Inout {
                        let place = self.address(&arg.expr)?;
                        if !aggregate && let ExprKind::Name(n) = &arg.expr.kind {
                            let var =
                                *self.vars.get(n).ok_or("unknown mutable foreign argument")?;
                            restores.push((var, place, param.ty.clone()));
                        }
                        Source::Memory(place)
                    } else if aggregate {
                        Source::Memory(value)
                    } else {
                        Source::Scalar(value)
                    };
                    words.extend(source_words(source));
                    words.push(little);
                    match ty {
                        ParameterType::Value(v) => {
                            match v {
                                ValueType::Enum(_) if matches!(source, Source::Scalar(_)) => {
                                    return Err("foreign enum argument is not in memory".into());
                                }
                                ValueType::Unit
                                | ValueType::Address { .. }
                                | ValueType::Handle { .. }
                                | ValueType::NonNullHandle { .. } => {
                                    return Err(
                                        "foreign parameter type has no native lowering".into()
                                    );
                                }
                                _ => {}
                            }
                            words.push(0);
                            value_words(v, &mut words);
                        }
                        ParameterType::Handle { .. } => words.push(1),
                        ParameterType::CStr => unreachable!("C strings are lowered above"),
                        ParameterType::Pointer {
                            pointee,
                            access,
                            nullable,
                        } => {
                            let (payload, some, offset, payload_ty) =
                                self.foreign_option_words(source, &param.ty, *nullable)?;
                            let writes = *access != PointerAccess::In;
                            let native = match pointee {
                                Pointee::Struct(_) => {
                                    if !matches!(payload, Source::Memory(_)) {
                                        return Err(
                                            "foreign struct argument is not in memory".into()
                                        );
                                    }
                                    if identical(pointee, little == 1) {
                                        None
                                    } else {
                                        Some(Layout::of(self.world, &payload_ty)?)
                                    }
                                }
                                Pointee::Scalar(_) => {
                                    if writes && matches!(payload, Source::Scalar(_)) {
                                        return Err("writable foreign pointer lacks a place".into());
                                    }
                                    None
                                }
                            };
                            words.extend([
                                3,
                                u64::from(writes),
                                u64::from(*nullable),
                                some,
                                offset,
                            ]);
                            pointee_words(pointee, native.as_ref(), little == 1, &mut words)?;
                        }
                        ParameterType::Slice {
                            element,
                            access,
                            nullable,
                        } => {
                            let (payload, some, offset, payload_ty) =
                                self.foreign_option_words(source, &param.ty, *nullable)?;
                            let writes = *access == Access::Inout;
                            let n = match &payload_ty {
                                Type::Tuple(ts) => ts.len(),
                                _ => 0,
                            };
                            let mut offsets = vec![];
                            let mut native = None;
                            if n > 0 {
                                if !matches!(payload, Source::Memory(_)) {
                                    return Err("foreign slice argument is not in memory".into());
                                }
                                if !identical(element, little == 1) {
                                    let layout = Layout::of(self.world, &payload_ty)?;
                                    n.checked_mul(element.size())
                                        .and_then(|b| u32::try_from(b).ok())
                                        .ok_or("foreign slice buffer is too large")?;
                                    let LayoutKind::Product(fields) = &layout.kind else {
                                        return Err("foreign slice is not a tuple".into());
                                    };
                                    offsets = fields
                                        .iter()
                                        .take(n)
                                        .map(|f| f.offset as i64 as u64)
                                        .collect();
                                    native = fields.first().map(|f| (*f.layout).clone());
                                }
                            }
                            words.extend([
                                4,
                                u64::from(writes),
                                u64::from(*nullable),
                                some,
                                offset,
                                n as u64,
                                element.size() as u64,
                            ]);
                            pointee_words(element, native.as_ref(), little == 1, &mut words)?;
                            words.extend(offsets);
                        }
                    }
                }
            }
            let r = self.foreign_request(swap::ARG, &words)?;
            let bad = || "malformed ffi ARG answer".to_string();
            let k = *r.first().ok_or_else(bad)? as usize;
            let c = r.get(1..1 + k).ok_or_else(bad)?;
            for w in c {
                c_args.push(value_of_word(*w)?);
            }
            let m = *r.get(1 + k).ok_or_else(bad)? as usize;
            let copy = r.get(2 + k..2 + k + m).ok_or_else(bad)?;
            if m > 0 {
                copies.push(copy.to_vec());
            }
            let s = *r.get(2 + k + m).ok_or_else(bad)? as usize;
            if s == 2 {
                strings.push((r[3 + k + m], r[4 + k + m]));
            }
        }
        let abi = plan.function.abi();
        let pointer = self.m.target_config().pointer_type();
        let mut sig = self.m.make_signature();
        for t in &abi.parameters {
            sig.params.push(abi_param(*t, pointer));
        }
        if let Some(t) = abi.result {
            sig.returns.push(abi_param(t, pointer));
        }
        let symbol = plan.function.link_symbol();
        self.foreign_symbol_free(symbol, Linkage::Import)?;
        let callee = self
            .m
            .declare_function(symbol, Linkage::Import, &sig)
            .map_err(|e| format!("foreign symbol '{symbol}': {e}"))?;
        let callee = self.m.declare_func_in_func(callee, self.b.func);
        let refusal = if strings.is_empty() {
            None
        } else {
            let layout = Layout::of(self.world, &signature.result)?;
            let (err, err_fields) = layout.variant("Err").ok_or("Result lacks Err")?;
            let err_offset = err_fields.first().ok_or("Err lacks a payload")?.offset;
            let mut words = vec![u64::from(layout.size), err as u64, err_offset as i64 as u64];
            words.extend(strings.iter().map(|(_, at)| *at));
            let r = self.foreign_request(swap::REFUSE, &words)?;
            let [slot, join] = r[..] else {
                return Err("malformed ffi REFUSE answer".into());
            };
            let join = u32::try_from(join)
                .map(ir::Block::from_u32)
                .map_err(|_| "malformed ffi REFUSE answer")?;
            Some((layout, value_of_word(slot)?, join))
        };
        let errno = if plan.function.reads_errno() {
            match ffi::os_error_symbol(self.m.isa().triple())? {
                ("__errno_location", _) => 1,
                ("__error", _) => 2,
                _ => 3,
            }
        } else {
            0
        };
        let mut words = vec![
            u64::from(callee.as_u32()),
            errno,
            u64::from(abi.result.is_some()),
            little,
            c_args.len() as u64,
        ];
        words.extend(c_args.iter().map(|v| word(*v)));
        words.push(copies.len() as u64);
        for c in &copies {
            words.push(c.len() as u64);
            words.extend(c);
        }
        let r = self.foreign_request(swap::CALL, &words)?;
        let [raw, os] = r[..] else {
            return Err("malformed ffi CALL answer".into());
        };
        let raw = if raw == NO_VALUE {
            None
        } else {
            Some(value_of_word(raw)?)
        };
        let os_error = if os == NO_VALUE {
            None
        } else {
            Some(value_of_word(os)?)
        };
        for (var, ptr, ty) in restores {
            let value = self.load(ptr, &ty)?;
            self.def_var(var, value);
            if let Some(destination) = self.pointer(var) {
                self.b
                    .ins()
                    .store(ir::MemFlags::new(), value, destination, 0);
            }
        }
        let mut ty = signature.result.clone();
        if refusal.is_some() {
            ty = result_ok(&ty)?;
        }
        let value_ty = match (&ty, os_error) {
            (Type::Tuple(ts), Some(_)) if ts.len() == 2 => ts[0].clone(),
            (_, Some(_)) => return Err("errno result is not a pair".into()),
            (t, None) => t.clone(),
        };
        let mut words = vec![raw.map_or(NO_VALUE, word)];
        let result = plan.function.result();
        value_words(result, &mut words);
        self.foreign_result_layout(result, raw.is_some(), &value_ty, &mut words)?;
        let r = self.foreign_request(swap::RESULT, &words)?;
        let mut value = value_of_word(*r.first().ok_or("malformed ffi RESULT answer")?)?;
        if let Some(code) = os_error {
            let layout = Layout::of(self.world, &ty)?;
            let LayoutKind::Product(fields) = &layout.kind else {
                return Err("errno result is not a pair".into());
            };
            let (first, second) = (fields[0].offset, fields[1].offset);
            let pair = self.alloc(layout.size);
            let at = self.offset(pair, first);
            self.store(at, value, &value_ty)?;
            let at = self.offset(pair, second);
            self.store(at, code, &Type::named("u32"))?;
            value = pair;
        }
        if let Some((layout, slot, join)) = refusal {
            let (ok, ok_fields) = layout.variant("Ok").ok_or("Result lacks Ok")?;
            let ok_offset = ok_fields.first().ok_or("Ok lacks a payload")?.offset;
            let tag = self.b.ins().iconst(types::I64, ok as i64);
            self.b.ins().store(ir::MemFlags::new(), tag, slot, 0);
            let at = self.offset(slot, ok_offset);
            self.store(at, value, &ty)?;
            self.jump(join, &[]);
            self.b.switch_to_block(join);
            self.b.seal_block(join);
            value = slot;
        }
        if !strings.is_empty() {
            let buffers: Vec<u64> = strings.iter().map(|(b, _)| *b).collect();
            self.foreign_request(swap::FINISH, &buffers)?;
        }
        Ok(Some(value))
    }
    /// The layout words `RESULT` needs for result `v` of source type `ty`,
    /// with `foreign_result`'s errors: `[size, ok, ok offset, err, err
    /// offset]` for a validated result (bool, enum, non-null handle),
    /// `[aggregate, size]` for a `NonNull` handle and `[size, some, some
    /// offset, none]` for a nullable handle.
    fn foreign_result_layout(
        &mut self,
        v: &ValueType,
        raw: bool,
        ty: &Type,
        out: &mut Vec<u64>,
    ) -> Result<(), String> {
        let needs_raw = !matches!(v, ValueType::Unit | ValueType::Address { .. });
        if needs_raw && !raw {
            return Err("foreign call returned no value".into());
        }
        let validated = |world: &World, out: &mut Vec<u64>| -> Result<(), String> {
            let layout = Layout::of(world, ty)?;
            let (ok_index, ok_fields) = layout.variant("Ok").ok_or("Result lacks Ok")?;
            let ok_offset = ok_fields.first().ok_or("Ok lacks a payload")?.offset;
            let (err_index, err_fields) = layout.variant("Err").ok_or("Result lacks Err")?;
            let err_offset = err_fields.first().ok_or("Err lacks a payload")?.offset;
            if ok_offset != err_offset {
                return Err("Result payload offsets differ".into());
            }
            out.extend([
                u64::from(layout.size),
                ok_index as u64,
                ok_offset as i64 as u64,
                err_index as u64,
                err_offset as i64 as u64,
            ]);
            Ok(())
        };
        match v {
            ValueType::Unit | ValueType::Scalar(_) => Ok(()),
            ValueType::Bool | ValueType::Enum(_) => validated(self.world, out),
            ValueType::Address { .. } => Err("foreign address results have no lowering".into()),
            ValueType::NonNullHandle { .. } => {
                let layout = Layout::of(self.world, ty)?;
                out.extend([u64::from(layout.aggregate()), u64::from(layout.size)]);
                Ok(())
            }
            ValueType::Handle { nullable, .. } => {
                if !nullable {
                    return validated(self.world, out);
                }
                let layout = Layout::of(self.world, ty)?;
                let (some, fields) = layout.variant("Some").ok_or("Option lacks Some")?;
                let offset = fields.first().ok_or("Some lacks a payload")?.offset;
                let (none, _) = layout.variant("None").ok_or("Option lacks None")?;
                out.extend([
                    u64::from(layout.size),
                    some as u64,
                    offset as i64 as u64,
                    none as u64,
                ]);
                Ok(())
            }
        }
    }
}
