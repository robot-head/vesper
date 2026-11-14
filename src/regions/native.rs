//! Native lowering of region blocks through runtime calls. A child module of
//! `native.rs`, so it emits code with the backend's own builder.
//!
//! On entry a block calls `vesper_region_open(capacity)`, which opens a fresh
//! [`vesper_alloc::Region`] and returns its run-time id; the id is kept in a
//! variable of the block's handler, every operation passes it to
//! `vesper_region_<op>(id, ...)`, and the normal exit calls
//! `vesper_region_close(id)`. Operations therefore always reach the exact
//! activation that lexically encloses them, also after an abort out of an
//! inner block. A region an abort leaves open is closed by
//! [`crate::regions::native_runtime::finish`], which the JIT driver calls
//! after the entry returns. Payloads are single machine words; `f64`
//! payloads travel as their bit patterns.
use super::*;
use crate::regions::{self, REGION_EFFECT};

impl<M: Module> Codegen<'_, '_, M> {
    fn region_call(&mut self, name: &str, args: &[ir::Value]) -> Result<ir::Value, String> {
        if !std::any::type_name::<M>().contains("JITModule") {
            return Err(
                "native object output of source regions needs a region runtime library; run regions with the evaluator or the JIT"
                    .into(),
            );
        }
        let mut signature = self.m.make_signature();
        signature
            .params
            .extend(args.iter().map(|_| AbiParam::new(types::I64)));
        signature.returns.push(AbiParam::new(types::I64));
        let id = self
            .m
            .declare_function(name, Linkage::Import, &signature)
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
        let word = |this: &mut Self, ty: &Type, value: ir::Value| -> Result<ir::Value, String> {
            if Layout::of(this.world, ty)?.aggregate() {
                return Err(format!(
                    "native region payloads must be machine words; {ty} is an aggregate (run it with the evaluator)"
                ));
            }
            Ok(if this.b.func.dfg.value_type(value) == types::F64 {
                this.b.ins().bitcast(types::I64, ir::MemFlags::new(), value)
            } else {
                value
            })
        };
        let id = self.b.use_var(region);
        let mut values = vec![id];
        let (name, result) = match op {
            "box" => {
                let ty = self.ty(&args[0].expr);
                let v = self.expr(&args[0].expr)?;
                values.push(word(self, &ty, v)?);
                ("vesper_region_box", None)
            }
            "get" | "take" => {
                let ty = payload_of(self, &args[0].expr)?;
                Layout::of(self.world, &ty)?
                    .aggregate()
                    .then(|| Err::<(), _>(format!("native region payloads must be machine words; {ty} is an aggregate (run it with the evaluator)")))
                    .transpose()?;
                values.push(self.expr(&args[0].expr)?);
                (
                    if op == "get" {
                        "vesper_region_get"
                    } else {
                        "vesper_region_take"
                    },
                    Some(ty),
                )
            }
            "set" => {
                let ty = payload_of(self, &args[0].expr)?;
                values.push(self.expr(&args[0].expr)?);
                let v = self.expr(&args[1].expr)?;
                values.push(word(self, &ty, v)?);
                ("vesper_region_set", None)
            }
            "checkpoint" => ("vesper_region_checkpoint", None),
            "rewind" | "commit" => {
                values.push(self.expr(&args[0].expr)?);
                (
                    if op == "rewind" {
                        "vesper_region_rewind"
                    } else {
                        "vesper_region_commit"
                    },
                    None,
                )
            }
            "used" => ("vesper_region_used", None),
            _ => return Err(format!("unknown region operation '{op}'")),
        };
        let out = self.region_call(name, &values)?;
        Ok(match result {
            Some(ty) if scalar(&ty, &[])? == types::F64 => {
                self.b.ins().bitcast(types::F64, ir::MemFlags::new(), out)
            }
            _ => out,
        })
    }
}
