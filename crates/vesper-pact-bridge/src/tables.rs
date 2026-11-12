//! Dense Pact id tables with reverse maps.
//!
//! Every table is sorted by external [`DefPath`], so indices depend only on
//! the set of declarations: reordering or reformatting sources leaves them
//! unchanged. They are still revision-local: adding a declaration can shift
//! later indices. Use [`DefPath`]s, never indices, as stable public names.

use crate::identity::{DefPath, PackageIdentity};
use std::collections::BTreeMap;
use vesper::ast::Mode;
use vesper_pact::{
    EffectId, ImplId, ModeScheme, ModelId, PackageId, ParamKind, TraitId, TypeCtorId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypeOrigin {
    Primitive,
    Unit,
    Tuple(usize),
    Adt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceParam {
    pub name: String,
    pub kind: ParamKind,
    /// Declared modal bound (type parameters).
    pub bound: Mode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeEntry {
    pub path: DefPath,
    pub package: PackageId,
    /// Parameters of the source declaration, in order.
    pub params: Vec<SourceParam>,
    /// Appended brand parameters with the trait each brand identifies.
    pub brands: Vec<(String, TraitId)>,
    pub mode: ModeScheme,
    pub(crate) origin: TypeOrigin,
}

impl TypeEntry {
    pub fn pact_params(&self) -> Vec<ParamKind> {
        let mut p: Vec<ParamKind> = self.params.iter().map(|p| p.kind).collect();
        p.extend(self.brands.iter().map(|_| ParamKind::Brand));
        p
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectEntry {
    pub path: DefPath,
    pub package: PackageId,
    pub params: Vec<(String, ParamKind)>,
    /// Declared modal bound of each parameter (`linear` for rows).
    pub bounds: Vec<Mode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraitEntry {
    pub path: DefPath,
    pub package: PackageId,
    pub params: Vec<(String, ParamKind)>,
    pub assoc: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConformanceEntry {
    pub path: DefPath,
    pub package: PackageId,
    pub trait_id: Option<TraitId>,
}

/// The id tables of one declaration revision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdTables {
    pub(crate) packages: Vec<PackageIdentity>,
    pub(crate) types: Vec<TypeEntry>,
    pub(crate) effects: Vec<EffectEntry>,
    pub(crate) traits: Vec<TraitEntry>,
    pub(crate) impls: Vec<ConformanceEntry>,
    pub(crate) models: Vec<ConformanceEntry>,
    pub(crate) package_index: BTreeMap<PackageIdentity, PackageId>,
    pub(crate) type_index: BTreeMap<DefPath, TypeCtorId>,
    pub(crate) effect_index: BTreeMap<DefPath, EffectId>,
    pub(crate) trait_index: BTreeMap<DefPath, TraitId>,
    pub(crate) impl_index: BTreeMap<DefPath, ImplId>,
    pub(crate) model_index: BTreeMap<DefPath, ModelId>,
}

impl IdTables {
    pub fn packages(&self) -> &[PackageIdentity] {
        &self.packages
    }
    pub fn package_id(&self, p: &PackageIdentity) -> Option<PackageId> {
        self.package_index.get(p).copied()
    }
    pub fn package(&self, id: PackageId) -> Option<&PackageIdentity> {
        self.packages.get(id.index())
    }
    pub fn types(&self) -> &[TypeEntry] {
        &self.types
    }
    pub fn type_id(&self, p: &DefPath) -> Option<TypeCtorId> {
        self.type_index.get(p).copied()
    }
    pub fn type_entry(&self, id: TypeCtorId) -> Option<&TypeEntry> {
        self.types.get(id.index())
    }
    pub fn effect_id(&self, p: &DefPath) -> Option<EffectId> {
        self.effect_index.get(p).copied()
    }
    pub fn effect_entry(&self, id: EffectId) -> Option<&EffectEntry> {
        self.effects.get(id.index())
    }
    pub fn trait_id(&self, p: &DefPath) -> Option<TraitId> {
        self.trait_index.get(p).copied()
    }
    pub fn trait_entry(&self, id: TraitId) -> Option<&TraitEntry> {
        self.traits.get(id.index())
    }
    pub fn impl_id(&self, p: &DefPath) -> Option<ImplId> {
        self.impl_index.get(p).copied()
    }
    pub fn impl_path(&self, id: ImplId) -> Option<&DefPath> {
        self.impls.get(id.index()).map(|e| &e.path)
    }
    pub fn model_id(&self, p: &DefPath) -> Option<ModelId> {
        self.model_index.get(p).copied()
    }
    pub fn model_path(&self, id: ModelId) -> Option<&DefPath> {
        self.models.get(id.index()).map(|e| &e.path)
    }

    /// Replaces Pact id tokens (`type#3`, `trait#0`, ...) in rendered Pact
    /// output with external names. Unknown ids are left as they are.
    pub fn rename_ids(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        'outer: while i < s.len() {
            for prefix in ["pkg#", "type#", "trait#", "effect#", "impl#", "model#"] {
                if s[i..].starts_with(prefix) {
                    let start = i + prefix.len();
                    let mut end = start;
                    while end < s.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    let preceded = i > 0 && (bytes[i - 1].is_ascii_alphanumeric());
                    if end > start
                        && !preceded
                        && let Ok(n) = s[start..end].parse::<u32>()
                        && let Some(name) = self.id_name(prefix, n)
                    {
                        out.push_str(&name);
                        i = end;
                        continue 'outer;
                    }
                }
            }
            let ch = s[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    fn id_name(&self, prefix: &str, n: u32) -> Option<String> {
        let n = n as usize;
        Some(match prefix {
            "pkg#" => self.packages.get(n)?.to_string(),
            "type#" => self.type_name(n)?,
            "trait#" => self.traits.get(n)?.path.to_string(),
            "effect#" => self.effects.get(n)?.path.to_string(),
            "impl#" => self.impls.get(n)?.path.to_string(),
            "model#" => self.models.get(n)?.path.to_string(),
            _ => return None,
        })
    }

    pub(crate) fn type_name(&self, n: usize) -> Option<String> {
        let e = self.types.get(n)?;
        Some(match e.origin {
            TypeOrigin::Tuple(k) => format!("tuple/{k}"),
            _ if e.path.package.is_builtin() => e.path.name.clone(),
            _ => e.path.to_string(),
        })
    }
}
