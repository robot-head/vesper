//! Native (Cranelift JIT and object) lowering of second-class trait objects
//! ([`crate::dyn_trait`], manuscript §7.1): the `{data, vtable}` pair a
//! coercion builds on the caller's stack, the vtable data objects, the
//! indirect slot call, and the drop of an unconsumed `sink dyn` parameter.
use super::*;
use crate::dyn_trait::{BOXED_HEADER, VTABLE_HEADER, VTable};
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, condcodes::IntCC, types};
use cranelift_module::{DataDescription, FuncOrDataId, Linkage, Module};

impl<M: Module> Codegen<'_, '_, M> {
    /// The object a `dyn` parameter of type `object` receives for `arg` of
    /// the concrete type `actual`, passed `pass`: a 16-byte stack pair of
    /// the argument's address and its vtable. Nothing is allocated on the
    /// heap. A scalar `inout` local is stored to memory for the call and
    /// reloaded after it (`restores`), as for any `inout` scalar.
    pub(super) fn dyn_coerce(
        &mut self,
        arg: &Arg,
        pass: Pass,
        actual: &Type,
        object: &Type,
        restores: &mut Vec<(Variable, ir::Value, Type)>,
    ) -> Result<ir::Value, String> {
        let actual = crate::numeric::default_type(actual);
        let vtable = self
            .world
            .dyn_objects
            .vtable(object, &actual)
            .cloned()
            .ok_or_else(|| format!("no vtable of {actual} as `{object}`"))?;
        let layout = Layout::of(self.world, &actual)?;
        let data = if pass == Pass::Inout {
            let ptr = self.address(&arg.expr)?;
            if !layout.aggregate()
                && let ExprKind::Name(n) = &arg.expr.kind
            {
                let var = *self.vars.get(n).ok_or("unknown mutable scalar")?;
                let value = self.use_var(var);
                self.b.ins().store(MemFlags::new(), value, ptr, 0);
                restores.push((var, ptr, actual.clone()));
            }
            ptr
        } else if layout.aggregate() {
            if pass == Pass::In && crate::check::Place::from_expr(&arg.expr).is_some() {
                self.address(&arg.expr)?
            } else {
                self.expr(&arg.expr)?
            }
        } else {
            let value = self.expr(&arg.expr)?;
            let slot = self.alloc_uninit(8);
            self.b.ins().store(MemFlags::new(), value, slot, 0);
            slot
        };
        let table = self.dyn_vtable(&vtable)?;
        let pair = self.alloc_uninit(16);
        self.b.ins().store(MemFlags::new(), data, pair, 0);
        self.b.ins().store(MemFlags::new(), table, pair, 8);
        Ok(pair)
    }

    /// A call through slot `slot`: replaces the receiver's object pair
    /// (`values[0]`) by its data pointer and returns the slot's function.
    pub(super) fn dyn_dispatch(
        &mut self,
        values: &mut [ir::Value],
        slot: usize,
    ) -> Result<ir::Value, String> {
        let pair = *values.first().ok_or("a vtable call lacks its receiver")?;
        let data = self.b.ins().load(types::I64, MemFlags::trusted(), pair, 0);
        let table = self.b.ins().load(types::I64, MemFlags::trusted(), pair, 8);
        values[0] = data;
        let offset = VTABLE_HEADER + 8 * i32::try_from(slot).map_err(|e| e.to_string())?;
        Ok(self.b.ins().load(
            types::I64,
            MemFlags::trusted().with_readonly(),
            table,
            offset,
        ))
    }

    /// A call through slot `slot` on a boxed receiver (§7.3): replaces the
    /// receiver (the box's block, whatever the method's pass) by the
    /// value's address and returns the slot's function and the block.
    pub(super) fn dyn_dispatch_boxed(
        &mut self,
        values: &mut [ir::Value],
        slot: usize,
    ) -> Result<(ir::Value, ir::Value), String> {
        let block = *values.first().ok_or("a vtable call lacks its receiver")?;
        let table = self.b.ins().load(types::I64, MemFlags::trusted(), block, 0);
        values[0] = self.b.ins().iadd_imm(block, BOXED_HEADER);
        let offset = VTABLE_HEADER + 8 * i32::try_from(slot).map_err(|e| e.to_string())?;
        let target = self.b.ins().load(
            types::I64,
            MemFlags::trusted().with_readonly(),
            table,
            offset,
        );
        Ok((target, block))
    }

    /// The object pair a boxed object `arg` lends to a `dyn` parameter
    /// (§7.3): `{value address, vtable}` on the caller's stack, and the
    /// box's block. Nothing is allocated.
    pub(super) fn dyn_lend_boxed(
        &mut self,
        arg: &Arg,
        _pass: Pass,
    ) -> Result<(ir::Value, ir::Value), String> {
        let block = self.expr(&arg.expr)?;
        let table = self.b.ins().load(types::I64, MemFlags::trusted(), block, 0);
        let data = self.b.ins().iadd_imm(block, BOXED_HEADER);
        let pair = self.alloc_uninit(16);
        self.b.ins().store(MemFlags::new(), data, pair, 0);
        self.b.ins().store(MemFlags::new(), table, pair, 8);
        Ok((pair, block))
    }

    /// `Box::new(value)` typed `Box[dyn Trait]` (§7.3): one heap block
    /// holding the vtable address and then the value of type `actual`.
    pub(super) fn dyn_box_new(
        &mut self,
        value: ir::Value,
        actual: &Type,
        object: &Type,
    ) -> Result<ir::Value, String> {
        let actual = crate::numeric::default_type(actual);
        let vtable = self
            .world
            .dyn_objects
            .vtable(object, &actual)
            .cloned()
            .ok_or_else(|| format!("no vtable of {actual} as `{object}`"))?;
        let layout = Layout::of(self.world, &actual)?;
        let block = self.fip_box_alloc(layout.size + BOXED_HEADER as u32)?;
        let table = self.dyn_vtable(&vtable)?;
        self.b.ins().store(MemFlags::new(), table, block, 0);
        let data = self.b.ins().iadd_imm(block, BOXED_HEADER);
        self.store(data, value, &actual)?;
        Ok(block)
    }

    /// The address of `vtable`'s data object, emitted once per compilation
    /// (layout in [`crate::dyn_trait`]).
    fn dyn_vtable(&mut self, vtable: &VTable) -> Result<ir::Value, String> {
        let symbol = format!(
            "__vesper_vtable_{:016x}",
            fnv1a64(&format!("{:?}|{:?}", vtable.object, vtable.self_ty))
        );
        let id = match self.m.get_name(&symbol) {
            Some(FuncOrDataId::Data(id)) => id,
            Some(_) => return Err(format!("{symbol} collides with a function symbol")),
            None => {
                let layout = Layout::of(self.world, &vtable.self_ty)?;
                let words = 4 + vtable.methods.len();
                let mut bytes = vec![0u8; words * 8];
                bytes[16..24].copy_from_slice(&u64::from(layout.size).to_le_bytes());
                bytes[24..32].copy_from_slice(&8u64.to_le_bytes());
                let mut description = DataDescription::new();
                description.define(bytes.into_boxed_slice());
                description.set_align(8);
                if crate::fip::elem_needs_drop(self.world, &vtable.self_ty) {
                    let drop = dyn_drop_helper(self.m, self.world, &vtable.self_ty)?;
                    let reference = self.m.declare_func_in_data(drop, &mut description);
                    description.write_function_addr(0, reference);
                }
                for (slot, (name, vars)) in vtable.methods.iter().enumerate() {
                    let entry = self.dyn_slot_entry(vtable, name, vars)?;
                    let reference = self.m.declare_func_in_data(entry, &mut description);
                    description.write_function_addr(32 + 8 * slot as u32, reference);
                }
                let id = self
                    .m
                    .declare_data(&symbol, Linkage::Local, false, false)
                    .map_err(|e| e.to_string())?;
                self.m
                    .define_data(id, &description)
                    .map_err(|e| e.to_string())?;
                id
            }
        };
        let global = self.m.declare_data_in_func(id, self.b.func);
        let pointer = self.m.target_config().pointer_type();
        Ok(self.b.ins().symbol_value(pointer, global))
    }

    /// The function a vtable slot holds: the slot function (`name` at
    /// `vars`) itself when it takes `self` by address, or an adapter that
    /// loads a by-value scalar `self` first.
    fn dyn_slot_entry(
        &mut self,
        vtable: &VTable,
        name: &str,
        vars: &BTreeMap<String, Type>,
    ) -> Result<FuncId, String> {
        let key = instance_key(name, vars);
        let id = *self
            .ids
            .get(&key)
            .ok_or_else(|| format!("missing vtable slot function '{key}'"))?;
        let f = concrete(self.world, name, vars)?;
        let receiver = f.params.first().ok_or("a vtable slot lacks `self`")?;
        let by_value =
            !(Layout::of(self.world, &receiver.ty)?.aggregate() || receiver.pass == Pass::Inout);
        let abi = self.abi.of(&key);
        let mut expected = f.clone();
        expected.params[0].ty = crate::dyn_trait::object(
            crate::dyn_trait::object_trait(&vtable.object)
                .cloned()
                .unwrap_or(Type::Infer),
        );
        let slot_abi = self.abi.slot(self.world, &expected)?;
        if abi != slot_abi {
            return Err(format!(
                "vtable slot '{key}' has another calling convention than its trait method"
            ));
        }
        if !by_value {
            return Ok(id);
        }
        let symbol = format!("__vesper_dyn_adapt_{:016x}", fnv1a64(&key));
        if let Some(FuncOrDataId::Func(adapter)) = self.m.get_name(&symbol) {
            return Ok(adapter);
        }
        let target = signature_with_abi(&*self.m, self.world, &f, false, abi)?;
        let mut sig = target.clone();
        let receiver_index = usize::from(
            Layout::of(self.world, &f.result)?.aggregate() && abi.result_words.is_none(),
        );
        let loaded = sig.params[receiver_index].value_type;
        sig.params[receiver_index] = AbiParam::new(self.m.target_config().pointer_type());
        let jit = std::any::type_name::<M>().contains("JITModule");
        let linkage = if jit {
            Linkage::Preemptible
        } else {
            Linkage::Local
        };
        let adapter = self
            .m
            .declare_function(&symbol, linkage, &sig)
            .map_err(|e| e.to_string())?;
        let mut ctx = self.m.make_context();
        ctx.func.signature = sig;
        let mut fb_ctx = FunctionBuilderContext::new();
        {
            let mut h = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
            let entry = h.create_block();
            h.append_block_params_for_function_params(entry);
            h.switch_to_block(entry);
            let mut args = h.block_params(entry).to_vec();
            args[receiver_index] =
                h.ins()
                    .load(loaded, MemFlags::trusted(), args[receiver_index], 0);
            let callee = self.m.declare_func_in_func(id, h.func);
            let call = h.ins().call(callee, &args);
            let results = h.inst_results(call).to_vec();
            h.ins().return_(&results);
            h.seal_all_blocks();
            h.finalize();
        }
        if jit {
            DefinitionLinkage::Jit.normalize_references(&mut ctx.func);
        }
        self.m
            .define_function(adapter, &mut ctx)
            .map_err(|e| e.to_string())?;
        self.m.clear_context(&mut ctx);
        Ok(adapter)
    }
}

/// `__vesper_dyn_drop_*(data)`: drops the `ty` stored at `data` (word 0 of
/// its vtables).
fn dyn_drop_helper<M: Module>(m: &mut M, world: &World, ty: &Type) -> Result<FuncId, String> {
    let symbol = format!("__vesper_dyn_drop_{:016x}", fnv1a64(&format!("{ty:?}")));
    if let Some(FuncOrDataId::Func(id)) = m.get_name(&symbol) {
        return Ok(id);
    }
    let mut sig = m.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    let jit = std::any::type_name::<M>().contains("JITModule");
    let linkage = if jit {
        Linkage::Preemptible
    } else {
        Linkage::Local
    };
    let id = m
        .declare_function(&symbol, linkage, &sig)
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
        let value = helper_load(&mut h, world, data, ty)?;
        // `elements`: the Box drop elaboration cannot see through the erased
        // parameter, so the vtable drop frees the boxes too.
        seq_native::emit_drop_glue(&mut h, m, world, value, ty, true)?;
        h.ins().return_(&[]);
        h.seal_all_blocks();
        h.finalize();
    }
    if jit {
        DefinitionLinkage::Jit.normalize_references(&mut ctx.func);
    }
    m.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
    m.clear_context(&mut ctx);
    Ok(id)
}

/// Drops the object whose pair is at `pair` through its vtable's drop word.
/// A moved-from object (its pair zeroed) and a type that owns nothing (drop
/// word 0) drop nothing.
pub(super) fn emit_drop_object<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    pair: ir::Value,
) -> Result<(), String> {
    let table = b.ins().load(types::I64, MemFlags::trusted(), pair, 8);
    let has_table = b.ins().icmp_imm(IntCC::NotEqual, table, 0);
    let load_drop = b.create_block();
    let call = b.create_block();
    let done = b.create_block();
    b.ins().brif(has_table, load_drop, &[], done, &[]);
    b.switch_to_block(load_drop);
    let drop = b
        .ins()
        .load(types::I64, MemFlags::trusted().with_readonly(), table, 0);
    let has_drop = b.ins().icmp_imm(IntCC::NotEqual, drop, 0);
    b.ins().brif(has_drop, call, &[], done, &[]);
    b.switch_to_block(call);
    let data = b.ins().load(types::I64, MemFlags::trusted(), pair, 0);
    let mut sig = m.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    let sig = b.import_signature(sig);
    b.ins().call_indirect(sig, drop, &[data]);
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
    Ok(())
}

/// Drops the boxed object (§7.3) whose block is `block`: the value through
/// its vtable's drop word, then the block. A null block (moved from) drops
/// nothing.
pub(super) fn emit_drop_boxed<M: Module>(
    b: &mut FunctionBuilder<'_>,
    m: &mut M,
    block: ir::Value,
) -> Result<(), String> {
    let live = b.ins().icmp_imm(IntCC::NotEqual, block, 0);
    let load_drop = b.create_block();
    let call = b.create_block();
    let free = b.create_block();
    let done = b.create_block();
    b.ins().brif(live, load_drop, &[], done, &[]);
    b.switch_to_block(load_drop);
    let table = b.ins().load(types::I64, MemFlags::trusted(), block, 0);
    let drop = b
        .ins()
        .load(types::I64, MemFlags::trusted().with_readonly(), table, 0);
    let has_drop = b.ins().icmp_imm(IntCC::NotEqual, drop, 0);
    b.ins().brif(has_drop, call, &[], free, &[]);
    b.switch_to_block(call);
    let data = b.ins().iadd_imm(block, BOXED_HEADER);
    let mut sig = m.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    let sig = b.import_signature(sig);
    b.ins().call_indirect(sig, drop, &[data]);
    b.ins().jump(free, &[]);
    b.switch_to_block(free);
    emit_fip_box_free(b, m, block)?;
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
    Ok(())
}
