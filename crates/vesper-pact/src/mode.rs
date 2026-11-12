//! Modal kinding of goal types: `mode(τ)` as a join of atoms.

use crate::program::{Env, Program};
use crate::term::*;

/// One summand of `mode(τ) = a_1 ⊔ ... ⊔ a_n`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModeAtom {
    Known(ModeTerm),
    /// Mode of an unresolved type inference variable.
    Unknown,
}

/// Computes the atoms of a fully resolved, normalized type. Projections are
/// treated as rigid and bounded by their associated type's declared bound.
pub(crate) fn mode_atoms(
    program: &Program,
    env: &Env,
    ty: &Ty,
    out: &mut Vec<ModeAtom>,
) -> Result<(), DuplicateLabel> {
    match ty {
        Ty::Ctor { ctor, args } => {
            let scheme = &program.ctor(*ctor).mode;
            out.push(ModeAtom::Known(ModeTerm::Const(scheme.base)));
            for &j in &scheme.joins {
                if let GenericArg::Ty(t) = &args[j as usize] {
                    mode_atoms(program, env, t, out)?;
                }
            }
        }
        Ty::Var(Var::Placeholder(p)) => {
            let bound = env.placeholders[*p as usize]
                .mode_bound
                .unwrap_or(Mode::Linear);
            out.push(ModeAtom::Known(ModeTerm::Const(bound)));
        }
        Ty::Var(_) => out.push(ModeAtom::Unknown),
        Ty::Projection(p) => {
            let t = program.trait_decl(p.trait_ref.trait_id);
            let mut binder = vec![GenericArg::Ty(p.trait_ref.self_ty.clone())];
            binder.extend(p.trait_ref.args.iter().cloned());
            let bound = instantiate_mode(&t.assoc[p.assoc.0 as usize].mode_bound, &binder);
            out.push(match bound {
                ModeTerm::Var(Var::Infer(_)) => ModeAtom::Unknown,
                b => ModeAtom::Known(b),
            });
        }
    }
    Ok(())
}

pub(crate) enum ModeVerdict {
    Holds,
    Fails { atom: ModeTerm },
    Unknown,
}

/// Decides `a_1 ⊔ ... ⊔ a_n ⊑ bound`. A definitely failing atom dominates
/// unknown atoms; rigid mode placeholders are only below themselves and
/// `linear`.
pub(crate) fn decide(atoms: &[ModeAtom], bound: &ModeTerm) -> ModeVerdict {
    let mut unknown = false;
    for atom in atoms {
        let a = match atom {
            ModeAtom::Known(a) => a,
            ModeAtom::Unknown => {
                if *bound != ModeTerm::Const(Mode::Linear) {
                    unknown = true;
                }
                continue;
            }
        };
        let ok = match (a, bound) {
            (ModeTerm::Const(Mode::Copy), _) => Some(true),
            (_, ModeTerm::Const(Mode::Linear)) => Some(true),
            (ModeTerm::Const(x), ModeTerm::Const(y)) => Some(x.is_below(*y)),
            (ModeTerm::Var(Var::Infer(_)), _) | (_, ModeTerm::Var(Var::Infer(_))) => None,
            (ModeTerm::Var(x), ModeTerm::Var(y)) => Some(x == y),
            (ModeTerm::Var(_), ModeTerm::Const(_)) | (ModeTerm::Const(_), ModeTerm::Var(_)) => {
                Some(false)
            }
        };
        match ok {
            Some(true) => {}
            Some(false) => return ModeVerdict::Fails { atom: a.clone() },
            None => unknown = true,
        }
    }
    if unknown {
        ModeVerdict::Unknown
    } else {
        ModeVerdict::Holds
    }
}
