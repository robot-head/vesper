//! Cranelift JIT/AOT lowering for `TiledSeq[T]` and `tiled(N) struct` (§12.5).
use super::*;
use crate::tiled::{self, TiledLayout, TiledOp};
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, types};
use cranelift_module::{Linkage, Module};

#[path = "aot_runtime.rs"]
pub(crate) mod aot_runtime;

fn little() -> MemFlags {
    MemFlags::new().with_endianness(ir::Endianness::Little)
}

impl<M: Module> Codegen<'_, '_, M> {
    fn tiled_runtime_call(&mut self, name: &str, args: &[ir::Value]) -> Result<ir::Value, String> {
        let jit = std::any::type_name::<M>().contains("JITModule");
        if !jit {
            if self.m.isa().triple().operating_system == target_lexicon::OperatingSystem::None_ {
                return Err(
                    "TiledSeq in a freestanding image needs a tiled runtime; the generated one uses the C library".into(),
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

    fn tiled_layout_of(&self, adt: &str) -> Result<TiledLayout, String> {
        let decl = self
            .world
            .adts
            .get(adt)
            .ok_or_else(|| format!("unknown tiled struct '{adt}'"))?;
        tiled::layout_of_adt(self.world, decl)
    }

    fn value_to_i64_word(&mut self, v: ir::Value) -> ir::Value {
        let ty = self.b.func.dfg.value_type(v);
        if ty == types::F64 {
            self.b.ins().bitcast(types::I64, MemFlags::new(), v)
        } else if ty.bits() < 64 {
            self.b.ins().uextend(types::I64, v)
        } else {
            v
        }
    }

    fn i64_word_to_type(&mut self, word: ir::Value, ty: &Type) -> Result<ir::Value, String> {
        Ok(match scalar(ty, &[])? {
            types::F64 => self.b.ins().bitcast(types::F64, MemFlags::new(), word),
            t if t.bits() < 64 => self.b.ins().ireduce(t, word),
            _ => word,
        })
    }

    fn store_tiled_row(
        &mut self,
        adt: &str,
        seq: ir::Value,
        row: ir::Value,
        record_ptr: ir::Value,
    ) -> Result<(), String> {
        let tlayout = self.tiled_layout_of(adt)?;
        let aos = Layout::of(self.world, &Type::named(adt))?;
        for h in &tlayout.hot_columns {
            let lf = aos
                .field(&h.name)
                .ok_or_else(|| format!("missing hot field '{}' in layout of '{adt}'", h.name))?;
            let word = self
                .b
                .ins()
                .load(types::I64, MemFlags::new(), record_ptr, lf.offset);
            let col_off = self.b.ins().iconst(types::I64, h.tile_offset as i64);
            let eb = self.b.ins().iconst(types::I64, h.elem_bytes as i64);
            self.tiled_runtime_call("vesper_tiled_hot_store", &[seq, row, col_off, eb, word])?;
        }
        for c in &tlayout.cold_fields {
            let lf = aos
                .field(&c.name)
                .ok_or_else(|| format!("missing cold field '{}' in layout of '{adt}'", c.name))?;
            if lf.layout.aggregate() {
                for w in 0..c.words {
                    let off = lf.offset + (w * 8) as i32;
                    let word = self
                        .b
                        .ins()
                        .load(types::I64, MemFlags::new(), record_ptr, off);
                    let woff = self.b.ins().iconst(types::I64, (c.word_offset + w) as i64);
                    self.tiled_runtime_call("vesper_tiled_cold_store", &[seq, row, woff, word])?;
                }
            } else {
                let word = self
                    .b
                    .ins()
                    .load(types::I64, MemFlags::new(), record_ptr, lf.offset);
                let woff = self.b.ins().iconst(types::I64, c.word_offset as i64);
                self.tiled_runtime_call("vesper_tiled_cold_store", &[seq, row, woff, word])?;
            }
        }
        Ok(())
    }

    fn load_tiled_row(
        &mut self,
        adt: &str,
        seq: ir::Value,
        row: ir::Value,
    ) -> Result<ir::Value, String> {
        let tlayout = self.tiled_layout_of(adt)?;
        let aos = Layout::of(self.world, &Type::named(adt))?;
        let out_ptr = self.alloc(aos.size);
        for h in &tlayout.hot_columns {
            let lf = aos
                .field(&h.name)
                .ok_or_else(|| format!("missing hot field '{}' in layout of '{adt}'", h.name))?;
            let col_off = self.b.ins().iconst(types::I64, h.tile_offset as i64);
            let eb = self.b.ins().iconst(types::I64, h.elem_bytes as i64);
            let word =
                self.tiled_runtime_call("vesper_tiled_hot_load", &[seq, row, col_off, eb])?;
            self.b
                .ins()
                .store(MemFlags::new(), word, out_ptr, lf.offset);
        }
        for c in &tlayout.cold_fields {
            let lf = aos
                .field(&c.name)
                .ok_or_else(|| format!("missing cold field '{}' in layout of '{adt}'", c.name))?;
            if lf.layout.aggregate() {
                for w in 0..c.words {
                    let woff = self.b.ins().iconst(types::I64, (c.word_offset + w) as i64);
                    let word =
                        self.tiled_runtime_call("vesper_tiled_cold_load", &[seq, row, woff])?;
                    let off = lf.offset + (w * 8) as i32;
                    self.b.ins().store(MemFlags::new(), word, out_ptr, off);
                }
            } else {
                let woff = self.b.ins().iconst(types::I64, c.word_offset as i64);
                let word = self.tiled_runtime_call("vesper_tiled_cold_load", &[seq, row, woff])?;
                self.b
                    .ins()
                    .store(MemFlags::new(), word, out_ptr, lf.offset);
            }
        }
        Ok(out_ptr)
    }

    fn resolve_region_var(&mut self, label: &str) -> Result<ir::Value, String> {
        let identity = self
            .capabilities
            .get(&format!("$label:{label}"))
            .ok_or_else(|| {
                format!("native TiledSeq::with_region lacks scoped capability '{label}'")
            })?
            .clone();
        let region_var = self
            .handlers
            .iter()
            .rev()
            .find(|h| h.identity == identity && h.effect.name == crate::regions::REGION_EFFECT)
            .and_then(|h| h.region)
            .ok_or("native TiledSeq::with_region must run inside an enclosing region block")?;
        Ok(self.b.use_var(region_var))
    }

    pub(super) fn tiled_op(
        &mut self,
        op: &TiledOp,
        args: &[ir::Value],
        capability: Option<&str>,
    ) -> Result<ir::Value, String> {
        match op {
            TiledOp::New { adt } => {
                let layout = self.tiled_layout_of(adt)?;
                let w = self
                    .b
                    .ins()
                    .iconst(types::I64, i64::from(layout.tile_width));
                let stride = self
                    .b
                    .ins()
                    .iconst(types::I64, layout.tile_stride_bytes as i64);
                let cold = self
                    .b
                    .ins()
                    .iconst(types::I64, layout.cold_words_per_row as i64);
                let zero = self.zero();
                self.tiled_runtime_call("vesper_tiled_new", &[w, stride, cold, zero, zero])
            }
            TiledOp::WithCapacity { adt } => {
                let layout = self.tiled_layout_of(adt)?;
                let w = self
                    .b
                    .ins()
                    .iconst(types::I64, i64::from(layout.tile_width));
                let stride = self
                    .b
                    .ins()
                    .iconst(types::I64, layout.tile_stride_bytes as i64);
                let cold = self
                    .b
                    .ins()
                    .iconst(types::I64, layout.cold_words_per_row as i64);
                let zero = self.zero();
                let cap = args[0];
                self.tiled_runtime_call("vesper_tiled_new", &[w, stride, cold, zero, cap])
            }
            TiledOp::WithRegion { adt, region_label } => {
                let layout = self.tiled_layout_of(adt)?;
                let label = capability.unwrap_or(region_label);
                let region_id = self.resolve_region_var(label)?;
                let w = self
                    .b
                    .ins()
                    .iconst(types::I64, i64::from(layout.tile_width));
                let stride = self
                    .b
                    .ins()
                    .iconst(types::I64, layout.tile_stride_bytes as i64);
                let cold = self
                    .b
                    .ins()
                    .iconst(types::I64, layout.cold_words_per_row as i64);
                let cap = args[0];
                self.tiled_runtime_call("vesper_tiled_new", &[w, stride, cold, region_id, cap])
            }
            TiledOp::Len => self.tiled_runtime_call("vesper_tiled_len", &[args[0]]),
            TiledOp::TileCount { .. } => {
                self.tiled_runtime_call("vesper_tiled_tile_count", &[args[0]])
            }
            TiledOp::Push { adt } => {
                let seq = args[0];
                let rec = args[1];
                let row = self.tiled_runtime_call("vesper_tiled_push_begin", &[seq])?;
                self.store_tiled_row(adt, seq, row, rec)?;
                Ok(self.zero())
            }
            TiledOp::Get { adt } => self.load_tiled_row(adt, args[0], args[1]),
            TiledOp::Set { adt } => {
                self.store_tiled_row(adt, args[0], args[1], args[2])?;
                Ok(self.zero())
            }
            TiledOp::ColGet { adt, field } => {
                let seq = args[0];
                let row = args[1];
                let layout = self.tiled_layout_of(adt)?;
                if let Some(h) = layout.hot_columns.iter().find(|h| &h.name == field) {
                    let col_off = self.b.ins().iconst(types::I64, h.tile_offset as i64);
                    let eb = self.b.ins().iconst(types::I64, h.elem_bytes as i64);
                    let word =
                        self.tiled_runtime_call("vesper_tiled_hot_load", &[seq, row, col_off, eb])?;
                    return self.i64_word_to_type(word, &h.ty);
                }
                let c = layout
                    .cold_fields
                    .iter()
                    .find(|c| &c.name == field)
                    .ok_or_else(|| format!("unknown field '{adt}.{field}'"))?;
                let fl = Layout::of(self.world, &c.ty)?;
                if fl.aggregate() {
                    let ptr = self.alloc(fl.size);
                    for w in 0..c.words {
                        let woff = self.b.ins().iconst(types::I64, (c.word_offset + w) as i64);
                        let word =
                            self.tiled_runtime_call("vesper_tiled_cold_load", &[seq, row, woff])?;
                        self.b
                            .ins()
                            .store(MemFlags::new(), word, ptr, (w * 8) as i32);
                    }
                    Ok(ptr)
                } else {
                    let woff = self.b.ins().iconst(types::I64, c.word_offset as i64);
                    let word =
                        self.tiled_runtime_call("vesper_tiled_cold_load", &[seq, row, woff])?;
                    self.i64_word_to_type(word, &c.ty)
                }
            }
            TiledOp::ColSet { adt, field } => {
                let seq = args[0];
                let row = args[1];
                let val = args[2];
                let layout = self.tiled_layout_of(adt)?;
                if let Some(h) = layout.hot_columns.iter().find(|h| &h.name == field) {
                    let col_off = self.b.ins().iconst(types::I64, h.tile_offset as i64);
                    let eb = self.b.ins().iconst(types::I64, h.elem_bytes as i64);
                    let word = self.value_to_i64_word(val);
                    self.tiled_runtime_call(
                        "vesper_tiled_hot_store",
                        &[seq, row, col_off, eb, word],
                    )?;
                    return Ok(self.zero());
                }
                let c = layout
                    .cold_fields
                    .iter()
                    .find(|c| &c.name == field)
                    .ok_or_else(|| format!("unknown field '{adt}.{field}'"))?;
                let fl = Layout::of(self.world, &c.ty)?;
                if fl.aggregate() {
                    for w in 0..c.words {
                        let word =
                            self.b
                                .ins()
                                .load(types::I64, MemFlags::new(), val, (w * 8) as i32);
                        let woff = self.b.ins().iconst(types::I64, (c.word_offset + w) as i64);
                        self.tiled_runtime_call(
                            "vesper_tiled_cold_store",
                            &[seq, row, woff, word],
                        )?;
                    }
                } else {
                    let woff = self.b.ins().iconst(types::I64, c.word_offset as i64);
                    let word = self.value_to_i64_word(val);
                    self.tiled_runtime_call("vesper_tiled_cold_store", &[seq, row, woff, word])?;
                }
                Ok(self.zero())
            }
            TiledOp::LoadTileVec { adt, field, shape } => {
                let seq = args[0];
                let tile_idx = args[1];
                let layout = self.tiled_layout_of(adt)?;
                let h = layout
                    .hot_columns
                    .iter()
                    .find(|h| &h.name == field)
                    .ok_or_else(|| format!("unknown hot field '{adt}.{field}'"))?;
                let col_off = self.b.ins().iconst(types::I64, h.tile_offset as i64);
                let col_ptr = self
                    .tiled_runtime_call("vesper_tiled_tile_col_ptr", &[seq, tile_idx, col_off])?;
                let result = self.alloc(shape.bytes());
                let valid = self
                    .b
                    .ins()
                    .icmp_imm(ir::condcodes::IntCC::NotEqual, col_ptr, 0);
                let ok_blk = self.b.create_block();
                let zero_blk = self.b.create_block();
                let done_blk = self.b.create_block();
                self.b.ins().brif(valid, ok_blk, &[], zero_blk, &[]);
                self.b.switch_to_block(ok_blk);
                for c in 0..shape.chunks() {
                    let chunk = self
                        .b
                        .ins()
                        .load(types::I8X16, little(), col_ptr, (c * 16) as i32);
                    self.b.ins().store(little(), chunk, result, (c * 16) as i32);
                }
                self.b.ins().jump(done_blk, &[]);
                self.b.switch_to_block(zero_blk);
                let zero_v = self.b.ins().iconst(types::I64, 0);
                for w in 0..(shape.bytes() / 8) {
                    self.b
                        .ins()
                        .store(MemFlags::new(), zero_v, result, (w * 8) as i32);
                }
                self.b.ins().jump(done_blk, &[]);
                self.b.switch_to_block(done_blk);
                Ok(result)
            }
        }
    }

    pub(super) fn tiled_ast_call(
        &mut self,
        callee: &Expr,
        types: &[Type],
        args: &[Arg],
        call_expr: &Expr,
    ) -> Option<Result<ir::Value, String>> {
        let (name, receiver) = match &callee.kind {
            ExprKind::Name(n) if !self.vars.contains_key(n) => (n.as_str(), None),
            ExprKind::Field(base, method) => {
                let base_ty = self.ty(base);
                if !tiled::is_seq_type(&base_ty) {
                    return None;
                }
                (method.as_str(), Some(base.as_ref()))
            }
            _ => return None,
        };
        let (op, capability) = tiled::resolve_ir_call(
            self.world,
            &self.world.types,
            &self.capabilities,
            name,
            receiver,
            types,
            args,
            call_expr,
        )?;
        Some((|| {
            let mut operands = Vec::new();
            if let Some(recv) = receiver {
                operands.push(self.expr(recv)?);
            }
            let skip_first = matches!(op, TiledOp::WithRegion { .. });
            for (idx, a) in args.iter().enumerate() {
                if skip_first && idx == 0 {
                    continue;
                }
                operands.push(self.expr(&a.expr)?);
            }
            self.tiled_op(&op, &operands, capability.as_deref())
        })())
    }
}
