//! Cranelift JIT/AOT lowering for `ChunkSeq[T]` and `Slice[T]` (§13).
//!
//! A sequence is a handle to a `{len, cap, data}` header; elements live at
//! `data + i * stride` with the packed stride of [`seq::elem_bytes`]. A slice
//! is a 24-byte value `{handle, start, len}`. Only allocation and growth call
//! the runtime (`vesper_seq_make`, `vesper_seq_grow`); every other operation,
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
    /// The slot's type: what a sequence's elements or an aggregate own.
    ty: Type,
    /// A `let`-bound string's or struct's own storage: the slot is bound to
    /// it so that a later binding can free the value it replaces.
    storage: Option<ir::Value>,
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
    ) -> Result<ir::Value, String> {
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
        let call = self.b.ins().call(reference, args);
        Ok(self.b.inst_results(call)[0])
    }

    /// The address of the shared empty header (#1021): what `new` and
    /// `with_capacity(0)` return. It is `{0, 0, null}`, freeing it is a
    /// no-op and growing it allocates a fresh header, which generated code
    /// writes back to the receiver's place ([`Self::seq_reserve`]).
    fn seq_empty_header(&mut self) -> Result<ir::Value, String> {
        if std::any::type_name::<M>().contains("JITModule") {
            let address = crate::seq::native_runtime::empty_header();
            return Ok(self.b.ins().iconst(types::I64, address as i64));
        }
        if self.m.isa().triple().operating_system == target_lexicon::OperatingSystem::None_ {
            return Err(
                "ChunkSeq in a freestanding image needs an allocator; the generated one uses the C library".into(),
            );
        }
        aot_runtime::define(self.m)?;
        let Some(cranelift_module::FuncOrDataId::Data(id)) =
            self.m.get_name(aot_runtime::EMPTY_HEADER)
        else {
            return Err("the sequence runtime lacks its empty header".into());
        };
        let data = self.m.declare_data_in_func(id, self.b.func);
        Ok(self.b.ins().symbol_value(types::I64, data))
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
            // `only` names the slots a nested graph owns, its own `sink`
            // parameters included when it is an inlined function.
            if only.is_some_and(|names| !names.contains(&name)) {
                continue;
            }
            let Some(&var) = self.vars.get(&name) else {
                continue;
            };
            if kind == crate::fip::SeqDrop::Fields {
                let mut fields = vec![];
                let ty =
                    crate::numeric::default_type(&substitute(&f.locals[&name], &self.generic_vars));
                let layout = Layout::of(self.world, &ty)?;
                if !seq_field_offsets(self.world, &layout, 0, &mut fields) || fields.is_empty() {
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
            let ty = f.locals.get(&name).cloned().unwrap_or(Type::Infer);
            slots.push(SeqDropSlot {
                var,
                kind,
                ty,
                storage,
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

    /// Frees what drop `slot` holds now: a sequence with the elements still
    /// in it, a string's buffer, or what an owned aggregate holds.
    fn seq_drop_free(&mut self, slot: &SeqDropSlot) -> Result<(), String> {
        let value = match (slot.kind, slot.storage) {
            (crate::fip::SeqDrop::Str, Some(storage)) => storage,
            _ => self.use_var(slot.var),
        };
        // An empty slot (never bound, or moved from) holds 0.
        let present = self.b.ins().icmp_imm(IntCC::NotEqual, value, 0);
        let free = self.b.create_block();
        let done = self.b.create_block();
        self.branch(present, free, &[], done, &[]);
        self.b.switch_to_block(free);
        let ty = match slot.kind {
            crate::fip::SeqDrop::Str => Type::named(crate::strings::STRING_TYPE),
            _ => slot.ty.clone(),
        };
        self.drop_value(value, &ty)?;
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

    /// A sequence or owned aggregate moved out of local `name` leaves its
    /// slot empty.
    pub(super) fn seq_drop_moved(&mut self, name: &str) {
        let Some(slot) = self.seq_drop_slot(name) else {
            return;
        };
        match slot.kind {
            crate::fip::SeqDrop::Seq => {
                let zero = self.zero();
                self.def_var(slot.var, zero);
            }
            crate::fip::SeqDrop::Owned | crate::fip::SeqDrop::Fields => {
                // The move copied the value out; what it owns now belongs
                // to the copy, so the source is zeroed (an empty sequence,
                // a borrowed-nothing string, no handles). A local read
                // before any binding has no storage.
                let ptr = self.use_var(slot.var);
                let bound = self.b.ins().icmp_imm(IntCC::NotEqual, ptr, 0);
                let clear = self.b.create_block();
                let done = self.b.create_block();
                self.branch(bound, clear, &[], done, &[]);
                self.b.switch_to_block(clear);
                let ty = crate::numeric::default_type(&substitute(&slot.ty, &self.generic_vars));
                let size = Layout::of(self.world, &ty).map_or(0, |l| l.size);
                for offset in (0..size).step_by(8) {
                    let zero = self.zero();
                    self.seq_set_word(ptr, offset as i32, zero);
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
            // A destructuring binding frees what its names held; their new
            // values are stored by [`Self::seq_drop_bound`].
            let mut names = vec![];
            pattern_bind_names(pattern, &mut names);
            for name in names {
                if let Some(slot) = self.seq_drop_slot(&name) {
                    self.seq_drop_free(&slot)?;
                }
            }
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

    /// After a destructuring binding: each string or struct it bound into a
    /// drop slot moves into the slot's own storage, as a plain binding's
    /// does.
    pub(super) fn seq_drop_bound(&mut self, pattern: &Pattern) -> Result<(), String> {
        if matches!(pattern.kind, PatternKind::Bind(_)) {
            return Ok(());
        }
        let mut names = vec![];
        pattern_bind_names(pattern, &mut names);
        for name in names {
            if let Some(slot) = self.seq_drop_slot(&name)
                && let Some(storage) = slot.storage
            {
                let size = match slot.kind {
                    crate::fip::SeqDrop::Str => crate::seq::HEADER_BYTES as u32,
                    _ => {
                        let ty =
                            crate::numeric::default_type(&substitute(&slot.ty, &self.generic_vars));
                        Layout::of(self.world, &ty)?.size
                    }
                };
                let value = self.use_var(slot.var);
                self.copy_bytes(value, storage, size);
                self.def_var(slot.var, storage);
            }
        }
        Ok(())
    }

    /// Assigning a whole drop slot frees what it held.
    pub(super) fn seq_drop_overwrite(&mut self, place: &str) -> Result<(), String> {
        if let Some(slot) = self.seq_drop_slot(place) {
            self.seq_drop_free(&slot)?;
        }
        Ok(())
    }

    /// Drops the element at `addr` that a `set` overwrites.
    fn seq_drop_elem(&mut self, addr: ir::Value, elem: &Type) -> Result<(), String> {
        if !self.drops_enabled() || !crate::fip::elem_needs_drop(self.world, elem) {
            return Ok(());
        }
        let value = helper_load(&mut self.b, self.world, addr, elem)?;
        emit_drop_glue(&mut self.b, self.m, self.world, value, elem, true)
    }

    /// Drops the `count` elements of `h` from index `from` that `clear` or
    /// `truncate` removes.
    fn seq_drop_range(
        &mut self,
        h: ir::Value,
        from: ir::Value,
        count: ir::Value,
        stride: i64,
        elem: &Type,
    ) -> Result<(), String> {
        if !self.drops_enabled() || !crate::fip::elem_needs_drop(self.world, elem) {
            return Ok(());
        }
        let zero = self.zero();
        let data = self.seq_elem_addr(h, zero, from, stride);
        emit_drop_elements(&mut self.b, self.m, self.world, data, count, elem)
    }

    fn seq_word(&mut self, ptr: ir::Value, offset: i32) -> ir::Value {
        if let Some(word) = self
            .exits
            .last()
            .and_then(|t| t.header_words.get(&(ptr, offset)))
        {
            return *word;
        }
        if self.invariant_headers.contains(&ptr) {
            return self.invariant_word(ptr, offset);
        }
        self.b.ins().load(types::I64, MemFlags::new(), ptr, offset)
    }

    /// Reads the receiver at `place` and its header words once, before a
    /// loop that cannot change them ([`crate::bounds_hoist::header_hoists`]):
    /// the loop's accesses on the returned receiver reuse the words.
    pub(super) fn seq_hoist_header(
        &mut self,
        place: &crate::check::Place,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        let e = ir_place_expr(place, 0..0);
        let mut words = vec![];
        let (receiver, handle) = if seq::is_slice_type(ty) {
            let view = self.address(&e)?;
            for offset in [0, 8, 16] {
                words.push(((view, offset), self.seq_word(view, offset)));
            }
            (view, words[0].1)
        } else {
            let handle = self.expr(&e)?;
            (handle, handle)
        };
        for offset in [LEN, DATA] {
            words.push(((handle, offset), self.seq_word(handle, offset)));
        }
        if let Some(t) = self.exits.last_mut() {
            t.header_words.extend(words);
        }
        Ok(receiver)
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
        if self.invariant_headers.contains(&handle) {
            // `data + start * stride` is invariant for the whole call
            // (`crate::bounds_hoist::stable_views`), so Cranelift computes
            // it once, out of every loop.
            let first = self.b.ins().imul_imm(start, stride);
            let base = self.b.ins().iadd(data, first);
            let offset = self.b.ins().imul_imm(i, stride);
            return self.b.ins().iadd(base, offset);
        }
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

    /// Grows `handle` to hold `needed` elements when its capacity is short,
    /// and returns the handle to use afterwards. Growing the shared empty
    /// header makes a new one (#1021), which is written back to `receiver`,
    /// the sequence's place; a string (`receiver` is `None`) grows in place.
    pub(super) fn seq_reserve(
        &mut self,
        handle: ir::Value,
        needed: ir::Value,
        stride: i64,
        receiver: Option<&Expr>,
    ) -> Result<ir::Value, String> {
        let cap = self.seq_word(handle, CAP);
        let fits = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, needed, cap);
        let grow = self.b.create_block();
        let done = self.b.create_block();
        let Some(receiver) = receiver else {
            self.branch(fits, done, &[], grow, &[]);
            self.b.switch_to_block(grow);
            let stride = self.b.ins().iconst(types::I64, stride);
            self.seq_runtime_call("vesper_seq_grow", &[handle, stride, needed])?;
            self.jump(done, &[]);
            self.b.switch_to_block(done);
            return Ok(handle);
        };
        self.b.append_block_param(done, types::I64);
        self.branch(fits, done, &[handle.into()], grow, &[]);
        self.b.switch_to_block(grow);
        let stride = self.b.ins().iconst(types::I64, stride);
        let grown = self.seq_runtime_call("vesper_seq_grow", &[handle, stride, needed])?;
        self.seq_write_back(receiver, grown)?;
        self.jump(done, &[grown.into()]);
        self.b.switch_to_block(done);
        Ok(self.b.block_params(done)[0])
    }

    /// [`Self::seq_reserve`] followed by `tail`, which writes through the
    /// handle. `tail` is emitted on the fast path with `handle` and on the
    /// growth path with the grown handle, so the fast path carries no block
    /// parameter for a handle that did not change.
    pub(super) fn seq_reserve_then(
        &mut self,
        handle: ir::Value,
        needed: ir::Value,
        stride: i64,
        receiver: Option<&Expr>,
        mut tail: impl FnMut(&mut Self, ir::Value) -> Result<(), String>,
    ) -> Result<(), String> {
        let cap = self.seq_word(handle, CAP);
        let fits = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, needed, cap);
        let fast = self.b.create_block();
        let grow = self.b.create_block();
        let done = self.b.create_block();
        self.branch(fits, fast, &[], grow, &[]);
        self.b.switch_to_block(fast);
        tail(self, handle)?;
        self.jump(done, &[]);
        self.b.switch_to_block(grow);
        self.b.set_cold_block(grow);
        let stride = self.b.ins().iconst(types::I64, stride);
        let grown = self.seq_runtime_call("vesper_seq_grow", &[handle, stride, needed])?;
        let grown = match receiver {
            Some(place) => {
                self.seq_write_back(place, grown)?;
                grown
            }
            None => handle,
        };
        tail(self, grown)?;
        self.jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(())
    }

    /// Stores `len` as the length of `h` when `changed` holds. A length
    /// that stays the same is not stored, so the shared empty header, which
    /// threads use at once, is never written (#1021).
    pub(super) fn seq_set_len_if(&mut self, h: ir::Value, changed: ir::Value, len: ir::Value) {
        let store = self.b.create_block();
        let done = self.b.create_block();
        self.branch(changed, store, &[], done, &[]);
        self.b.switch_to_block(store);
        self.seq_set_word(h, LEN, len);
        self.jump(done, &[]);
        self.b.switch_to_block(done);
    }

    /// Stores a grown sequence handle into its place: a local (register or
    /// `inout` pointer), or a field path through memory.
    fn seq_write_back(&mut self, place: &Expr, handle: ir::Value) -> Result<(), String> {
        let name = format!("$seq_grown{}", handle.as_u32());
        let var = self.b.declare_var(types::I64);
        self.def_var(var, handle);
        self.vars.insert(name.clone(), var);
        let ty = self.ty(place);
        self.local_types.insert(name.clone(), ty);
        let value = Expr {
            kind: ExprKind::Name(name.clone()),
            span: 0..0,
        };
        self.expr(&Expr {
            kind: ExprKind::Assign("=".into(), Box::new(place.clone()), Box::new(value)),
            span: place.span.clone(),
        })?;
        self.vars.remove(&name);
        self.local_types.remove(&name);
        Ok(())
    }

    /// `(handle, start, len)` of a sequence or slice operand.
    fn seq_view(&mut self, value: ir::Value, ty: &Type) -> (ir::Value, ir::Value, ir::Value) {
        if seq::is_seq_type(ty) {
            let len = self.seq_word(value, LEN);
            (value, self.zero(), len)
        } else {
            let h = self.seq_word(value, 0);
            if self.invariant_headers.contains(&value) {
                self.invariant_headers.insert(h);
            }
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
        receiver: Option<&Expr>,
    ) -> Result<ir::Value, String> {
        if let SeqOp::Str(op) = op {
            return self.str_op(op, args);
        }
        if let Some(value) = self.hoisted_seq_op(op, args, arg_types)? {
            return Ok(value);
        }
        // A growing operation on a sequence writes a new handle back to its
        // place (#1021); a string grows in place.
        let receiver =
            receiver.filter(|_| op.mutates() && arg_types.first().is_some_and(seq::is_seq_type));
        if let Some(value) = self.byte_seq_op(op, args, arg_types, receiver)? {
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
        if let SeqOp::Vector(io) = op {
            let (h, start, len) = self.seq_view(arg(0)?, &arg_types[0]);
            let zero = self.zero();
            let base = self.seq_elem_addr(h, start, zero, stride);
            let value = if io.store { Some(arg(2)?) } else { None };
            return self.vector_slice_io(io, base, len, arg(1)?, value, stride);
        }
        Ok(match op {
            SeqOp::New => {
                // An empty sequence shares one static header and allocates
                // on its first growth, as Rust's `Vec::new` (#1021).
                self.seq_empty_header()?
            }
            SeqOp::WithCapacity => {
                let cap = arg(0)?;
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
                    .declare_function("vesper_seq_make", linkage, &signature)
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
                let value = arg(1)?;
                self.seq_reserve_then(h, next, stride, receiver, |this, h| {
                    let zero = this.zero();
                    let addr = this.seq_elem_addr(h, zero, len, stride);
                    this.seq_store_elem(addr, value, &elem)?;
                    this.seq_set_word(h, LEN, next);
                    Ok(())
                })?;
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
                    self.seq_drop_elem(addr, &elem)?;
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
                    self.seq_drop_elem(addr, &elem)?;
                    self.seq_store_elem(addr, arg(2)?, &elem)?;
                    self.jump(done, &[]);
                    self.b.switch_to_block(no);
                    // The element moved in but not stored is dropped.
                    if self.drops_enabled() && crate::fip::elem_needs_drop(self.world, &elem) {
                        emit_drop_glue(&mut self.b, self.m, self.world, arg(2)?, &elem, true)?;
                    }
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
                let h = arg(0)?;
                let zero = self.zero();
                let len = self.seq_word(h, LEN);
                self.seq_drop_range(h, zero, len, stride, &elem)?;
                let some = self.b.ins().icmp_imm(IntCC::NotEqual, len, 0);
                self.seq_set_len_if(h, some, zero);
                zero
            }
            SeqOp::Truncate => {
                let h = arg(0)?;
                let n = arg(1)?;
                let len = self.seq_word(h, LEN);
                let shorter = self.b.ins().icmp(IntCC::UnsignedLessThan, n, len);
                let next = self.b.ins().select(shorter, n, len);
                let removed = self.b.ins().isub(len, next);
                self.seq_drop_range(h, next, removed, stride, &elem)?;
                self.seq_set_len_if(h, shorter, next);
                self.zero()
            }
            SeqOp::Reserve => {
                let h = arg(0)?;
                let len = self.seq_word(h, LEN);
                // A request past the address space saturates; growth then
                // faults as an out-of-memory sequence does.
                let (needed, over) = self.b.ins().uadd_overflow(len, arg(1)?);
                let max = self.b.ins().iconst(types::I64, -1);
                let needed = self.b.ins().select(over, max, needed);
                self.seq_reserve_then(h, needed, stride, receiver, |_, _| Ok(()))?;
                self.zero()
            }
            SeqOp::Extend => {
                let h = arg(0)?;
                let (sh, sstart, slen) = self.seq_view(arg(1)?, &arg_types[1]);
                let len = self.seq_word(h, LEN);
                let needed = self.b.ins().iadd(len, slen);
                self.seq_reserve_then(h, needed, stride, receiver, |this, h| {
                    let zero = this.zero();
                    let dst = this.seq_elem_addr(h, zero, len, stride);
                    let src = this.seq_elem_addr(sh, sstart, zero, stride);
                    let bytes = this.b.ins().imul_imm(slen, stride);
                    this.seq_move(dst, src, bytes);
                    let some = this.b.ins().icmp_imm(IntCC::NotEqual, slen, 0);
                    this.seq_set_len_if(h, some, needed);
                    Ok(())
                })?;
                self.zero()
            }
            SeqOp::Resize => {
                let h = arg(0)?;
                let (n, value) = (arg(1)?, arg(2)?);
                let len = self.seq_word(h, LEN);
                // Copy elements: shortening drops nothing.
                let longer = self.b.ins().icmp(IntCC::UnsignedGreaterThan, n, len);
                let grow = self.b.create_block();
                let shrink = self.b.create_block();
                let done = self.b.create_block();
                self.branch(longer, grow, &[], shrink, &[]);
                self.b.switch_to_block(shrink);
                let shorter = self.b.ins().icmp(IntCC::UnsignedLessThan, n, len);
                self.seq_set_len_if(h, shorter, n);
                self.jump(done, &[]);
                self.b.switch_to_block(grow);
                self.seq_reserve_then(h, n, stride, receiver, |this, h| {
                    let added = this.b.ins().isub(n, len);
                    this.seq_fill_elems(h, len, added, value, &elem, stride)?;
                    this.seq_set_word(h, LEN, n);
                    Ok(())
                })?;
                self.jump(done, &[]);
                self.b.switch_to_block(done);
                self.zero()
            }
            SeqOp::ExtendFromWithin { total } => {
                let h = arg(0)?;
                let (lo, hi) = (arg(1)?, arg(2)?);
                let len = self.seq_word(h, LEN);
                let ordered = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, lo, hi);
                let inside = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, hi, len);
                let valid = self.b.ins().band(ordered, inside);
                self.seq_when(valid, total, |this| {
                    let n = this.b.ins().isub(hi, lo);
                    let needed = this.b.ins().iadd(len, n);
                    // The source is read through the grown handle: growth
                    // moves the buffer. It ends at `hi <= len`, below the
                    // destination, so the copy never overlaps.
                    this.seq_reserve_then(h, needed, stride, receiver, |this, h| {
                        let zero = this.zero();
                        let dst = this.seq_elem_addr(h, zero, len, stride);
                        let src = this.seq_elem_addr(h, zero, lo, stride);
                        let bytes = this.b.ins().imul_imm(n, stride);
                        this.seq_move(dst, src, bytes);
                        let some = this.b.ins().icmp_imm(IntCC::NotEqual, n, 0);
                        this.seq_set_len_if(h, some, needed);
                        Ok(())
                    })
                })?
            }
            SeqOp::ExtendReversed => {
                let h = arg(0)?;
                let (sh, sstart, slen) = self.seq_view(arg(1)?, &arg_types[1]);
                let len = self.seq_word(h, LEN);
                let needed = self.b.ins().iadd(len, slen);
                self.seq_reserve_then(h, needed, stride, receiver, |this, h| {
                    let zero = this.zero();
                    let dst = this.seq_elem_addr(h, zero, len, stride);
                    let src = this.seq_elem_addr(sh, sstart, zero, stride);
                    this.seq_copy_reversed(dst, src, slen, stride, &elem)?;
                    let some = this.b.ins().icmp_imm(IntCC::NotEqual, slen, 0);
                    this.seq_set_len_if(h, some, needed);
                    Ok(())
                })?;
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
            | SeqOp::CopyWithin { .. }
            | SeqOp::Fill
            | SeqOp::FillRange { .. }
            | SeqOp::Wide(_)
            | SeqOp::SumBe16
            | SeqOp::BytesEq
            | SeqOp::BytesCmp
            | SeqOp::FindByte
            | SeqOp::FindEither
            | SeqOp::FindNotIn
            | SeqOp::EqIgnoreAsciiCase
            | SeqOp::ExtendAsciiLowercase
            | SeqOp::MakeAsciiLowercase { .. }
            | SeqOp::Vector(_) => {
                unreachable!("string, hoisted, byte and vector operations return above")
            }
            SeqOp::Swap | SeqOp::Replace => {
                return Err(format!(
                    "native {} is lowered from its places (seq_place_primitive)",
                    op.name()
                ));
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

    /// Copies the `n` elements at `src` to `dst` (distinct buffers) last
    /// first: element `k` of `dst` is element `n - 1 - k` of `src`. Bytes
    /// move eight at a time through a byte swap, the rest one element at a
    /// time.
    fn seq_copy_reversed(
        &mut self,
        dst: ir::Value,
        src: ir::Value,
        n: ir::Value,
        stride: i64,
        elem: &Type,
    ) -> Result<(), String> {
        let zero = self.zero();
        let mut from = zero;
        if stride == 1 {
            // Word `k` of `dst` (bytes `8k..8k + 8`) is the byte-swapped
            // word of `src` that ends at `n - 8k`.
            let head = self.b.create_block();
            let body = self.b.create_block();
            let tail = self.b.create_block();
            self.b.append_block_param(head, types::I64);
            self.b.append_block_param(tail, types::I64);
            self.jump(head, &[zero.into()]);
            self.b.switch_to_block(head);
            let k = self.b.block_params(head)[0];
            let left = self.b.ins().isub(n, k);
            let word = self
                .b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, left, 8);
            self.branch(word, body, &[], tail, &[k.into()]);
            self.b.switch_to_block(body);
            let at = self.b.ins().iadd(src, left);
            let w = self.b.ins().load(types::I64, MemFlags::new(), at, -8);
            let w = self.b.ins().bswap(w);
            let to = self.b.ins().iadd(dst, k);
            self.b.ins().store(MemFlags::new(), w, to, 0);
            let next = self.b.ins().iadd_imm(k, 8);
            self.jump(head, &[next.into()]);
            self.b.switch_to_block(tail);
            from = self.b.block_params(tail)[0];
        }
        let head = self.b.create_block();
        let body = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(head, types::I64);
        self.jump(head, &[from.into()]);
        self.b.switch_to_block(head);
        let k = self.b.block_params(head)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, k, n);
        self.branch(more, body, &[], done, &[]);
        self.b.switch_to_block(body);
        let last = self.b.ins().iadd_imm(n, -1);
        let back = self.b.ins().isub(last, k);
        let from_addr = self.seq_elem_addr_raw(src, back, stride);
        let to_addr = self.seq_elem_addr_raw(dst, k, stride);
        let layout = Layout::of(self.world, elem)?;
        if layout.aggregate() {
            self.copy_bytes(from_addr, to_addr, layout.size);
        } else {
            let value = self.seq_load_elem(from_addr, elem)?;
            self.seq_store_elem(to_addr, value, elem)?;
        }
        let next = self.b.ins().iadd_imm(k, 1);
        self.jump(head, &[next.into()]);
        self.b.switch_to_block(done);
        Ok(())
    }

    /// `base + i * stride`.
    fn seq_elem_addr_raw(&mut self, base: ir::Value, i: ir::Value, stride: i64) -> ir::Value {
        let offset = self.b.ins().imul_imm(i, stride);
        self.b.ins().iadd(base, offset)
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

    /// `swap(inout a, inout b)` and `replace(inout a, v)`: the places are
    /// written through their addresses, an aggregate by a copy through a
    /// scratch slot, and a scalar local kept in a register is restored from
    /// its scratch copy afterwards, as `call_function` does.
    pub(super) fn seq_place_primitive(
        &mut self,
        op: SeqOp,
        operands: &[&Expr],
        ty: Option<Type>,
    ) -> Result<ir::Value, String> {
        let [a, b] = operands else {
            return Err(format!("native {} needs two operands", op.name()));
        };
        let ty = crate::numeric::default_type(&ty.unwrap_or_else(|| self.ty(a)));
        let layout = Layout::of(self.world, &ty)?;
        let swap = op == SeqOp::Swap;
        let value = if swap { None } else { Some(self.expr(b)?) };
        let pa = self.address(a)?;
        let pb = if swap { Some(self.address(b)?) } else { None };
        let result = if layout.aggregate() {
            let old = self.alloc_uninit(layout.size);
            self.copy_bytes(pa, old, layout.size);
            if let Some(pb) = pb {
                self.copy_bytes(pb, pa, layout.size);
                self.copy_bytes(old, pb, layout.size);
                self.zero()
            } else {
                let value = value.ok_or("native replace lacks its value")?;
                self.store(pa, value, &ty)?;
                old
            }
        } else {
            let old = self.load(pa, &ty)?;
            if let Some(pb) = pb {
                let other = self.load(pb, &ty)?;
                self.store(pa, other, &ty)?;
                self.store(pb, old, &ty)?;
                self.zero()
            } else {
                let value = value.ok_or("native replace lacks its value")?;
                self.store(pa, value, &ty)?;
                old
            }
        };
        if !layout.aggregate() {
            for (e, ptr) in [(a, Some(pa)), (b, pb)] {
                if let (ExprKind::Name(n), Some(ptr)) = (&e.kind, ptr)
                    && let Some(var) = self.vars.get(n).copied()
                {
                    let value = self.load(ptr, &ty)?;
                    self.def_var(var, value);
                }
            }
        }
        Ok(result)
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
        if op.place_primitive() {
            return Some(self.seq_place_primitive(op, &operands, None));
        }
        let receiver = operands
            .first()
            .map(|e| (*e).clone())
            .filter(|_| op.has_receiver());
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
            self.seq_op(op, &values, &types, &result, receiver.as_ref())
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
    /// The read is hoisted out of its loop's bounds checks: always `Some`.
    always: bool,
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

/// The `try_get` results of graph `f` (checked, or hoisted out of a counted
/// loop's bounds checks) that only its own block's `match` and that match's
/// arm bindings consume (`Some(x)`, `Some(_)`, `None`, `_`).
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
                op: SeqOp::TryGet | SeqOp::Hoisted(crate::seq::Access::TryGet),
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
                // The fast copy of a versioned loop (`crate::bounds_hoist`)
                // keeps the value ids of its instructions, so its own read
                // and match of the same value fuse too.
                Terminator::Match { value: v, .. } => {
                    v != value || b.instructions.iter().any(|i| i.dest == *value)
                }
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
        checked: bool,
    ) -> Result<ir::Value, String> {
        let elem = seq::elem(receiver_type)
            .cloned()
            .ok_or("native try_get lacks its element type")?;
        let elem = crate::numeric::default_type(&elem);
        let stride = self.seq_stride(&elem)?;
        let (h, start, len) = self.seq_view(receiver, receiver_type);
        let valid = if checked {
            let valid = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
            self.b.ins().uextend(types::I64, valid)
        } else {
            self.b.ins().iconst(types::I64, 1)
        };
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
                    always: !checked,
                }),
            );
        }
        Ok(self.zero())
    }

    /// Whether the arms `seen` of the `match` on fused read `value` cover
    /// both outcomes of its bounds check (`Some` and `None`, or a `_`), so
    /// the last of them needs no test of its own.
    pub(super) fn seq_fused_exhaustive(
        &self,
        value: crate::ir::ValueId,
        seen: &[&Pattern],
    ) -> bool {
        let Some(Some(read)) = self.seq_fused(value) else {
            return false;
        };
        let some = |p: &&Pattern| matches!(&p.kind, PatternKind::Constructor { name, .. } if name.ends_with("Some"));
        let none = |p: &&Pattern| matches!(&p.kind, PatternKind::Constructor { name, .. } if !name.ends_with("Some"));
        seen.last()
            .is_some_and(|p| matches!(p.kind, PatternKind::Wild))
            || (seen.iter().any(some) && seen.iter().any(none))
            // A hoisted read is `Some`: its first `Some` arm is taken.
            || (read.always && seen.last().is_some_and(some))
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
                    kind: PatternKind::Bind(bound),
                    ..
                },
            )) = fields.first()
        {
            let addr = self.b.use_var(read.addr);
            // A split local (#1090) loads its words from the element
            // itself, not from a copy of it: the words no later use reads
            // are dead loads, so `Some(h) => h.f` is one load.
            if self.split_of(bound).is_some() {
                self.split_write(bound, &[], addr)?;
                return Ok(true);
            }
            let element = self.seq_load_elem(addr, &read.elem)?;
            let mut slot_index = 0;
            self.bind_ir(dest, &mut slot_index, false, inner, element, &read.elem)?;
        }
        Ok(true)
    }
}

/// Calls a sequence runtime function (`vesper_seq_free`, `vesper_str_release`)
/// from a generated drop helper, as [`Codegen::seq_runtime_call`] does.
fn glue_runtime_call<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    name: &str,
    args: &[ir::Value],
) -> Result<(), String> {
    let jit = std::any::type_name::<M>().contains("JITModule");
    if !jit {
        aot_runtime::define(m)?;
    }
    let mut signature = m.make_signature();
    signature
        .params
        .extend(args.iter().map(|_| AbiParam::new(types::I64)));
    signature.returns.push(AbiParam::new(types::I64));
    let linkage = if jit { Linkage::Import } else { Linkage::Local };
    let id = m
        .declare_function(name, linkage, &signature)
        .map_err(|e| e.to_string())?;
    let reference = m.declare_func_in_func(id, b.func);
    b.ins().call(reference, args);
    Ok(())
}

/// Drops `value` of type `ty` (§13, port gaps lane 2): a sequence frees the
/// elements still in it, then its header and buffer; a `String` frees its
/// buffer; a tuple, struct or enum drops the fields of its live variant
/// that own something. `value` is a sequence handle (0 is empty), or the
/// address of an aggregate. `elements` is set for an element of a dropped
/// sequence, which also frees the boxes it holds (no Box drop tracks a
/// sequence buffer); elsewhere boxes are left to the Box drop elaboration.
pub(super) fn emit_drop_glue<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    world: &World,
    value: ir::Value,
    ty: &Type,
    elements: bool,
) -> Result<(), String> {
    let ty = crate::numeric::default_type(ty);
    let owns = |t: &Type| {
        if elements {
            crate::fip::elem_needs_drop(world, t)
        } else {
            crate::fip::needs_drop(world, t)
        }
    };
    if !owns(&ty) {
        return Ok(());
    }
    if crate::dyn_trait::is_object(&ty) {
        // An unconsumed `sink dyn` parameter: its vtable's drop.
        return super::dyn_native::emit_drop_object(b, m, value);
    }
    if seq::is_seq_type(&ty) {
        // An empty slot or a moved-from field holds handle 0, which the
        // executable runtime does not accept.
        let elem = seq::elem(&ty)
            .ok_or("ChunkSeq without an element type")?
            .clone();
        let present = b.ins().icmp_imm(IntCC::NotEqual, value, 0);
        let drop = b.create_block();
        let done = b.create_block();
        b.ins().brif(present, drop, &[], done, &[]);
        b.switch_to_block(drop);
        if crate::fip::elem_needs_drop(world, &elem) {
            let len = b.ins().load(types::I64, MemFlags::new(), value, LEN);
            let data = b.ins().load(types::I64, MemFlags::new(), value, DATA);
            emit_drop_elements(b, m, world, data, len, &elem)?;
        }
        glue_runtime_call(b, m, "vesper_seq_free", &[value])?;
        b.ins().jump(done, &[]);
        b.switch_to_block(done);
        return Ok(());
    }
    if crate::strings::is_string_type(&ty) {
        return glue_runtime_call(b, m, "vesper_str_release", &[value]);
    }
    if crate::fip::is_box_type(world, &ty) {
        return emit_drop_box_value(b, m, world, value, &ty);
    }
    let layout = Layout::of(world, &ty)?;
    match layout.kind {
        LayoutKind::Scalar => Ok(()),
        LayoutKind::Product(fields) => {
            for field in fields.into_iter().filter(|f| owns(&f.ty)) {
                let ptr = b.ins().iadd_imm(value, i64::from(field.offset));
                let v = helper_load(b, world, ptr, &field.ty)?;
                emit_drop_glue(b, m, world, v, &field.ty, elements)?;
            }
            Ok(())
        }
        LayoutKind::Enum(variants) => {
            let owning: Vec<(usize, Vec<LayoutField>)> = variants
                .into_iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    let fields: Vec<LayoutField> =
                        v.fields.into_iter().filter(|f| owns(&f.ty)).collect();
                    (!fields.is_empty()).then_some((i, fields))
                })
                .collect();
            let tag = b.ins().load(types::I64, MemFlags::new(), value, 0);
            let done = b.create_block();
            for (tag_index, fields) in owning {
                let this = b.create_block();
                let next = b.create_block();
                let is = b.ins().icmp_imm(IntCC::Equal, tag, tag_index as i64);
                b.ins().brif(is, this, &[], next, &[]);
                b.switch_to_block(this);
                for field in fields {
                    let ptr = b.ins().iadd_imm(value, i64::from(field.offset));
                    let v = helper_load(b, world, ptr, &field.ty)?;
                    emit_drop_glue(b, m, world, v, &field.ty, elements)?;
                }
                b.ins().jump(done, &[]);
                b.switch_to_block(next);
            }
            b.ins().jump(done, &[]);
            b.switch_to_block(done);
            Ok(())
        }
    }
}

/// Drops the `count` elements of type `elem` stored from `data` on, through
/// one generated helper per element type (`__vesper_drop_elems_*`), which
/// also serves element types that hold sequences of themselves.
pub(super) fn emit_drop_elements<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    world: &World,
    data: ir::Value,
    count: ir::Value,
    elem: &Type,
) -> Result<(), String> {
    let elem = crate::numeric::default_type(elem);
    let sym = format!("__vesper_drop_elems_{:016x}", fnv1a64(&elem.to_string()));
    let id = match m.get_name(&sym) {
        Some(cranelift_module::FuncOrDataId::Func(id)) => id,
        Some(_) => return Err(format!("{sym} collides with a data symbol")),
        None => {
            let stride = seq::elem_bytes(world, &elem)?;
            let mut sig = m.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            let jit = std::any::type_name::<M>().contains("JITModule");
            let linkage = if jit {
                Linkage::Preemptible
            } else {
                Linkage::Local
            };
            let id = m
                .declare_function(&sym, linkage, &sig)
                .map_err(|e| e.to_string())?;
            let mut ctx = m.make_context();
            ctx.func.signature = sig;
            let mut fb_ctx = FunctionBuilderContext::new();
            {
                let mut h = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
                let entry = h.create_block();
                h.append_block_params_for_function_params(entry);
                h.switch_to_block(entry);
                let data = h.block_params(entry)[0];
                let count = h.block_params(entry)[1];
                let head = h.create_block();
                h.append_block_param(head, types::I64);
                let body = h.create_block();
                let exit = h.create_block();
                let zero = h.ins().iconst(types::I64, 0);
                h.ins().jump(head, &[zero.into()]);
                h.switch_to_block(head);
                let i = h.block_params(head)[0];
                let more = h.ins().icmp(IntCC::UnsignedLessThan, i, count);
                h.ins().brif(more, body, &[], exit, &[]);
                h.switch_to_block(body);
                let offset = h.ins().imul_imm(i, stride);
                let ptr = h.ins().iadd(data, offset);
                let v = helper_load(&mut h, world, ptr, &elem)?;
                emit_drop_glue(&mut h, m, world, v, &elem, true)?;
                let next = h.ins().iadd_imm(i, 1);
                h.ins().jump(head, &[next.into()]);
                h.switch_to_block(exit);
                h.ins().return_(&[]);
                h.seal_all_blocks();
                h.finalize();
            }
            if jit {
                DefinitionLinkage::Jit.normalize_references(&mut ctx.func);
            }
            m.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
            m.clear_context(&mut ctx);
            id
        }
    };
    let reference = m.declare_func_in_func(id, b.func);
    b.ins().call(reference, &[data, count]);
    Ok(())
}

impl<M: Module> Codegen<'_, '_, M> {
    /// Whether this function frees what it drops: not in `@fip` code, and
    /// not in a freestanding image, which has no allocator to return to.
    pub(super) fn drops_enabled(&self) -> bool {
        let jit = std::any::type_name::<M>().contains("JITModule");
        !self.in_fip_fn
            && (jit
                || self.m.isa().triple().operating_system != target_lexicon::OperatingSystem::None_)
    }

    /// Drops `value` of type `ty` ([`emit_drop_glue`]).
    pub(super) fn drop_value(&mut self, value: ir::Value, ty: &Type) -> Result<(), String> {
        let ty = crate::numeric::default_type(&substitute(ty, &self.generic_vars));
        emit_drop_glue(&mut self.b, self.m, self.world, value, &ty, false)
    }

    /// Whether dropping a value of `ty` frees anything ([`crate::fip::needs_drop`]).
    pub(super) fn needs_drop(&self, ty: &Type) -> bool {
        let ty = crate::numeric::default_type(&substitute(ty, &self.generic_vars));
        crate::fip::needs_drop(self.world, &ty)
    }

    /// Drops the value of type `ty` stored at `addr`.
    pub(super) fn drop_in_place(&mut self, addr: ir::Value, ty: &Type) -> Result<(), String> {
        let ty = crate::numeric::default_type(&substitute(ty, &self.generic_vars));
        let value = helper_load(&mut self.b, self.world, addr, &ty)?;
        emit_drop_glue(&mut self.b, self.m, self.world, value, &ty, false)
    }

    /// Before a write `place = v` replaces an owned value: frees what the
    /// place holds ([`crate::fip::replace_drops`]). Whole writes of a drop
    /// slot are freed by [`Self::seq_drop_overwrite`] instead.
    pub(super) fn drop_replaced(
        &mut self,
        place: &str,
        op: &str,
        at: (usize, usize),
    ) -> Result<(), String> {
        if op != "=" || !self.drops_enabled() {
            return Ok(());
        }
        if !self.exits.last().is_some_and(|t| t.replace.contains(&at)) {
            return Ok(());
        }
        let mut parts = place.split('.');
        let root = parts.next().unwrap_or("");
        let path: Vec<String> = parts.map(str::to_owned).collect();
        if path.is_empty() && self.seq_drop_slot(root).is_some() {
            return Ok(());
        }
        let Some(mut ty) = self.local_types.get(root).cloned() else {
            return Ok(());
        };
        for field in &path {
            let layout = Layout::of(self.world, &substitute(&ty, &self.generic_vars))?;
            let LayoutKind::Product(fields) = layout.kind else {
                return Ok(());
            };
            let Some(f) = fields.into_iter().find(|f| &f.name == field) else {
                return Ok(());
            };
            ty = f.ty;
        }
        if !self.needs_drop(&ty) {
            return Ok(());
        }
        let e = ir_place_expr(
            &crate::check::Place {
                root: root.into(),
                path,
            },
            0..0,
        );
        let addr = self.address(&e)?;
        self.drop_in_place(addr, &ty)
    }

    /// Drops the owned temporaries of `f` whose last use is instruction
    /// `inst` of `block` ([`crate::fip::temporary_drops`]).
    pub(super) fn drop_temporaries(
        &mut self,
        temps: &BTreeMap<(usize, usize), Vec<crate::ir::ValueId>>,
        f: &crate::ir::FunctionIr,
        block: usize,
        inst: usize,
        values: &BTreeMap<crate::ir::ValueId, ir::Value>,
    ) -> Result<(), String> {
        let Some(ids) = temps.get(&(block, inst)) else {
            return Ok(());
        };
        for id in ids {
            let ty = f.values.get(id).cloned().unwrap_or(Type::Infer);
            if let Some(&value) = values.get(id)
                && self.needs_drop(&ty)
            {
                self.drop_value(value, &ty)?;
            }
        }
        Ok(())
    }
}

/// The names a pattern binds.
fn pattern_bind_names(p: &Pattern, out: &mut Vec<String>) {
    match &p.kind {
        PatternKind::Bind(n) => out.push(n.clone()),
        PatternKind::Tuple(ps) => ps.iter().for_each(|p| pattern_bind_names(p, out)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, p)| pattern_bind_names(p, out));
        }
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}
