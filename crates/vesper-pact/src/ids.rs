//! Stable numeric identifiers used at the compiler boundary.
//!
//! Every identifier is an index into the corresponding declaration table of a
//! [`crate::ProgramDto`]. The solver never consults source names: the
//! integration adapter owns the mapping from resolved compiler `DefId`s to
//! these indices and renders names in diagnostics.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "#{}"), self.0)
            }
        }
    };
}

id!(
    /// A package node of the resolved dependency graph (`Loom.lock`).
    PackageId,
    "pkg"
);
id!(
    /// A nominal type constructor (structs, enums, primitives, tuples of a fixed arity).
    TypeCtorId,
    "type"
);
id!(
    /// A trait declaration.
    TraitId,
    "trait"
);
id!(
    /// A declared effect (e.g. `Fail`, `Net`, `Isa`).
    EffectId,
    "effect"
);
id!(
    /// An anonymous canonical `impl` (Tier 1, participates in implicit resolution).
    ImplId,
    "impl"
);
id!(
    /// A named lexical `model` (Tier 2, never found by canonical search).
    ModelId,
    "model"
);

/// Index of an associated type within its trait's declaration list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssocIdx(pub u32);

impl fmt::Display for AssocIdx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "assoc#{}", self.0)
    }
}
