//! CEK evaluator support for `TiledSeq[T]` and `tiled(N) struct` (§12.5).
use crate::{
    ast::Type,
    runtime::{Runtime, Value},
    tiled::{TiledOp, layout_of_adt},
    vector::Elem,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

#[derive(Clone)]
pub struct TiledSeqState {
    pub adt: String,
    pub tile_width: u32,
    pub rows: Vec<BTreeMap<String, Value>>,
    pub region_id: Option<u64>,
    pub charged_bytes: usize,
    pub row_charge_bytes: usize,
}

impl TiledSeqState {
    fn tile_count(&self) -> u64 {
        let len = self.rows.len() as u64;
        let w = u64::from(self.tile_width.max(1));
        len.div_ceil(w)
    }
}

fn as_usize(v: &Value, what: &str) -> Result<usize, String> {
    match v {
        Value::UInt(n, _) => usize::try_from(*n).map_err(|_| format!("{what} overflows usize")),
        Value::Int(n) if *n >= 0 => {
            usize::try_from(*n).map_err(|_| format!("{what} overflows usize"))
        }
        _ => Err(format!("expected non-negative integer for {what}")),
    }
}

fn lane_from_value(elem: Elem, v: Option<&Value>) -> Result<Value, String> {
    let Some(v) = v else {
        return Ok(match elem {
            Elem::F64 => Value::Float(0.0),
            Elem::I64 => Value::Int(0),
            e => Value::UInt(0, e.bits()),
        });
    };
    Ok(match (elem, v) {
        (Elem::F64, Value::Float(f)) => Value::Float(*f),
        (Elem::I64, Value::Int(n)) => Value::Int(*n),
        (Elem::I64, Value::UInt(n, _)) => Value::Int(*n as i64),
        (e, Value::UInt(n, _)) => Value::UInt(*n & e.mask(), e.bits()),
        (e, Value::Int(n)) => Value::UInt((*n as u64) & e.mask(), e.bits()),
        (Elem::U8, Value::Bool(b)) => Value::UInt(u64::from(*b), 8),
        _ => return Err("tiled hot column value does not match vector lane type".into()),
    })
}

fn row_charge_bytes(world: &crate::check::World, adt: &crate::ast::Adt) -> Result<usize, String> {
    let layout = layout_of_adt(world, adt)?;
    let hot_per_row = layout
        .tile_stride_bytes
        .div_ceil(layout.tile_width as usize);
    let cold_per_row = layout.cold_words_per_row * 8;
    Ok((hot_per_row + cold_per_row).max(8))
}

impl Runtime<'_> {
    fn open_tiled_region(
        &mut self,
        label: &str,
        env: &BTreeMap<String, crate::runtime::Slot>,
    ) -> Result<u64, String> {
        let Some(id) = crate::runtime::capability(env, &format!("$label:{label}")) else {
            return Err(format!("region capability '{label}' is not active"));
        };
        if !self.regions.is_open(id) {
            let capacity = self
                .region_capacity_for(id)
                .ok_or("region capability is no longer active")?;
            self.regions.open(id, capacity).map_err(|e| e.to_string())?;
        }
        Ok(id)
    }

    fn ensure_tiled_region_active(&self, state: &TiledSeqState) -> Result<(), String> {
        if let Some(id) = state.region_id
            && !self.regions.is_open(id)
        {
            return Err("tiled sequence region is no longer active".into());
        }
        Ok(())
    }

    fn charge_tiled_region(
        &mut self,
        state: &mut TiledSeqState,
        needed_rows: usize,
    ) -> Result<(), String> {
        let Some(id) = state.region_id else {
            return Ok(());
        };
        if !self.regions.is_open(id) {
            return Err("tiled sequence region is no longer active".into());
        }
        let target_bytes = needed_rows.saturating_mul(state.row_charge_bytes);
        if target_bytes > state.charged_bytes {
            let delta = target_bytes - state.charged_bytes;
            self.regions
                .alloc(id, delta, None)
                .map_err(|e| e.to_string())?;
            state.charged_bytes = target_bytes;
        }
        Ok(())
    }

    pub(super) fn tiled_op(
        &mut self,
        op: &TiledOp,
        args: Vec<Value>,
        env: &BTreeMap<String, crate::runtime::Slot>,
    ) -> Result<Value, String> {
        match op {
            TiledOp::New { adt } => {
                let decl = self
                    .world
                    .adts
                    .get(adt)
                    .ok_or_else(|| format!("unknown tiled struct '{adt}'"))?;
                let width = decl.tile_width.ok_or("struct is not tiled")?;
                let row_charge_bytes = row_charge_bytes(self.world, decl)?;
                Ok(Value::TiledSeq(Rc::new(RefCell::new(TiledSeqState {
                    adt: adt.clone(),
                    tile_width: width,
                    rows: Vec::new(),
                    region_id: None,
                    charged_bytes: 0,
                    row_charge_bytes,
                }))))
            }
            TiledOp::WithCapacity { adt } => {
                let cap = as_usize(args.first().ok_or("missing capacity")?, "capacity")?;
                let decl = self
                    .world
                    .adts
                    .get(adt)
                    .ok_or_else(|| format!("unknown tiled struct '{adt}'"))?;
                let width = decl.tile_width.ok_or("struct is not tiled")?;
                let row_charge_bytes = row_charge_bytes(self.world, decl)?;
                Ok(Value::TiledSeq(Rc::new(RefCell::new(TiledSeqState {
                    adt: adt.clone(),
                    tile_width: width,
                    rows: Vec::with_capacity(cap.min(65536)),
                    region_id: None,
                    charged_bytes: 0,
                    row_charge_bytes,
                }))))
            }
            TiledOp::WithRegion { adt, region_label } => {
                let cap = as_usize(args.first().ok_or("missing capacity")?, "capacity")?;
                let region_id = self.open_tiled_region(region_label, env)?;
                let decl = self
                    .world
                    .adts
                    .get(adt)
                    .ok_or_else(|| format!("unknown tiled struct '{adt}'"))?;
                let width = decl.tile_width.ok_or("struct is not tiled")?;
                let row_charge = row_charge_bytes(self.world, decl)?;
                let mut state = TiledSeqState {
                    adt: adt.clone(),
                    tile_width: width,
                    rows: Vec::with_capacity(cap.min(65536)),
                    region_id: Some(region_id),
                    charged_bytes: 0,
                    row_charge_bytes: row_charge,
                };
                if cap > 0 {
                    self.charge_tiled_region(&mut state, cap)?;
                }
                Ok(Value::TiledSeq(Rc::new(RefCell::new(state))))
            }
            TiledOp::Len => {
                let seq = expect_seq(args.first())?;
                let state = seq.borrow();
                self.ensure_tiled_region_active(&state)?;
                Ok(Value::UInt(state.rows.len() as u64, 64))
            }
            TiledOp::TileCount { .. } => {
                let seq = expect_seq(args.first())?;
                let state = seq.borrow();
                self.ensure_tiled_region_active(&state)?;
                Ok(Value::UInt(state.tile_count(), 64))
            }
            TiledOp::Push { adt } => {
                let seq = expect_seq(args.first())?;
                let Value::Data { ty, fields, .. } = args.get(1).ok_or("missing value")?.clone()
                else {
                    return Err(format!("TiledSeq::push expected struct '{adt}'"));
                };
                if &ty != adt {
                    return Err(format!("TiledSeq::push expected '{adt}', found '{ty}'"));
                }
                let mut state = seq.borrow_mut();
                let next_len = state.rows.len() + 1;
                self.charge_tiled_region(&mut state, next_len)?;
                state.rows.push(fields);
                Ok(Value::Tuple(vec![]))
            }
            TiledOp::Get { adt } => {
                let seq = expect_seq(args.first())?;
                let idx = as_usize(args.get(1).ok_or("missing index")?, "index")?;
                let state = seq.borrow();
                self.ensure_tiled_region_active(&state)?;
                let fields = state.rows.get(idx).cloned().ok_or_else(|| {
                    format!(
                        "TiledSeq::get index {idx} out of bounds (len {})",
                        state.rows.len()
                    )
                })?;
                Ok(Value::Data {
                    ty: adt.clone(),
                    constructor: adt.clone(),
                    fields,
                    resource_identity: None,
                })
            }
            TiledOp::Set { adt } => {
                let seq = expect_seq(args.first())?;
                let idx = as_usize(args.get(1).ok_or("missing index")?, "index")?;
                let Value::Data { ty, fields, .. } = args.get(2).ok_or("missing value")?.clone()
                else {
                    return Err(format!("TiledSeq::set expected struct '{adt}'"));
                };
                if &ty != adt {
                    return Err(format!("TiledSeq::set expected '{adt}', found '{ty}'"));
                }
                let mut state = seq.borrow_mut();
                self.ensure_tiled_region_active(&state)?;
                let len = state.rows.len();
                let slot = state.rows.get_mut(idx).ok_or_else(|| {
                    format!("TiledSeq::set index {idx} out of bounds (len {len})")
                })?;
                *slot = fields;
                Ok(Value::Tuple(vec![]))
            }
            TiledOp::ColGet { field, .. } => {
                let seq = expect_seq(args.first())?;
                let idx = as_usize(args.get(1).ok_or("missing index")?, "index")?;
                let state = seq.borrow();
                self.ensure_tiled_region_active(&state)?;
                let row = state.rows.get(idx).ok_or_else(|| {
                    format!(
                        "TiledSeq::col_get index {idx} out of bounds (len {})",
                        state.rows.len()
                    )
                })?;
                row.get(field)
                    .cloned()
                    .ok_or_else(|| format!("unknown tiled field '{field}'"))
            }
            TiledOp::ColSet { field, .. } => {
                let seq = expect_seq(args.first())?;
                let idx = as_usize(args.get(1).ok_or("missing index")?, "index")?;
                let val = args.get(2).ok_or("missing value")?.clone();
                let mut state = seq.borrow_mut();
                self.ensure_tiled_region_active(&state)?;
                let len = state.rows.len();
                let row = state.rows.get_mut(idx).ok_or_else(|| {
                    format!("TiledSeq::col_set index {idx} out of bounds (len {len})")
                })?;
                let entry = row
                    .get_mut(field)
                    .ok_or_else(|| format!("unknown tiled field '{field}'"))?;
                *entry = val;
                Ok(Value::Tuple(vec![]))
            }
            TiledOp::LoadTileVec { adt, field, shape } => {
                let seq = expect_seq(args.first())?;
                let tile_idx = as_usize(args.get(1).ok_or("missing tile index")?, "tile_idx")?;
                let state = seq.borrow();
                self.ensure_tiled_region_active(&state)?;
                let tile_count = state.tile_count() as usize;
                if tile_idx >= tile_count {
                    return Err(format!(
                        "TiledSeq::load_tile_vec tile index {tile_idx} out of bounds (tile_count {tile_count})"
                    ));
                }
                let elem_ty = self
                    .world
                    .adts
                    .get(adt)
                    .and_then(|a| a.variants.first())
                    .and_then(|v| v.fields.iter().find(|f| &f.name == field))
                    .map(|f| &f.ty);
                let is_bool =
                    matches!(elem_ty, Some(Type::Named(n, a)) if n == "bool" && a.is_empty());
                let w = state.tile_width as usize;
                let base = tile_idx * w;
                let mut lanes = Vec::with_capacity(w);
                for lane in 0..w {
                    let v = state.rows.get(base + lane).and_then(|r| r.get(field));
                    if is_bool {
                        let b = match v {
                            Some(Value::Bool(b)) => u64::from(*b),
                            None => 0,
                            _ => return Err("expected bool in bool hot column".into()),
                        };
                        lanes.push(Value::UInt(b, 8));
                    } else {
                        lanes.push(lane_from_value(shape.elem, v)?);
                    }
                }
                Ok(Value::Tuple(lanes))
            }
        }
    }
}

fn expect_seq(v: Option<&Value>) -> Result<Rc<RefCell<TiledSeqState>>, String> {
    match v {
        Some(Value::TiledSeq(seq)) => Ok(Rc::clone(seq)),
        _ => Err("expected a TiledSeq value".into()),
    }
}
