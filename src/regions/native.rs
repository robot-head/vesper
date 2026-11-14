//! Native lowering of region blocks through runtime calls. A child module of
//! `native.rs`, so it emits code with the backend's own builder.
//!
//! On entry a block calls `vesper_region_open(capacity)`, which opens a fresh
//! region and returns its run-time id; the id is kept in a variable of the
//! block's handler, every operation passes it to `vesper_region_<op>(id,
//! ...)`, and the normal exit calls `vesper_region_close(id)`. Operations
//! therefore always reach the exact activation that lexically encloses them,
//! also after an abort out of an inner block.
//!
//! A payload is its native layout (`crate::layout`): `box` allocates
//! [`crate::regions::store::slot_bytes`] of it and stores it word by word,
//! `get` copies it out word by word (into a fresh stack slot for an
//! aggregate), `set` overwrites it and `take` copies it out and releases it.
//! Scalars are one word; `f64` travels as its bit pattern.
//!
//! The JIT resolves the calls to [`crate::regions::native_runtime`], whose
//! driver closes regions an abort left open after the entry returns. Object
//! output (`vesper build`, `vesper emit object`) defines the same symbols in
//! the object itself ([`aot_runtime`]), so executables need only the C
//! library.
use super::*;
use crate::regions::{self, REGION_EFFECT};

/// The generated region runtime of object output.
#[path = "aot_runtime.rs"]
pub(crate) mod aot_runtime;

impl<M: Module> Codegen<'_, '_, M> {
    fn region_call(&mut self, name: &str, args: &[ir::Value]) -> Result<ir::Value, String> {
        let jit = std::any::type_name::<M>().contains("JITModule");
        if !jit {
            if self.m.isa().triple().operating_system == target_lexicon::OperatingSystem::None_ {
                return Err(
                    "source regions in a freestanding image need a region runtime; the generated one uses the C library".into(),
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

    /// Entry of a region block (`None` for other handlers).
    pub(super) fn region_enter(
        &mut self,
        effect: &Effect,
        ret: &Clause,
    ) -> Result<Option<Variable>, String> {
        let Some(capacity) = regions::block_capacity(effect, ret) else {
            return Ok(None);
        };
        let c = self.b.ins().iconst(
            types::I64,
            i64::try_from(capacity).map_err(|e| e.to_string())?,
        );
        let id = self.region_call("vesper_region_open", &[c])?;
        let var = self.b.declare_var(types::I64);
        self.b.def_var(var, id);
        Ok(Some(var))
    }

    /// Normal exit of a region block.
    pub(super) fn region_exit(&mut self, region: Option<Variable>) -> Result<(), String> {
        if let Some(var) = region
            && self.b.current_block().is_some()
        {
            let id = self.b.use_var(var);
            self.region_call("vesper_region_close", &[id])?;
        }
        Ok(())
    }

    /// `label.op(args)` on a region capability.
    pub(super) fn region_perform(
        &mut self,
        label: Option<&str>,
        op: &str,
        args: &[Arg],
    ) -> Result<ir::Value, String> {
        let label = label.ok_or("native region operations need their capability label")?;
        let identity = self
            .capabilities
            .get(&format!("$label:{label}"))
            .ok_or("native region operation lacks scoped evidence")?
            .clone();
        let region = self
            .handlers
            .iter()
            .rev()
            .find(|h| h.identity == identity && h.effect.name == REGION_EFFECT)
            .and_then(|h| h.region)
            .ok_or("native region operation does not resolve to an enclosing region block")?;
        let payload_of = |this: &Self, e: &Expr| -> Result<Type, String> {
            let t = this.ty(e);
            regions::box_parts(&t)
                .map(|(_, p)| p.clone())
                .ok_or_else(|| format!("native region operation '{op}' expects a RegionBox"))
        };
        let id = self.b.use_var(region);
        match op {
            "box" => {
                let ty = self.ty(&args[0].expr);
                let layout = Layout::of(self.world, &ty)?;
                let value = self.expr(&args[0].expr)?;
                let size = self.b.ins().iconst(types::I64, i64::from(layout.size));
                let handle = self.region_call("vesper_region_alloc", &[id, size])?;
                self.region_store_payload(id, handle, &layout, value)?;
                Ok(handle)
            }
            "get" | "take" => {
                let ty = payload_of(self, &args[0].expr)?;
                let handle = self.expr(&args[0].expr)?;
                let value = self.region_load_payload(id, handle, &ty)?;
                if op == "take" {
                    self.region_call("vesper_region_release", &[id, handle])?;
                }
                Ok(value)
            }
            "set" => {
                let ty = payload_of(self, &args[0].expr)?;
                let layout = Layout::of(self.world, &ty)?;
                let handle = self.expr(&args[0].expr)?;
                let value = self.expr(&args[1].expr)?;
                self.region_store_payload(id, handle, &layout, value)?;
                Ok(self.zero())
            }
            "checkpoint" => self.region_call("vesper_region_checkpoint", &[id]),
            "rewind" | "commit" => {
                let mark = self.expr(&args[0].expr)?;
                let name = if op == "rewind" {
                    "vesper_region_rewind"
                } else {
                    "vesper_region_commit"
                };
                self.region_call(name, &[id, mark])
            }
            "used" => self.region_call("vesper_region_used", &[id]),
            _ => Err(format!("unknown region operation '{op}'")),
        }
    }

    /// Writes `value` (a scalar, or the address of an aggregate of
    /// `layout`) into the payload `handle` word by word.
    fn region_store_payload(
        &mut self,
        id: ir::Value,
        handle: ir::Value,
        layout: &Layout,
        value: ir::Value,
    ) -> Result<(), String> {
        let words: Vec<ir::Value> = if layout.aggregate() {
            (0..layout.size.div_ceil(8))
                .map(|i| {
                    self.b
                        .ins()
                        .load(types::I64, ir::MemFlags::new(), value, (i * 8) as i32)
                })
                .collect()
        } else {
            let ty = self.b.func.dfg.value_type(value);
            vec![if ty == types::F64 {
                self.b.ins().bitcast(types::I64, ir::MemFlags::new(), value)
            } else if ty.bits() < 64 {
                self.b.ins().uextend(types::I64, value)
            } else {
                value
            }]
        };
        for (i, word) in words.into_iter().enumerate() {
            let index = self.b.ins().iconst(types::I64, i as i64);
            self.region_call("vesper_region_store", &[id, handle, index, word])?;
        }
        Ok(())
    }

    /// Reads the payload `handle` of type `ty`: a scalar, or the address of
    /// a fresh copy of an aggregate.
    fn region_load_payload(
        &mut self,
        id: ir::Value,
        handle: ir::Value,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, ty)?;
        if layout.aggregate() {
            let ptr = self.alloc(layout.size);
            for i in 0..layout.size.div_ceil(8) {
                let index = self.b.ins().iconst(types::I64, i64::from(i));
                let word = self.region_call("vesper_region_load", &[id, handle, index])?;
                self.b
                    .ins()
                    .store(ir::MemFlags::new(), word, ptr, (i * 8) as i32);
            }
            return Ok(ptr);
        }
        let index = self.zero();
        let word = self.region_call("vesper_region_load", &[id, handle, index])?;
        Ok(match scalar(ty, &[])? {
            types::F64 => self.b.ins().bitcast(types::F64, ir::MemFlags::new(), word),
            t if t.bits() < 64 => self.b.ins().ireduce(t, word),
            _ => word,
        })
    }
}
