//! Modal kinding of goal types: `mode(τ)` as a join of atoms.

use crate::term::*;

/// One summand of `mode(τ) = a_1 ⊔ ... ⊔ a_n`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModeAtom {
    Known(ModeTerm),
    /// Mode of an unresolved type inference variable.
    Unknown,
}
