//! Constructor mode schemes derived from the checker's `World::mode`.
//!
//! Pact summarizes a constructor's modal kind as `base ⊔ mode(args[i]) for i
//! in joins`. The checker computes `World::mode` structurally (field
//! modalities, generic bounds and recursive instantiations). A scheme is
//! derived by probing `World::mode` with symbolic arguments and accepted only
//! after it agrees with `World::mode` on every well-formed combination of
//! argument modes; otherwise the constructor is reported as unsupported.
//! `World::mode` is compositional at the top level (argument modes are
//! computed before the constructor's fields are visited), so agreement on
//! argument modes implies agreement on every ground well-formed instantiation.

use crate::source::SourceWorld;
use std::collections::BTreeMap;
use vesper_core::ast::{Adt, Effects, Mode, Type};
use vesper_pact::ModeScheme;

/// At most `3^8` argument-mode combinations are verified per constructor.
pub const MAX_VERIFIED_COMBINATIONS: usize = 6561;

pub(crate) fn to_pact(m: Mode) -> vesper_pact::Mode {
    match m {
        Mode::Copy => vesper_pact::Mode::Copy,
        Mode::Affine => vesper_pact::Mode::Affine,
        Mode::Linear => vesper_pact::Mode::Linear,
    }
}

pub(crate) fn from_pact(m: vesper_pact::Mode) -> Mode {
    match m {
        vesper_pact::Mode::Copy => Mode::Copy,
        vesper_pact::Mode::Affine => Mode::Affine,
        vesper_pact::Mode::Linear => Mode::Linear,
    }
}

const MODES: [Mode; 3] = [Mode::Copy, Mode::Affine, Mode::Linear];

fn probe_name(i: usize) -> String {
    // `$` cannot start a source identifier, so probes never alias a type.
    format!("$probe{i}")
}

fn evaluate(world: &dyn SourceWorld, adt: &Adt, modes: &[Option<Mode>]) -> Mode {
    let mut bounds = BTreeMap::new();
    let args = adt
        .generics
        .iter()
        .enumerate()
        .map(|(i, g)| {
            if g.row {
                Type::Row(Effects::default())
            } else {
                bounds.insert(probe_name(i), modes[i].unwrap_or(Mode::Copy));
                Type::Named(probe_name(i), vec![])
            }
        })
        .collect();
    world.mode(&Type::Named(adt.name.clone(), args), &bounds)
}

/// Derives and verifies the scheme of `adt` against `world`.
pub(crate) fn derive_scheme(world: &dyn SourceWorld, adt: &Adt) -> Result<ModeScheme, String> {
    let type_params: Vec<usize> = adt
        .generics
        .iter()
        .enumerate()
        .filter(|(_, g)| !g.row)
        .map(|(i, _)| i)
        .collect();
    let combos = type_params.iter().try_fold(1usize, |acc, &i| {
        acc.checked_mul(
            MODES
                .iter()
                .filter(|m| **m <= adt.generics[i].bound)
                .count(),
        )
    });
    match combos {
        Some(n) if n <= MAX_VERIFIED_COMBINATIONS => {}
        _ => {
            return Err(format!(
                "`{}` has {} type parameters; verifying its mode scheme against the checker \
                 needs more than {MAX_VERIFIED_COMBINATIONS} argument-mode combinations",
                adt.name,
                type_params.len()
            ));
        }
    }
    let n = adt.generics.len();
    let all_copy: Vec<Option<Mode>> = (0..n).map(|_| Some(Mode::Copy)).collect();
    let base = evaluate(world, adt, &all_copy);
    // A parameter is joined when raising it alone raises the result. The
    // probe may exceed the declared bound; agreement is only required on the
    // well-formed domain below, where the field-modality meet is the identity.
    let joins: Vec<u32> = type_params
        .iter()
        .copied()
        .filter(|&i| {
            let mut m = all_copy.clone();
            m[i] = Some(Mode::Linear);
            evaluate(world, adt, &m) > base
        })
        .map(|i| i as u32)
        .collect();
    let mut current = all_copy;
    verify(world, adt, &type_params, 0, &mut current, base, &joins)?;
    Ok(ModeScheme {
        base: to_pact(base),
        joins,
    })
}

fn verify(
    world: &dyn SourceWorld,
    adt: &Adt,
    params: &[usize],
    k: usize,
    current: &mut Vec<Option<Mode>>,
    base: Mode,
    joins: &[u32],
) -> Result<(), String> {
    if k == params.len() {
        let expected = joins.iter().fold(base, |m, &j| {
            m.join(current[j as usize].unwrap_or(Mode::Copy))
        });
        let actual = evaluate(world, adt, current);
        if actual != expected {
            let shown: Vec<String> = params
                .iter()
                .map(|&i| {
                    format!(
                        "{} = {:?}",
                        adt.generics[i].name,
                        current[i].unwrap_or(Mode::Copy)
                    )
                })
                .collect();
            return Err(format!(
                "the checker's mode of `{}` with [{}] is {actual:?}, which is not \
                 `base ⊔ joined arguments` ({expected:?}); Pact's ModeScheme cannot represent it",
                adt.name,
                shown.join(", ")
            ));
        }
        return Ok(());
    }
    let i = params[k];
    for m in MODES {
        if m <= adt.generics[i].bound {
            current[i] = Some(m);
            verify(world, adt, params, k + 1, current, base, joins)?;
        }
    }
    current[i] = Some(Mode::Copy);
    Ok(())
}
