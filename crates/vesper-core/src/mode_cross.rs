//! Modal kinding and `[T-Cross]` mode-crossing judgments (§3.2, §4.2).
//!
//! Complements the lattice representation in [`crate::mode_vec`] with:
//! - Modal kind synthesis (`Delta |- tau : mod m_0`) for `[K-Prim]`, `[K-Var]`,
//!   generic `[K-Adt]` instantiation with field-modality well-formedness
//!   (`k_i <= m_i`, `E0105`), and `[K-Fn]`.
//! - `[T-SubMode]`: modal weakening (`m_1 <= m_2`).
//! - `[T-Cross]`: mode crossing strengthening generic or linear expressions
//!   back to `m_0` when `Delta |- tau : mod m_0` and `m_0 <= m`.
//! - `[T-Var]`: variable lookup enforcing context presence `rho_ctx <= rho`
//!   and discardability (`discard(Gamma)`) of residual bindings.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::mode_vec::{Contention, ModeVec, Order, Portability, Usage};

/// Quantitative presence in `Delta ; Sigma ; Phi ; Gamma |- e : tau @ (rho, m) | eff`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Presence {
    /// `@0` ghost witness / refinement index (erased at codegen).
    Ghost = 0,
    /// `@1` runtime computation value.
    Runtime = 1,
}

/// Borrow / destination provenance of a binding in `Gamma`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaceOwnership {
    Own,
    SharedBorrow,
    MutBorrow,
    OutSlot,
}

/// Primitive scalar types covered by `[K-Prim]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrimKind {
    Bool,
    U8,
    U16,
    U32,
    U64,
    Usize,
    I8,
    I16,
    I32,
    I64,
    Isize,
    F32,
    F64,
    Unit,
    Never,
}

/// Field specification inside an ADT declaration (`f_i: ty @@ override_mode`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdtFieldKind {
    pub name: String,
    pub ty: KindType,
    /// Explicit `@@ m_i` field override; defaults to `ModeVec::TOP` when omitted.
    pub override_mode: ModeVec,
}

/// Type expression for modal kind computation (`Delta |- tau : mod m_0`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindType {
    /// `[K-Prim]`: `scalar(tau) => mod (copy, portable, uncontended, unordered)`.
    Prim(PrimKind),
    /// `[K-Var]`: `(alpha : mod m) in Delta`.
    Var(String),
    /// `[K-Adt]`: parameterized ADT declaration instantiated at `type_args`.
    Adt {
        name: String,
        decl_mode: ModeVec,
        params: Vec<String>,
        fields: Vec<AdtFieldKind>,
        type_args: Vec<KindType>,
    },
    /// `[K-Fn]`: `tau_1 ->^(eff)_m tau_2 : mod m`.
    Fn { fn_mode: ModeVec },
}

impl KindType {
    fn free_vars_into(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Prim(_) | Self::Fn { .. } => {}
            Self::Var(name) => {
                out.insert(name.clone());
            }
            Self::Adt {
                params,
                fields,
                type_args,
                ..
            } => {
                for arg in type_args {
                    arg.free_vars_into(out);
                }
                let mut inner = BTreeSet::new();
                for field in fields {
                    field.ty.free_vars_into(&mut inner);
                }
                for p in params {
                    inner.remove(p);
                }
                out.extend(inner);
            }
        }
    }

    /// Instantiate generic parameter `params` with `args` for `[K-Adt]`.
    pub fn substitute(&self, subst: &BTreeMap<String, KindType>) -> Self {
        match self {
            Self::Prim(p) => Self::Prim(*p),
            Self::Var(name) => subst
                .get(name)
                .cloned()
                .unwrap_or_else(|| Self::Var(name.clone())),
            Self::Adt {
                name,
                decl_mode,
                params,
                fields,
                type_args,
            } => {
                let new_type_args = type_args.iter().map(|a| a.substitute(subst)).collect();
                let mut field_subst: BTreeMap<String, KindType> = subst
                    .iter()
                    .filter(|(k, _)| !params.iter().any(|p| p == *k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();

                if field_subst.is_empty() {
                    return Self::Adt {
                        name: name.clone(),
                        decl_mode: *decl_mode,
                        params: params.clone(),
                        fields: fields.clone(),
                        type_args: new_type_args,
                    };
                }

                let mut field_free = BTreeSet::new();
                for field in fields {
                    field.ty.free_vars_into(&mut field_free);
                }
                let mut replacement_free = BTreeSet::new();
                for (k, v) in &field_subst {
                    if field_free.contains(k) {
                        v.free_vars_into(&mut replacement_free);
                    }
                }

                let mut new_params = params.clone();
                let mut alpha_rename: BTreeMap<String, KindType> = BTreeMap::new();
                let mut used_names: BTreeSet<String> = field_free;
                used_names.extend(replacement_free.iter().cloned());
                used_names.extend(params.iter().cloned());

                for param in &mut new_params {
                    if replacement_free.contains(param.as_str()) {
                        let mut idx = 0usize;
                        let fresh = loop {
                            let cand = format!("{param}__alpha{idx}");
                            if !used_names.contains(&cand) {
                                break cand;
                            }
                            idx += 1;
                        };
                        used_names.insert(fresh.clone());
                        alpha_rename.insert(param.clone(), Self::Var(fresh.clone()));
                        *param = fresh;
                    }
                }

                for (old, new_var) in alpha_rename {
                    field_subst.insert(old, new_var);
                }

                Self::Adt {
                    name: name.clone(),
                    decl_mode: *decl_mode,
                    params: new_params,
                    fields: fields
                        .iter()
                        .map(|f| AdtFieldKind {
                            name: f.name.clone(),
                            ty: f.ty.substitute(&field_subst),
                            override_mode: f.override_mode,
                        })
                        .collect(),
                    type_args: new_type_args,
                }
            }
            Self::Fn { fn_mode } => Self::Fn { fn_mode: *fn_mode },
        }
    }
}

/// Kind context `Delta` binding type variables `alpha : mod m`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindEnv {
    vars: BTreeMap<String, ModeVec>,
}

impl KindEnv {
    pub fn bind_var(&mut self, name: impl Into<String>, bound: ModeVec) {
        self.vars.insert(name.into(), bound);
    }

    pub fn lookup(&self, name: &str) -> Option<ModeVec> {
        self.vars.get(name).copied()
    }
}

/// Errors returned by modal kinding, `[T-Cross]`, `[T-SubMode]`, and `[T-Var]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeJudgmentError {
    UnboundKindVar {
        name: String,
    },
    ArityMismatch {
        adt: String,
        expected: usize,
        actual: usize,
    },
    /// `E0105`: field component kind `k_i` is not bounded by override `m_i`.
    FieldModalityViolation {
        adt: String,
        field: String,
        component_kind: ModeVec,
        override_mode: ModeVec,
    },
    /// `[T-SubMode]` / `[T-Cross]` order violation (`lhs </= rhs`).
    ModeOrderViolation {
        actual: ModeVec,
        required: ModeVec,
    },
    /// `[T-Var]`: binding missing in `Gamma`.
    UnboundVar {
        name: String,
    },
    /// `[T-Var]`: runtime context (`@1`) cannot use a ghost (`@0`) binding.
    PresenceViolation {
        name: String,
        ctx_presence: Presence,
        binding_presence: Presence,
    },
    /// `[T-Var]`: binding is uninitialized (e.g., an unwritten `OutSlot`).
    UninitializedVar {
        name: String,
    },
    /// `[T-Var]`: residual `Gamma` contains a non-discardable obligation.
    ResidualNotDiscardable {
        name: String,
    },
}

/// Compute `Delta |- ty : mod m_0` via `[K-Prim]`, `[K-Var]`, `[K-Adt]`, `[K-Fn]`.
pub fn infer_modal_kind(delta: &KindEnv, ty: &KindType) -> Result<ModeVec, ModeJudgmentError> {
    match ty {
        KindType::Prim(_) => Ok(ModeVec::BOTTOM),
        KindType::Var(name) => delta
            .lookup(name)
            .ok_or_else(|| ModeJudgmentError::UnboundKindVar { name: name.clone() }),
        KindType::Fn { fn_mode } => Ok(*fn_mode),
        KindType::Adt {
            name,
            decl_mode,
            params,
            fields,
            type_args,
        } => {
            if params.len() != type_args.len() {
                return Err(ModeJudgmentError::ArityMismatch {
                    adt: name.clone(),
                    expected: params.len(),
                    actual: type_args.len(),
                });
            }
            let subst: BTreeMap<String, KindType> = params
                .iter()
                .cloned()
                .zip(type_args.iter().cloned())
                .collect();
            let mut acc = *decl_mode;
            for field in fields {
                let instantiated = field.ty.substitute(&subst);
                let comp_kind = infer_modal_kind(delta, &instantiated)?;
                if !comp_kind.leq(field.override_mode) {
                    return Err(ModeJudgmentError::FieldModalityViolation {
                        adt: name.clone(),
                        field: field.name.clone(),
                        component_kind: comp_kind,
                        override_mode: field.override_mode,
                    });
                }
                acc = acc.join(comp_kind.meet(field.override_mode));
            }
            Ok(acc)
        }
    }
}

/// `[T-SubMode]`: weaken expression mode `m_1` to `m_2` iff `m_1 <= m_2`.
pub fn sub_mode(m_1: ModeVec, m_2: ModeVec) -> Result<ModeVec, ModeJudgmentError> {
    if m_1.leq(m_2) {
        Ok(m_2)
    } else {
        Err(ModeJudgmentError::ModeOrderViolation {
            actual: m_1,
            required: m_2,
        })
    }
}

/// `[T-Cross]`: strengthen expression mode `expr_mode` down to the type's intrinsic
/// kind `m_0` whenever `Delta |- ty : mod m_0` and `m_0 <= expr_mode`.
pub fn mode_cross(
    delta: &KindEnv,
    ty: &KindType,
    expr_mode: ModeVec,
) -> Result<ModeVec, ModeJudgmentError> {
    let m_0 = infer_modal_kind(delta, ty)?;
    if m_0.leq(expr_mode) {
        Ok(m_0)
    } else {
        Err(ModeJudgmentError::ModeOrderViolation {
            actual: m_0,
            required: expr_mode,
        })
    }
}

/// One entry in a place context `Gamma` for `[T-Var]` checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarContextEntry {
    pub name: String,
    pub presence: Presence,
    pub mode: ModeVec,
    pub ownership: PlaceOwnership,
    pub initialized: bool,
}

impl VarContextEntry {
    /// Whether this entry satisfies `discard(Gamma)` (§4.1).
    pub fn is_discardable(&self) -> bool {
        self.initialized
            && self.mode.usage() <= Usage::Affine
            && !matches!(
                self.ownership,
                PlaceOwnership::MutBorrow | PlaceOwnership::OutSlot
            )
    }
}

/// `[T-Var]`: validate residual discardability `discard(Gamma)` and presence
/// constraint `rho_ctx <= rho`, returning `(rho, mode)` for target variable `target`.
pub fn check_var_use(
    gamma: &[VarContextEntry],
    ctx_presence: Presence,
    target: &str,
) -> Result<(Presence, ModeVec), ModeJudgmentError> {
    let mut found = None;
    for entry in gamma {
        if entry.name == target && found.is_none() {
            if (ctx_presence as u8) > (entry.presence as u8) {
                return Err(ModeJudgmentError::PresenceViolation {
                    name: entry.name.clone(),
                    ctx_presence,
                    binding_presence: entry.presence,
                });
            }
            if !entry.initialized {
                return Err(ModeJudgmentError::UninitializedVar {
                    name: entry.name.clone(),
                });
            }
            found = Some((entry.presence, entry.mode));
        } else if !entry.is_discardable() {
            return Err(ModeJudgmentError::ResidualNotDiscardable {
                name: entry.name.clone(),
            });
        }
    }
    found.ok_or_else(|| ModeJudgmentError::UnboundVar {
        name: target.to_string(),
    })
}

/// Convenience constant constructor for a non-pinned, uncontended, unordered mode
/// with given `Usage`.
pub const fn usage_mode(usage: Usage) -> ModeVec {
    ModeVec::new(
        usage,
        Portability::Portable,
        Contention::Uncontended,
        Order::Unordered,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option_of(arg: KindType) -> KindType {
        KindType::Adt {
            name: "Option".into(),
            decl_mode: ModeVec::BOTTOM,
            params: vec!["T".into()],
            fields: vec![AdtFieldKind {
                name: "some_payload".into(),
                ty: KindType::Var("T".into()),
                override_mode: ModeVec::TOP,
            }],
            type_args: vec![arg],
        }
    }

    #[test]
    fn option_specializes_across_usage_modes_and_crosses_on_scalar() {
        let delta = KindEnv::default();
        let opt_u64 = option_of(KindType::Prim(PrimKind::U64));
        assert_eq!(infer_modal_kind(&delta, &opt_u64), Ok(ModeVec::BOTTOM));

        // [T-Cross]: generic `linear` expression returning Option[u64] strengthens back to Copy.
        let crossed = mode_cross(&delta, &opt_u64, usage_mode(Usage::Linear)).unwrap();
        assert_eq!(crossed, usage_mode(Usage::Copy));
    }

    #[test]
    fn field_modality_override_rejects_linear_in_copy_container() {
        let mut delta = KindEnv::default();
        delta.bind_var("Txn", usage_mode(Usage::Linear));
        let bad_wrapper = KindType::Adt {
            name: "BadWrapper".into(),
            decl_mode: ModeVec::BOTTOM,
            params: vec![],
            fields: vec![AdtFieldKind {
                name: "ticket".into(),
                ty: KindType::Var("Txn".into()),
                override_mode: usage_mode(Usage::Copy),
            }],
            type_args: vec![],
        };

        assert_eq!(
            infer_modal_kind(&delta, &bad_wrapper),
            Err(ModeJudgmentError::FieldModalityViolation {
                adt: "BadWrapper".into(),
                field: "ticket".into(),
                component_kind: usage_mode(Usage::Linear),
                override_mode: usage_mode(Usage::Copy),
            })
        );
    }

    #[test]
    fn t_var_enforces_presence_and_discardability() {
        let gamma = vec![
            VarContextEntry {
                name: "x".into(),
                presence: Presence::Runtime,
                mode: usage_mode(Usage::Linear),
                ownership: PlaceOwnership::Own,
                initialized: true,
            },
            VarContextEntry {
                name: "unused_aff".into(),
                presence: Presence::Runtime,
                mode: usage_mode(Usage::Affine),
                ownership: PlaceOwnership::Own,
                initialized: true,
            },
        ];
        assert_eq!(
            check_var_use(&gamma, Presence::Runtime, "x"),
            Ok((Presence::Runtime, usage_mode(Usage::Linear)))
        );

        let with_unused_linear = vec![
            VarContextEntry {
                name: "x".into(),
                presence: Presence::Runtime,
                mode: usage_mode(Usage::Copy),
                ownership: PlaceOwnership::Own,
                initialized: true,
            },
            VarContextEntry {
                name: "live_txn".into(),
                presence: Presence::Runtime,
                mode: usage_mode(Usage::Linear),
                ownership: PlaceOwnership::Own,
                initialized: true,
            },
        ];
        assert_eq!(
            check_var_use(&with_unused_linear, Presence::Runtime, "x"),
            Err(ModeJudgmentError::ResidualNotDiscardable {
                name: "live_txn".into()
            })
        );
    }
}
