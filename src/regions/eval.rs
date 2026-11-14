//! Evaluator support for region blocks. A child module of `runtime.rs`: the
//! region of a block is opened on its first operation, keyed by the block's
//! runtime capability identity, and closed when the block's handler frame
//! returns. Payloads are evaluator values held beside a byte allocation in a
//! [`vesper_alloc::Region`] bounded by the block's capacity; the allocation
//! has the payload's native layout size, so `used()` and the bound agree with
//! the native backends (`vesper.region.accounting/1`, see
//! [`crate::regions::store`]).
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
            self.regions.open(id, capacity).map_err(|e| e.to_string())?;
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
                let size = self.region_payload_size(&payload)?;
                handle(
                    BOX_TYPE,
                    id,
                    self.regions.alloc(id, size, Some(payload)).map_err(fault)?,
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
                self.regions
                    .take(id, index)
                    .map_err(fault)?
                    .ok_or("region payload has no value")?
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

    /// Native layout size of a payload: from the call's checked result type
    /// `RegionBox[r, T]` when `T` is concrete, otherwise from the value
    /// (inside generic functions, whose instantiation the evaluator does not
    /// track). A generic argument the value does not determine counts as one
    /// word, so such a payload may be charged less than natively.
    fn region_payload_size(&self, payload: &Value) -> Result<usize, String> {
        let from_call = match self.stack.last() {
            Some(Frame::IrResume { ty, .. }) => crate::regions::box_parts(ty)
                .and_then(|(_, t)| crate::layout::Layout::of(self.world, t).ok()),
            _ => None,
        };
        let layout = match from_call {
            Some(l) => l,
            None => {
                let ty = value_type(self.world, payload)
                    .ok_or("the size of this region payload is not known")?;
                crate::layout::Layout::of(self.world, &ty)?
            }
        };
        Ok(layout.size as usize)
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

/// The type of a value, for its layout. Type arguments of generic data are
/// recovered from the fields the value holds; an argument the value does not
/// determine (the `T` of `Option::None`) is taken as one machine word.
fn value_type(world: &crate::check::World, v: &Value) -> Option<Type> {
    Some(match v {
        Value::Int(_) => Type::named("i64"),
        Value::UInt(_, 8) => Type::named("u8"),
        Value::UInt(_, 32) => Type::named("u32"),
        Value::UInt(..) => Type::named("u64"),
        Value::Float(_) => Type::named("f64"),
        Value::Bool(_) => Type::named("bool"),
        Value::String(_) => Type::named("String"),
        Value::Tuple(vs) if vs.is_empty() => Type::unit(),
        Value::Tuple(vs) => Type::Tuple(
            vs.iter()
                .map(|v| value_type(world, v))
                .collect::<Option<_>>()?,
        ),
        Value::Data {
            ty,
            constructor,
            fields,
            ..
        } => {
            let adt = world.adts.get(ty)?;
            let mut bound = BTreeMap::new();
            let short = constructor.rsplit("::").next().unwrap_or(constructor);
            if let Some(variant) = adt.variants.iter().find(|v| v.name == short) {
                for field in &variant.fields {
                    if let Some(value) = fields.get(&field.name)
                        && let Some(actual) = value_type(world, value)
                    {
                        bind_generics(&field.ty, &actual, &adt.generics, &mut bound);
                    }
                }
            }
            Type::Named(
                ty.clone(),
                adt.generics
                    .iter()
                    .map(|g| {
                        bound
                            .get(&g.name)
                            .cloned()
                            .unwrap_or_else(|| Type::named("u64"))
                    })
                    .collect(),
            )
        }
        // Functions, closures and capabilities are one machine word.
        _ => Type::named("u64"),
    })
}

/// Binds the generic parameters of `declared` that `actual` determines.
fn bind_generics(
    declared: &Type,
    actual: &Type,
    generics: &[Generic],
    bound: &mut BTreeMap<String, Type>,
) {
    match (declared, actual) {
        (Type::Named(n, args), _) if args.is_empty() && generics.iter().any(|g| g.name == *n) => {
            bound.entry(n.clone()).or_insert_with(|| actual.clone());
        }
        (Type::Named(n, a), Type::Named(m, b)) if n == m && a.len() == b.len() => {
            for (d, x) in a.iter().zip(b) {
                bind_generics(d, x, generics, bound);
            }
        }
        (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
            for (d, x) in a.iter().zip(b) {
                bind_generics(d, x, generics, bound);
            }
        }
        _ => {}
    }
}
