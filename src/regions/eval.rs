//! Evaluator support for region blocks. A child module of `runtime.rs`: the
//! region of a block is opened on its first operation, keyed by the block's
//! runtime capability identity, and closed when the block's handler frame
//! returns. Payloads are evaluator values stored in a
//! [`vesper_alloc::Region`] bounded by the block's capacity, counted in
//! one-word payload slots like the native backend.
use super::*;
use crate::regions::{BOX_TYPE, WATERMARK_TYPE};

fn handle(ty: &str, region: u64, index: u64) -> Value {
    Value::Data {
        ty: ty.into(),
        constructor: ty.into(),
        fields: BTreeMap::from([
            ("region".into(), Value::UInt(region, 64)),
            ("index".into(), Value::UInt(index, 64)),
        ]),
        resource_identity: None,
    }
}
fn handle_index(value: &Value, ty: &str, region: u64) -> Result<u64, String> {
    match value {
        Value::Data { ty: t, fields, .. } if t == ty => {
            match (fields.get("region"), fields.get("index")) {
                (Some(Value::UInt(r, _)), Some(Value::UInt(i, _))) if *r == region => Ok(*i),
                (Some(Value::UInt(_, _)), Some(Value::UInt(_, _))) => {
                    Err("region handle belongs to another region".into())
                }
                _ => Err("malformed region handle".into()),
            }
        }
        _ => Err(format!("region operation expects a {ty}")),
    }
}

impl Runtime<'_> {
    /// Performs `r.op(args)` on the region block whose capability is `target`.
    pub(super) fn region_perform(
        &mut self,
        op: &str,
        mut args: Vec<Value>,
        target: Option<u64>,
    ) -> Result<Control, String> {
        let id = target.ok_or("region capability is not active")?;
        if !self.regions.is_open(id) {
            let capacity = self
                .stack
                .iter()
                .rev()
                .find_map(|f| match f {
                    Frame::Handler(h) if h.id == id => {
                        crate::regions::block_capacity(&h.effect, &h.ret)
                    }
                    _ => None,
                })
                .ok_or("region capability is no longer active")?;
            // Each payload is stored as one evaluator value. The bound is
            // scaled so that a block holds as many word-sized payloads as
            // the native backend's one-word slots.
            let scaled = capacity.saturating_mul(std::mem::size_of::<Value>().div_ceil(8));
            self.regions.open(id, scaled).map_err(|e| e.to_string())?;
        }
        let fault = |e: crate::regions::store::RegionFault| e.to_string();
        let mut arg = |i: usize| {
            args.get_mut(i)
                .map(|v| std::mem::replace(v, Value::Tuple(vec![])))
                .ok_or_else(|| format!("region operation '{op}' lacks an argument"))
        };
        let value = match op {
            "box" => {
                let payload = arg(0)?;
                handle(
                    BOX_TYPE,
                    id,
                    self.regions.alloc(id, payload, false).map_err(fault)?,
                )
            }
            "get" => {
                let index = handle_index(&arg(0)?, BOX_TYPE, id)?;
                self.regions.get(id, index).map_err(fault)?.clone()
            }
            "set" => {
                let index = handle_index(&arg(0)?, BOX_TYPE, id)?;
                let payload = arg(1)?;
                self.regions.set(id, index, payload).map_err(fault)?;
                Value::Tuple(vec![])
            }
            "take" => {
                let index = handle_index(&arg(0)?, BOX_TYPE, id)?;
                self.regions.take(id, index).map_err(fault)?
            }
            "checkpoint" => handle(
                WATERMARK_TYPE,
                id,
                self.regions.checkpoint(id).map_err(fault)?,
            ),
            "rewind" => {
                let index = handle_index(&arg(0)?, WATERMARK_TYPE, id)?;
                self.regions.rewind(id, index).map_err(fault)?;
                Value::Tuple(vec![])
            }
            "commit" => {
                let index = handle_index(&arg(0)?, WATERMARK_TYPE, id)?;
                self.regions.commit(id, index).map_err(fault)?;
                Value::Tuple(vec![])
            }
            "used" => Value::UInt(self.regions.used(id).map_err(fault)?, 64),
            _ => return Err(format!("unknown region operation '{op}'")),
        };
        Ok(Control::Return(value))
    }

    /// Closes the region of a block whose handler frame returned.
    pub(super) fn region_end(&mut self, h: &Handler) -> Result<(), String> {
        if crate::regions::block_capacity(&h.effect, &h.ret).is_some() {
            self.regions.close(h.id).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// `(opened, closed, open now)` region counters of this runtime.
    pub fn region_counts(&self) -> (u64, u64, usize) {
        self.regions.counts()
    }
}
