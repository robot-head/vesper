//! Cranelift JIT/AOT lowering for `ChunkSeq[T]` and `Slice[T]` (§13).
//!
//! A sequence is a handle to a `{len, cap, data}` header; elements live at
//! `data + i * stride` with the packed stride of [`seq::elem_bytes`]. A slice
//! is a 24-byte value `{handle, start, len}`. Only allocation and growth call
//! the runtime (`vesper_seq_new`, `vesper_seq_grow`); every other operation,
//! including bounds checks that raise `Fail[RuntimeError]`, is inline.
use super::*;
use crate::seq::{self, SeqOp};
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, condcodes::IntCC, types};
use cranelift_module::{Linkage, Module};

#[path = "aot_runtime.rs"]
pub(crate) mod aot_runtime;

const LEN: i32 = 0;
const CAP: i32 = 8;
const DATA: i32 = 16;

impl<M: Module> Codegen<'_, '_, M> {
    fn seq_runtime_call(&mut self, name: &str, args: &[ir::Value]) -> Result<(), String> {
        let jit = std::any::type_name::<M>().contains("JITModule");
        if !jit {
            if self.m.isa().triple().operating_system == target_lexicon::OperatingSystem::None_ {
                return Err(
                    "ChunkSeq in a freestanding image needs an allocator; the generated one uses the C library".into(),
                );
            }
            aot_runtime::define(self.m)?;
        }
        let mut signature = self.m.make_signature();
        signature
            .params
            .extend(args.iter().map(|_| AbiParam::new(types::I64)));
        signature.returns.push(AbiParam::new(types::I64));
        let linkage = if jit { Linkage::Import } else { Linkage::Local };
        let id = self
            .m
            .declare_function(name, linkage, &signature)
            .map_err(|e| e.to_string())?;
        let reference = self.m.declare_func_in_func(id, self.b.func);
        self.b.ins().call(reference, args);
        Ok(())
    }

    fn seq_word(&mut self, ptr: ir::Value, offset: i32) -> ir::Value {
        self.b.ins().load(types::I64, MemFlags::new(), ptr, offset)
    }

    fn seq_set_word(&mut self, ptr: ir::Value, offset: i32, value: ir::Value) {
        self.b.ins().store(MemFlags::new(), value, ptr, offset);
    }

    fn seq_stride(&self, elem: &Type) -> Result<i64, String> {
        if contains_function(self.world, elem) {
            return Err(format!(
                "native ChunkSeq elements cannot contain functions, found {elem}"
            ));
        }
        seq::elem_bytes(self.world, elem)
    }

    /// The address of element `i` of the buffer of `handle`, from `start`.
    fn seq_elem_addr(
        &mut self,
        handle: ir::Value,
        start: ir::Value,
        i: ir::Value,
        stride: i64,
    ) -> ir::Value {
        let data = self.seq_word(handle, DATA);
        let at = self.b.ins().iadd(start, i);
        let offset = self.b.ins().imul_imm(at, stride);
        self.b.ins().iadd(data, offset)
    }

    fn seq_load_elem(&mut self, addr: ir::Value, elem: &Type) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, elem)?;
        if layout.aggregate() {
            let copy = self.alloc(layout.size);
            self.copy_bytes(addr, copy, layout.size);
            return Ok(copy);
        }
        Ok(match self.seq_stride(elem)? {
            1 => self.b.ins().uload8(types::I64, MemFlags::new(), addr, 0),
            4 => self.b.ins().uload32(MemFlags::new(), addr, 0),
            _ => self
                .b
                .ins()
                .load(scalar(elem, &[])?, MemFlags::new(), addr, 0),
        })
    }

    fn seq_store_elem(
        &mut self,
        addr: ir::Value,
        value: ir::Value,
        elem: &Type,
    ) -> Result<(), String> {
        let layout = Layout::of(self.world, elem)?;
        if layout.aggregate() {
            self.copy_bytes(value, addr, layout.size);
            return Ok(());
        }
        let value = self.coerce(value, Some(elem));
        match self.seq_stride(elem)? {
            1 => {
                self.b.ins().istore8(MemFlags::new(), value, addr, 0);
            }
            4 => {
                self.b.ins().istore32(MemFlags::new(), value, addr, 0);
            }
            _ => {
                self.b.ins().store(MemFlags::new(), value, addr, 0);
            }
        }
        Ok(())
    }

    /// Raises `IndexOutOfBounds` unless `valid` holds.
    fn seq_guard(&mut self, valid: ir::Value) -> Result<(), String> {
        let good = self.b.create_block();
        let bad = self.b.create_block();
        self.branch(valid, good, &[], bad, &[]);
        self.b.switch_to_block(bad);
        self.primitive_failure("IndexOutOfBounds")?;
        self.jump(good, &[]);
        self.b.switch_to_block(good);
        Ok(())
    }

    /// Grows `handle` to hold `needed` elements when its capacity is short.
    fn seq_reserve(
        &mut self,
        handle: ir::Value,
        needed: ir::Value,
        stride: i64,
    ) -> Result<(), String> {
        let cap = self.seq_word(handle, CAP);
        let fits = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, needed, cap);
        let grow = self.b.create_block();
        let done = self.b.create_block();
        self.branch(fits, done, &[], grow, &[]);
        self.b.switch_to_block(grow);
        let stride = self.b.ins().iconst(types::I64, stride);
        self.seq_runtime_call("vesper_seq_grow", &[handle, stride, needed])?;
        self.jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(())
    }

    /// `(handle, start, len)` of a sequence or slice operand.
    fn seq_view(&mut self, value: ir::Value, ty: &Type) -> (ir::Value, ir::Value, ir::Value) {
        if seq::is_seq_type(ty) {
            let len = self.seq_word(value, LEN);
            (value, self.zero(), len)
        } else {
            let h = self.seq_word(value, 0);
            let start = self.seq_word(value, 8);
            let len = self.seq_word(value, 16);
            (h, start, len)
        }
    }

    fn seq_slice_value(&mut self, h: ir::Value, start: ir::Value, len: ir::Value) -> ir::Value {
        let ptr = self.alloc(24);
        self.seq_set_word(ptr, 0, h);
        self.seq_set_word(ptr, 8, start);
        self.seq_set_word(ptr, 16, len);
        ptr
    }

    /// Lowers one sequence operation on its operand values.
    pub(super) fn seq_op(
        &mut self,
        op: SeqOp,
        args: &[ir::Value],
        arg_types: &[Type],
        result: &Type,
    ) -> Result<ir::Value, String> {
        let elem = if op.has_receiver() {
            arg_types.first().and_then(seq::elem)
        } else {
            seq::elem(result)
        }
        .cloned()
        .ok_or_else(|| format!("native ChunkSeq::{} lacks its element type", op.name()))?;
        let elem = crate::numeric::default_type(&elem);
        let stride = self.seq_stride(&elem)?;
        let arg = |i: usize| {
            args.get(i)
                .copied()
                .ok_or_else(|| format!("native ChunkSeq::{} lacks an operand", op.name()))
        };
        Ok(match op {
            SeqOp::New | SeqOp::WithCapacity => {
                let cap = if op == SeqOp::New {
                    self.zero()
                } else {
                    arg(0)?
                };
                let stride = self.b.ins().iconst(types::I64, stride);
                let mut signature = self.m.make_signature();
                signature.params.push(AbiParam::new(types::I64));
                signature.params.push(AbiParam::new(types::I64));
                signature.returns.push(AbiParam::new(types::I64));
                let jit = std::any::type_name::<M>().contains("JITModule");
                if !jit {
                    if self.m.isa().triple().operating_system
                        == target_lexicon::OperatingSystem::None_
                    {
                        return Err(
                            "ChunkSeq in a freestanding image needs an allocator; the generated one uses the C library".into(),
                        );
                    }
                    aot_runtime::define(self.m)?;
                }
                let linkage = if jit { Linkage::Import } else { Linkage::Local };
                let id = self
                    .m
                    .declare_function("vesper_seq_new", linkage, &signature)
                    .map_err(|e| e.to_string())?;
                let reference = self.m.declare_func_in_func(id, self.b.func);
                let call = self.b.ins().call(reference, &[cap, stride]);
                self.b.inst_results(call)[0]
            }
            SeqOp::Len => self.seq_word(arg(0)?, LEN),
            SeqOp::SliceLen => self.seq_word(arg(0)?, 16),
            SeqOp::Push => {
                let h = arg(0)?;
                let len = self.seq_word(h, LEN);
                let next = self.b.ins().iadd_imm(len, 1);
                self.seq_reserve(h, next, stride)?;
                let zero = self.zero();
                let addr = self.seq_elem_addr(h, zero, len, stride);
                self.seq_store_elem(addr, arg(1)?, &elem)?;
                self.seq_set_word(h, LEN, next);
                self.zero()
            }
            SeqOp::Pop => {
                let option = Type::Named("Option".into(), vec![elem.clone()]);
                let h = arg(0)?;
                let len = self.seq_word(h, LEN);
                let present = self.b.ins().icmp_imm(IntCC::NotEqual, len, 0);
                self.seq_option(&option, &elem, present, |this| {
                    let last = this.b.ins().iadd_imm(len, -1);
                    let zero = this.zero();
                    let addr = this.seq_elem_addr(h, zero, last, stride);
                    let value = this.seq_load_elem(addr, &elem)?;
                    this.seq_set_word(h, LEN, last);
                    Ok(value)
                })?
            }
            SeqOp::Get | SeqOp::Set | SeqOp::SliceGet | SeqOp::SliceSet => {
                let (h, start, len) = self.seq_view(arg(0)?, &arg_types[0]);
                let i = arg(1)?;
                let valid = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                self.seq_guard(valid)?;
                let addr = self.seq_elem_addr(h, start, i, stride);
                if matches!(op, SeqOp::Get | SeqOp::SliceGet) {
                    self.seq_load_elem(addr, &elem)?
                } else {
                    self.seq_store_elem(addr, arg(2)?, &elem)?;
                    self.zero()
                }
            }
            SeqOp::TryGet | SeqOp::TrySet => {
                let (h, start, len) = self.seq_view(arg(0)?, &arg_types[0]);
                let i = arg(1)?;
                let valid = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                if op == SeqOp::TrySet {
                    let yes = self.b.create_block();
                    let no = self.b.create_block();
                    let done = self.b.create_block();
                    self.branch(valid, yes, &[], no, &[]);
                    self.b.switch_to_block(yes);
                    let addr = self.seq_elem_addr(h, start, i, stride);
                    self.seq_store_elem(addr, arg(2)?, &elem)?;
                    self.jump(done, &[]);
                    self.b.switch_to_block(no);
                    self.jump(done, &[]);
                    self.b.switch_to_block(done);
                    self.b.ins().uextend(types::I64, valid)
                } else {
                    let option = Type::Named("Option".into(), vec![elem.clone()]);
                    self.seq_option(&option, &elem, valid, |this| {
                        let addr = this.seq_elem_addr(h, start, i, stride);
                        this.seq_load_elem(addr, &elem)
                    })?
                }
            }
            SeqOp::Clear => {
                let zero = self.zero();
                self.seq_set_word(arg(0)?, LEN, zero);
                zero
            }
            SeqOp::Truncate => {
                let h = arg(0)?;
                let n = arg(1)?;
                let len = self.seq_word(h, LEN);
                let shorter = self.b.ins().icmp(IntCC::UnsignedLessThan, n, len);
                let next = self.b.ins().select(shorter, n, len);
                self.seq_set_word(h, LEN, next);
                self.zero()
            }
            SeqOp::Extend => {
                let h = arg(0)?;
                let (sh, sstart, slen) = self.seq_view(arg(1)?, &arg_types[1]);
                let len = self.seq_word(h, LEN);
                let needed = self.b.ins().iadd(len, slen);
                self.seq_reserve(h, needed, stride)?;
                let zero = self.zero();
                let dst = self.seq_elem_addr(h, zero, len, stride);
                let src = self.seq_elem_addr(sh, sstart, zero, stride);
                let bytes = self.b.ins().imul_imm(slen, stride);
                self.seq_copy(dst, src, bytes);
                self.seq_set_word(h, LEN, needed);
                self.zero()
            }
            SeqOp::Finish => {
                let len = self.seq_word(arg(0)?, LEN);
                let empty = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                self.seq_guard(empty)?;
                self.zero()
            }
            SeqOp::AsSlice => {
                let (h, start, len) = self.seq_view(arg(0)?, &arg_types[0]);
                self.seq_slice_value(h, start, len)
            }
            SeqOp::SubSlice => {
                let (h, start, len) = self.seq_view(arg(0)?, &arg_types[0]);
                let lo = arg(1)?;
                let hi = arg(2)?;
                let ordered = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, lo, hi);
                let inside = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, hi, len);
                let valid = self.b.ins().band(ordered, inside);
                let good = self.b.create_block();
                let bad = self.b.create_block();
                let done = self.b.create_block();
                self.branch(valid, good, &[], bad, &[]);
                // An interval fault reports and continues with an empty view
                // in the JIT, and exits in an executable.
                self.b.switch_to_block(bad);
                let code = self.b.ins().iconst(types::I64, 1);
                self.seq_runtime_call("vesper_seq_fault", &[code])?;
                self.jump(done, &[]);
                self.b.switch_to_block(good);
                self.jump(done, &[]);
                self.b.switch_to_block(done);
                let zero = self.zero();
                let at = self.b.ins().iadd(start, lo);
                let at = self.b.ins().select(valid, at, zero);
                let n = self.b.ins().isub(hi, lo);
                let n = self.b.ins().select(valid, n, zero);
                self.seq_slice_value(h, at, n)
            }
        })
    }

    /// An `Option[elem]` that is `Some(load())` when `present` holds.
    fn seq_option(
        &mut self,
        option: &Type,
        elem: &Type,
        present: ir::Value,
        load: impl FnOnce(&mut Self) -> Result<ir::Value, String>,
    ) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, option)?;
        let (some_tag, some_fields) = layout.variant("Some").ok_or("native Option lacks Some")?;
        let payload = some_fields
            .first()
            .map(|f| f.offset)
            .ok_or("native Option lacks a payload")?;
        let (none_tag, _) = layout.variant("None").ok_or("native Option lacks None")?;
        let out = self.alloc(layout.size);
        let none = self.b.create_block();
        let some = self.b.create_block();
        let done = self.b.create_block();
        self.branch(present, some, &[], none, &[]);
        self.b.switch_to_block(none);
        let tag = self.b.ins().iconst(types::I64, none_tag as i64);
        self.seq_set_word(out, 0, tag);
        self.jump(done, &[]);
        self.b.switch_to_block(some);
        let value = load(self)?;
        let tag = self.b.ins().iconst(types::I64, some_tag as i64);
        self.seq_set_word(out, 0, tag);
        let slot = self.offset(out, payload);
        self.store(slot, value, elem)?;
        self.jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(out)
    }

    /// Copies `bytes` bytes from `src` to `dst` (distinct buffers).
    fn seq_copy(&mut self, dst: ir::Value, src: ir::Value, bytes: ir::Value) {
        let head = self.b.create_block();
        let body = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(head, types::I64);
        let zero = self.zero();
        self.jump(head, &[zero.into()]);
        self.b.switch_to_block(head);
        let k = self.b.block_params(head)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, bytes);
        self.branch(more, body, &[], done, &[]);
        self.b.switch_to_block(body);
        let from = self.b.ins().iadd(src, k);
        let byte = self.b.ins().uload8(types::I64, MemFlags::new(), from, 0);
        let to = self.b.ins().iadd(dst, k);
        self.b.ins().istore8(MemFlags::new(), byte, to, 0);
        let next = self.b.ins().iadd_imm(k, 1);
        self.jump(head, &[next.into()]);
        self.b.switch_to_block(done);
    }

    /// The AST form of a sequence call, element index or slice view.
    pub(super) fn seq_ast_expr(&mut self, e: &Expr) -> Option<Result<ir::Value, String>> {
        let (op, operands): (SeqOp, Vec<&Expr>) = match &e.kind {
            ExprKind::Index(base, index) => {
                if let Some((base, lo, hi)) = seq::interval(e) {
                    (SeqOp::SubSlice, vec![base, lo, hi])
                } else {
                    let t = self.ty(base);
                    let op = if seq::is_seq_type(&t) {
                        SeqOp::Get
                    } else if seq::is_slice_type(&t) {
                        SeqOp::SliceGet
                    } else {
                        return None;
                    };
                    (op, vec![base, index])
                }
            }
            ExprKind::Call { callee, args, .. } => match &callee.kind {
                ExprKind::Name(n) if !self.vars.contains_key(n) => {
                    if self.world.functions.contains_key(n) {
                        return None;
                    }
                    (
                        seq::resolve(n, None)?,
                        args.iter().map(|a| &a.expr).collect(),
                    )
                }
                ExprKind::Field(base, method) => {
                    let t = self.ty(base);
                    let op = seq::resolve(method, Some(&t))?;
                    let mut operands = vec![base.as_ref()];
                    operands.extend(args.iter().map(|a| &a.expr));
                    (op, operands)
                }
                _ => return None,
            },
            _ => return None,
        };
        Some((|| {
            let mut values = vec![];
            let mut types = vec![];
            for o in operands {
                values.push(self.expr(o)?);
                types.push(self.ty(o));
            }
            let result = self.ty(e);
            self.seq_op(op, &values, &types, &result)
        })())
    }
}
