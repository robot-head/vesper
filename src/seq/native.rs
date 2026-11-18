//! Cranelift JIT/AOT lowering for `ChunkSeq[T]` and `Slice[T]` (§13).
//!
//! A sequence is a handle to a `{len, cap, data}` header; elements live at
//! `data + i * stride` with the packed stride of [`seq::elem_bytes`]. A slice
//! is a 24-byte value `{handle, start, len}`. Only allocation and growth call
//! the runtime (`vesper_seq_new`, `vesper_seq_grow`); every other operation,
//! including bounds checks that raise `Fail[RuntimeError]`, is inline.
//!
//! Drops: the slots of [`crate::fip::seq_drop_plan`] are freed on every exit
//! of their function (`vesper_seq_free`, `vesper_str_release`), when they
//! are bound or assigned again, and a sequence moved out of its slot empties
//! it, so each sequence is freed once by its last owner.
use super::*;
use crate::seq::{self, SeqOp};
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, condcodes::IntCC, types};
use cranelift_module::{Linkage, Module};

#[path = "aot_runtime.rs"]
pub(crate) mod aot_runtime;
#[path = "native_bytes.rs"]
mod bytes;
#[path = "../bounds_hoist/native.rs"]
mod hoisted;

const LEN: i32 = 0;
const CAP: i32 = 8;
const DATA: i32 = 16;

/// A slot the current function frees on exit ([`crate::fip::seq_drop_plan`]).
#[derive(Clone)]
pub(super) struct SeqDropSlot {
    var: Variable,
    kind: crate::fip::SeqDrop,
    /// A `let`-bound string's own storage: the slot is bound to it so that
    /// a later binding can free the string it replaces.
    storage: Option<ir::Value>,
    /// [`crate::fip::SeqDrop::Fields`]: the byte offsets of the struct's
    /// `ChunkSeq` handles.
    fields: Vec<i32>,
}

/// The offsets of every `ChunkSeq` handle in a value of `layout`, when each
/// one sits in a (nested) product field; `None` when a sequence or slice is
/// reachable some other way (an enum payload, a string, a view).
fn seq_field_offsets(world: &World, layout: &Layout, base: i32, out: &mut Vec<i32>) -> bool {
    let LayoutKind::Product(fields) = &layout.kind else {
        return false;
    };
    for f in fields {
        if seq::is_seq_type(&f.ty) {
            out.push(base + f.offset);
        } else if !seq::holds_sequence(world, &f.ty) {
            continue;
        } else if seq::is_slice_type(&f.ty)
            || !seq_field_offsets(world, &f.layout, base + f.offset, out)
        {
            return false;
        }
    }
    true
}

impl<M: Module> Codegen<'_, '_, M> {
    pub(super) fn seq_runtime_call(
        &mut self,
        name: &str,
        args: &[ir::Value],
    ) -> Result<(), String> {
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

    /// The drop slots of graph `f`, with their string storage allocated and
    /// emptied. `only` limits a nested graph to the locals it declares.
    pub(super) fn seq_drop_slots(
        &mut self,
        f: &crate::ir::FunctionIr,
        only: Option<&std::collections::BTreeSet<String>>,
    ) -> Result<Vec<SeqDropSlot>, String> {
        let mut slots = vec![];
        let jit = std::any::type_name::<M>().contains("JITModule");
        if !jit && self.m.isa().triple().operating_system == target_lexicon::OperatingSystem::None_
        {
            // A freestanding image has no allocator to return buffers to.
            return Ok(slots);
        }
        for (name, kind) in crate::fip::seq_drop_plan(self.world, f) {
            let param = f.params.iter().any(|p| p.name == name);
            if only.is_some_and(|names| param || !names.contains(&name)) {
                continue;
            }
            let Some(&var) = self.vars.get(&name) else {
                continue;
            };
            let mut fields = vec![];
            if kind == crate::fip::SeqDrop::Fields {
                let ty =
                    crate::numeric::default_type(&substitute(&f.locals[&name], &self.generic_vars));
                let layout = Layout::of(self.world, &ty)?;
                if !self.operands_only
                    || !seq_field_offsets(self.world, &layout, 0, &mut fields)
                    || fields.is_empty()
                {
                    continue;
                }
            }
            let storage = if kind == crate::fip::SeqDrop::Str && !param {
                let storage = self.alloc(crate::seq::HEADER_BYTES as u32);
                let zero = self.zero();
                for offset in [LEN, CAP, DATA] {
                    self.seq_set_word(storage, offset, zero);
                }
                Some(storage)
            } else if kind == crate::fip::SeqDrop::Fields {
                // Its own storage, so that a binding in a loop (whose
                // struct literal reuses one temporary) frees the struct it
                // replaces, not the one it binds.
                let ty =
                    crate::numeric::default_type(&substitute(&f.locals[&name], &self.generic_vars));
                Some(self.alloc(Layout::of(self.world, &ty)?.size))
            } else {
                None
            };
            slots.push(SeqDropSlot {
                var,
                kind,
                storage,
                fields,
            });
        }
        Ok(slots)
    }

    /// The drop slot that the local `name` names in the graphs being emitted
    /// (a handled block names its function's slots through aliases).
    fn seq_drop_slot(&self, name: &str) -> Option<SeqDropSlot> {
        let var = *self.vars.get(name)?;
        self.exits
            .iter()
            .flat_map(|t| &t.drops)
            .find(|d| d.var == var)
            .cloned()
    }

    /// Frees what drop `slot` holds now.
    fn seq_drop_free(&mut self, slot: &SeqDropSlot) -> Result<(), String> {
        if slot.kind == crate::fip::SeqDrop::Fields {
            // A struct local not bound yet is a null pointer; a moved one
            // holds zero handles.
            let ptr = self.use_var(slot.var);
            let bound = self.b.ins().icmp_imm(IntCC::NotEqual, ptr, 0);
            let fields = self.b.create_block();
            let done = self.b.create_block();
            self.branch(bound, fields, &[], done, &[]);
            self.b.switch_to_block(fields);
            for &offset in &slot.fields {
                let handle = self.seq_word(ptr, offset);
                let present = self.b.ins().icmp_imm(IntCC::NotEqual, handle, 0);
                let free = self.b.create_block();
                let next = self.b.create_block();
                self.branch(present, free, &[], next, &[]);
                self.b.switch_to_block(free);
                self.seq_runtime_call("vesper_seq_free", &[handle])?;
                let zero = self.zero();
                self.seq_set_word(ptr, offset, zero);
                self.jump(next, &[]);
                self.b.switch_to_block(next);
            }
            self.jump(done, &[]);
            self.b.switch_to_block(done);
            return Ok(());
        }
        let (value, function) = match (slot.kind, slot.storage) {
            (crate::fip::SeqDrop::Seq, _) => (self.use_var(slot.var), "vesper_seq_free"),
            (crate::fip::SeqDrop::Str, Some(storage)) => (storage, "vesper_str_release"),
            (crate::fip::SeqDrop::Str, None) => (self.use_var(slot.var), "vesper_str_release"),
            (crate::fip::SeqDrop::Fields, _) => unreachable!("struct drops return above"),
        };
        // An empty slot (never bound, or moved from) holds 0.
        let present = self.b.ins().icmp_imm(IntCC::NotEqual, value, 0);
        let free = self.b.create_block();
        let done = self.b.create_block();
        self.branch(present, free, &[], done, &[]);
        self.b.switch_to_block(free);
        self.seq_runtime_call(function, &[value])?;
        self.jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(())
    }

    /// Frees every drop slot of the graphs from `depth` outward: an exit
    /// from graph `depth` and the graphs nested in it.
    pub(super) fn seq_drop_exit(&mut self, depth: usize) -> Result<(), String> {
        let slots: Vec<SeqDropSlot> = self
            .exits
            .iter()
            .skip(depth)
            .flat_map(|t| t.drops.clone())
            .collect();
        for slot in &slots {
            self.seq_drop_free(slot)?;
        }
        Ok(())
    }

    /// A sequence moved out of local `name` leaves its slot empty.
    pub(super) fn seq_drop_moved(&mut self, name: &str) {
        let Some(slot) = self.seq_drop_slot(name) else {
            return;
        };
        match slot.kind {
            crate::fip::SeqDrop::Seq => {
                let zero = self.zero();
                self.def_var(slot.var, zero);
            }
            crate::fip::SeqDrop::Fields => {
                // The move copied the struct out; its handles now belong to
                // the copy. A local read before any binding has no storage.
                let ptr = self.use_var(slot.var);
                let bound = self.b.ins().icmp_imm(IntCC::NotEqual, ptr, 0);
                let clear = self.b.create_block();
                let done = self.b.create_block();
                self.branch(bound, clear, &[], done, &[]);
                self.b.switch_to_block(clear);
                for &offset in &slot.fields {
                    let zero = self.zero();
                    self.seq_set_word(ptr, offset, zero);
                }
                self.jump(done, &[]);
                self.b.switch_to_block(done);
            }
            crate::fip::SeqDrop::Str => {}
        }
    }

    /// Binding a drop slot frees what it held. A string is copied into the
    /// slot's own storage, which the binding then names; returns the value
    /// to bind.
    pub(super) fn seq_drop_rebind(
        &mut self,
        pattern: &Pattern,
        value: ir::Value,
    ) -> Result<ir::Value, String> {
        let PatternKind::Bind(name) = &pattern.kind else {
            return Ok(value);
        };
        let Some(slot) = self.seq_drop_slot(name) else {
            return Ok(value);
        };
        self.seq_drop_free(&slot)?;
        Ok(match slot.storage {
            Some(storage) if slot.kind == crate::fip::SeqDrop::Fields => {
                let ty = self
                    .local_types
                    .get(name)
                    .cloned()
                    .ok_or("drop slot lacks type")?;
                let size = Layout::of(self.world, &ty)?.size;
                self.copy_bytes(value, storage, size);
                storage
            }
            Some(storage) => {
                self.copy_bytes(value, storage, crate::seq::HEADER_BYTES as u32);
                storage
            }
            None => value,
        })
    }

    /// Assigning a whole drop slot frees what it held.
    pub(super) fn seq_drop_overwrite(&mut self, place: &str) -> Result<(), String> {
        if let Some(slot) = self.seq_drop_slot(place) {
            self.seq_drop_free(&slot)?;
        }
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

    /// The element at `addr` for a store that copies it at once (an
    /// `Option` payload): an aggregate element is copied straight from the
    /// buffer on the optimize path, with no temporary.
    fn seq_elem_for_store(&mut self, addr: ir::Value, elem: &Type) -> Result<ir::Value, String> {
        if self.operands_only && Layout::of(self.world, elem)?.aggregate() {
            return Ok(addr);
        }
        self.seq_load_elem(addr, elem)
    }

    fn seq_load_elem(&mut self, addr: ir::Value, elem: &Type) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, elem)?;
        if layout.aggregate() {
            // The copy writes every word.
            let copy = if self.operands_only {
                self.alloc_uninit(layout.size)
            } else {
                self.alloc(layout.size)
            };
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
    pub(super) fn seq_guard(&mut self, valid: ir::Value) -> Result<(), String> {
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
    pub(super) fn seq_reserve(
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
        if let SeqOp::Str(op) = op {
            return self.str_op(op, args);
        }
        if let Some(value) = self.hoisted_seq_op(op, args, arg_types)? {
            return Ok(value);
        }
        if let Some(value) = self.byte_seq_op(op, args, arg_types)? {
            return Ok(value);
        }
        if op == SeqOp::SubSlice && arg_types.first().is_some_and(crate::strings::is_text) {
            return self.str_op(crate::strings::StrOp::SubStr, args);
        }
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
                    let value = this.seq_elem_for_store(addr, &elem)?;
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
                        this.seq_elem_for_store(addr, &elem)
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
                self.seq_move(dst, src, bytes);
                self.seq_set_word(h, LEN, needed);
                self.zero()
            }
            SeqOp::Finish => {
                let h = arg(0)?;
                let len = self.seq_word(h, LEN);
                let empty = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                self.seq_guard(empty)?;
                // `finish` consumes the drained sequence.
                self.seq_runtime_call("vesper_seq_free", &[h])?;
                self.zero()
            }
            SeqOp::AsSlice => {
                let (h, start, len) = self.seq_view(arg(0)?, &arg_types[0]);
                self.seq_slice_value(h, start, len)
            }
            SeqOp::Str(_)
            | SeqOp::Hoisted(_)
            | SeqOp::BoundsGuard { .. }
            | SeqOp::CopyFrom { .. }
            | SeqOp::Fill
            | SeqOp::Wide(_)
            | SeqOp::SumBe16
            | SeqOp::BytesEq
            | SeqOp::FindByte
            | SeqOp::FindEither
            | SeqOp::FindNotIn
            | SeqOp::EqIgnoreAsciiCase
            | SeqOp::ExtendAsciiLowercase => {
                unreachable!("string, hoisted and byte operations return above")
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
    pub(super) fn seq_option(
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
        // The optimize path zeroes the payload only on the `None` edge;
        // `Some` writes every word.
        let lazy = self.operands_only;
        let out = if lazy {
            self.alloc_uninit(layout.size)
        } else {
            self.alloc(layout.size)
        };
        let none = self.b.create_block();
        let some = self.b.create_block();
        let done = self.b.create_block();
        self.branch(present, some, &[], none, &[]);
        self.b.switch_to_block(none);
        let tag = self.b.ins().iconst(types::I64, none_tag as i64);
        self.seq_set_word(out, 0, tag);
        if lazy {
            for off in (8..layout.size).step_by(8) {
                let zero = self.zero();
                self.seq_set_word(out, off as i32, zero);
            }
        }
        self.jump(done, &[]);
        self.b.switch_to_block(some);
        let value = load(self)?;
        let tag = self.b.ins().iconst(types::I64, some_tag as i64);
        self.seq_set_word(out, 0, tag);
        if lazy {
            // An `f32` payload store writes only the low half of its word.
            let end = if elem.name() == "f32" {
                payload as u32
            } else {
                payload as u32 + Layout::of(self.world, elem)?.size
            };
            for off in (8..layout.size).step_by(8) {
                if off < payload as u32 || off >= end {
                    let zero = self.zero();
                    self.seq_set_word(out, off as i32, zero);
                }
            }
        }
        let slot = self.offset(out, payload);
        self.store(slot, value, elem)?;
        self.jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(out)
    }

    /// Copies `bytes` bytes from `src` to `dst` (distinct buffers).
    pub(super) fn seq_copy(&mut self, dst: ir::Value, src: ir::Value, bytes: ir::Value) {
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
            for (i, o) in operands.into_iter().enumerate() {
                // A string update writes through its receiver's place.
                let updates_text = i == 0 && op.mutates() && crate::strings::is_text(&self.ty(o));
                values.push(if updates_text {
                    self.address(o)?
                } else {
                    self.expr(o)?
                });
                types.push(self.ty(o));
            }
            let result = self.ty(e);
            self.seq_op(op, &values, &types, &result)
        })())
    }
}

/// A `try_get` whose `Option` result lives in two SSA variables, whether the
/// index is in bounds and the element address, instead of in memory: the
/// `match` that consumes it branches on the bounds check and its `Some` arm
/// loads the element directly.
#[derive(Clone)]
pub(super) struct FusedRead {
    valid: Variable,
    addr: Variable,
    elem: Type,
}

fn fused_pattern(p: &Pattern) -> bool {
    match &p.kind {
        PatternKind::Wild => true,
        PatternKind::Constructor { name, fields, .. } => match name.as_str() {
            "Option::Some" | "Some" => {
                fields.len() == 1
                    && matches!(fields[0].1.kind, PatternKind::Bind(_) | PatternKind::Wild)
            }
            "Option::None" | "None" => fields.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// The `try_get` results of graph `f` that only its own block's `match` and
/// that match's arm bindings consume (`Some(x)`, `Some(_)`, `None`, `_`).
pub(super) fn fused_reads(
    f: &crate::ir::FunctionIr,
) -> BTreeMap<crate::ir::ValueId, Option<FusedRead>> {
    use crate::ir::{Op, Terminator};
    let mut out = BTreeMap::new();
    for block in &f.blocks {
        let Terminator::Match { value, arms } = &block.terminator else {
            continue;
        };
        let Some(read) = block.instructions.iter().find(|i| i.dest == *value) else {
            continue;
        };
        if !matches!(
            read.op,
            Op::Seq {
                op: SeqOp::TryGet,
                ..
            }
        ) || !arms.iter().all(|(p, _)| fused_pattern(p))
        {
            continue;
        }
        let only_arm_binds = f.blocks.iter().all(|b| {
            let terminator_ok = match &b.terminator {
                Terminator::Return(v) | Terminator::Branch { condition: v, .. } => v != value,
                Terminator::Jump { args, .. } => !args.contains(value),
                Terminator::Match { value: v, .. } => v != value || std::ptr::eq(b, block),
                Terminator::Unreachable => true,
            };
            terminator_ok
                && !b.params.contains(value)
                && b.instructions.iter().all(|i| {
                    !crate::fip::op_reads_value(&i.op, *value)
                        || matches!(&i.op, Op::Bind { pattern, value: v } if v == value && fused_pattern(pattern))
                })
        });
        if only_arm_binds {
            out.insert(*value, None);
        }
    }
    out
}

impl<M: Module> Codegen<'_, '_, M> {
    fn seq_fused(&self, value: crate::ir::ValueId) -> Option<&Option<FusedRead>> {
        self.exits.last().and_then(|t| t.fused.get(&value))
    }

    /// Whether `try_get` result `value` of the graph being emitted is fused.
    pub(super) fn seq_is_fused(&self, value: crate::ir::ValueId) -> bool {
        self.seq_fused(value).is_some()
    }

    /// Lowers fused `try_get` `dest` on `(receiver, index)`: a bounds check
    /// and an element address, no `Option` value.
    pub(super) fn seq_fused_try_get(
        &mut self,
        dest: crate::ir::ValueId,
        [receiver, i]: [ir::Value; 2],
        receiver_type: &Type,
    ) -> Result<ir::Value, String> {
        let elem = seq::elem(receiver_type)
            .cloned()
            .ok_or("native try_get lacks its element type")?;
        let elem = crate::numeric::default_type(&elem);
        let stride = self.seq_stride(&elem)?;
        let (h, start, len) = self.seq_view(receiver, receiver_type);
        let valid = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        let valid = self.b.ins().uextend(types::I64, valid);
        let addr = self.seq_elem_addr(h, start, i, stride);
        let valid_var = self.b.declare_var(types::I64);
        self.b.def_var(valid_var, valid);
        let addr_var = self.b.declare_var(types::I64);
        self.b.def_var(addr_var, addr);
        if let Some(t) = self.exits.last_mut() {
            t.fused.insert(
                dest,
                Some(FusedRead {
                    valid: valid_var,
                    addr: addr_var,
                    elem,
                }),
            );
        }
        Ok(self.zero())
    }

    /// The condition of match arm `p` on fused read `value`.
    pub(super) fn seq_fused_condition(
        &mut self,
        value: crate::ir::ValueId,
        p: &Pattern,
    ) -> Option<ir::Value> {
        let read = self.seq_fused(value)?.clone()?;
        let valid = self.b.use_var(read.valid);
        Some(match &p.kind {
            PatternKind::Constructor { name, .. } if name.ends_with("Some") => {
                self.b.ins().icmp_imm(IntCC::NotEqual, valid, 0)
            }
            PatternKind::Constructor { .. } => self.b.ins().icmp_imm(IntCC::Equal, valid, 0),
            _ => self.b.ins().iconst(types::I8, 1),
        })
    }

    /// Binds arm pattern `pattern` on fused read `value`: `Some(x)` loads the
    /// element into `x`. Returns whether `value` is fused.
    pub(super) fn seq_fused_bind(
        &mut self,
        dest: crate::ir::ValueId,
        pattern: &Pattern,
        value: crate::ir::ValueId,
    ) -> Result<bool, String> {
        let Some(read) = self.seq_fused(value).cloned() else {
            return Ok(false);
        };
        if let (Some(read), PatternKind::Constructor { name, fields, .. }) = (read, &pattern.kind)
            && name.ends_with("Some")
            && let Some((
                _,
                inner @ Pattern {
                    kind: PatternKind::Bind(_),
                    ..
                },
            )) = fields.first()
        {
            let addr = self.b.use_var(read.addr);
            let element = self.seq_load_elem(addr, &read.elem)?;
            let mut slot_index = 0;
            self.bind_ir(dest, &mut slot_index, false, inner, element, &read.elem)?;
        }
        Ok(true)
    }
}
