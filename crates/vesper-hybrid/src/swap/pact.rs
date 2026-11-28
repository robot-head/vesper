// The trait frontend (SH2 lane 13, #1185), from `modules/pact`: the
// declaration pass, the Pact session, builtin wanted and the
// dependency pass of `@vesper/check-pact`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/pact.rs"]
mod pact_binding;

// The trait declaration pass's encoder and decoder (#1185), shared with
// the root package's tests/self_host_check_pact.rs.
#[path = "../../../../tests/common/pact_words.rs"]
mod pact_words;

use super::{Registry, Swap, check_words, must, on, open, pass, stats};

/// Packages whose trait declarations the Vesper declaration pass checked.
static PACT_CALLS: AtomicU64 = AtomicU64::new(0);
/// The Pact swaps' costs.
static COST: stats::Cost = stats::Cost::new("pact");

/// `vesper::pact_frontend::DeclarationPassTable` from `modules/pact`: the
/// declaration pass of `@vesper/check-pact` (collecting and validating the
/// trait, conformance, `use model` and bound declarations of a package
/// without dependencies). A module whose literals the decoder cannot read
/// stays in Rust; a clean answer leaves the Pact session to Rust.
const VESPER_DECLARATION_PASS: vesper::pact_frontend::DeclarationPassTable =
    vesper::pact_frontend::DeclarationPassTable {
        diagnostics: |module, base_world, identity| {
            let timer = stats::Timer::start();
            let input = pact_words::declaration_input(module, base_world, &identity.to_string())?;
            let timer = timer.lap(&COST.encode);
            let words = pact_binding::pass_words(&input);
            let timer = timer.lap(&COST.vesper);
            let diagnostics = pact_words::decode_declarations(&words);
            timer.lap(&COST.decode);
            PACT_CALLS.fetch_add(1, Relaxed);
            COST.call(
                false,
                || {
                    vesper::pact_frontend::rust_declaration_diagnostics(
                        module, base_world, identity,
                    )
                },
                |rust| rust.as_ref() == Some(&diagnostics),
            );
            Some(diagnostics)
        },
    };

struct VesperPactSessionImpl {
    inner: std::sync::Mutex<pact_binding::vesper__check_2pact__session__PactSession>,
    revision: String,
}

impl vesper::pact_frontend::VesperPactSession for VesperPactSessionImpl {
    fn revision(&self) -> &str {
        &self.revision
    }

    fn solve(
        &self,
        q: &vesper::pact_frontend::PactQuery<'_>,
    ) -> Result<(usize, String), (u8, String)> {
        let timer = stats::Timer::start();
        let sq = pact_words::SessionQuery {
            env_params: q.env_params,
            assumptions: q.assumptions,
            scopes: q.scopes,
            infer: q.infer,
            goal: q.goal.clone(),
        };
        let qbytes = pact_words::encode_session_query(&sq);
        let timer = timer.lap(&COST.encode);
        let words = {
            let mut sess = self.inner.lock().expect("pact session mutex");
            pact_binding::pact_session_solve(&mut sess, &qbytes)
        };
        let timer = timer.lap(&COST.vesper);
        let res = pact_words::decode_solve_words(&words);
        timer.lap(&COST.decode);
        PACT_CALLS.fetch_add(1, Relaxed);
        res
    }

    fn mono_check(
        &self,
        input: &vesper::pact_frontend::MonoCheckOutcome,
    ) -> Option<Result<Vec<bool>, Vec<vesper_core::ast::Diagnostic>>> {
        let timer = stats::Timer::start();
        let bytes = pact_words::encode_mono_body(input, &[]);
        let timer = timer.lap(&COST.encode);
        let words = {
            let mut sess = self.inner.lock().expect("pact session mutex");
            pact_binding::pact_session_mono_check(&mut sess, &bytes)
        };
        let timer = timer.lap(&COST.vesper);
        let (_session_diags, ok, bounded, errors) = pact_words::decode_mono_check(&words);
        timer.lap(&COST.decode);
        PACT_CALLS.fetch_add(1, Relaxed);
        if !ok {
            return None;
        }
        if errors.is_empty() {
            Some(Ok(bounded))
        } else {
            Some(Err(errors))
        }
    }
}

/// `builtin_traits::wanted` from `modules/pact` (SH2 lane 13, `#1185`).
const VESPER_BUILTIN_WANTED: vesper::pact_frontend::BuiltinWantedTable =
    vesper::pact_frontend::BuiltinWantedTable {
        wanted: |module| {
            let timer = stats::Timer::start();
            let input = pact_words::encode_builtin_wanted(module);
            let timer = timer.lap(&COST.encode);
            let words = pact_binding::pact_builtin_wanted(&input);
            let timer = timer.lap(&COST.vesper);
            let wanted = words.first().copied().unwrap_or(0) != 0;
            timer.lap(&COST.decode);
            PACT_CALLS.fetch_add(1, Relaxed);
            Some(wanted)
        },
    };

/// `Frontend::add_dependency` declaration pass from `modules/pact` (SH2 lane 13, `#1185`).
const VESPER_DEPS_PASS: vesper::pact_frontend::DepsPassTable =
    vesper::pact_frontend::DepsPassTable {
        diagnostics: |module, base_world, identity, deps| {
            let timer = stats::Timer::start();
            let dep_worlds: Vec<_> = deps
                .iter()
                .map(|d| vesper::check::World::collect(&d.module).0)
                .collect();
            // Each dependency's package name (what Rust's `add_dependency`
            // dedups on) and its whole identity (`name@version`, what its
            // messages print).
            let dep_ids: Vec<String> = deps.iter().map(|d| d.identity.to_string()).collect();
            let dep_refs: Vec<_> = deps
                .iter()
                .zip(&dep_worlds)
                .zip(&dep_ids)
                .map(|((d, w), id)| (d.identity.name.as_str(), id.as_str(), &d.module, w))
                .collect();
            let input = pact_words::encode_deps_input(
                module,
                base_world,
                &identity.to_string(),
                &dep_refs,
            )?;
            let timer = timer.lap(&COST.encode);
            let words = pact_binding::pact_deps_words(&input);
            let timer = timer.lap(&COST.vesper);
            let diagnostics = pact_words::decode_declarations(&words);
            timer.lap(&COST.decode);
            PACT_CALLS.fetch_add(1, Relaxed);
            Some(diagnostics)
        },
    };

/// Full coherence-gated Pact session from `modules/pact` (SH2 lane 13 part 2b).
const VESPER_PACT_SESSION: vesper::pact_frontend::PactSessionTable =
    vesper::pact_frontend::PactSessionTable {
        open: |module, base_world, identity, solver, fingerprint| {
            let timer = stats::Timer::start();
            let (max_depth, max_steps, max_fix) = solver.map_or((48, 100_000, 16), |s| {
                (
                    s.max_depth as u64,
                    s.max_steps,
                    s.max_fixpoint_iterations as u64,
                )
            });
            let prelude = vesper::pact_frontend::prelude_world();
            let input = pact_words::session_input(
                module,
                base_world,
                &prelude,
                &identity.name,
                &identity.version,
                max_depth,
                max_steps,
                max_fix,
                fingerprint,
            )?;
            let timer = timer.lap(&COST.encode);
            let sess = pact_binding::pact_session_open(&input);
            let timer = timer.lap(&COST.vesper);
            let out = if pact_binding::pact_session_is_ok(&sess) {
                let rev_words = pact_binding::pact_session_revision(&sess);
                let revision = check_words::Words::new(&rev_words).text();
                vesper::pact_frontend::PactOpenResult::Open(Box::new(VesperPactSessionImpl {
                    inner: std::sync::Mutex::new(sess),
                    revision,
                }))
            } else {
                let diag_words = pact_binding::pact_session_take_diagnostics(&sess);
                let diagnostics = pact_words::decode_session_words(&diag_words);
                vesper::pact_frontend::PactOpenResult::Diagnostics(diagnostics)
            };
            timer.lap(&COST.decode);
            PACT_CALLS.fetch_add(1, Relaxed);
            Some(out)
        },
    };

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    pass("declaration_pass", "pact-declarations", "#2517"),
    open(
        "pact_session",
        "no flip-gate workload: the session also answers every later solve and mono check, so it needs an end-to-end check_package timing (#2684)",
    ),
    open(
        "builtin_wanted",
        "flip gate FAIL ~4.2x: the swap encodes the whole module to answer one bit (scripts/ci/flip-gate/pact-builtin-wanted.json, #2684)",
    ),
    open(
        "deps_pass",
        "0 diffs through the swap point's encoding (fixed in #2684), ABBA ~0.3x; full gate pending (scripts/ci/flip-gate/pact-deps.json)",
    ),
];

pub(super) fn register(_r: &mut Registry) {
    if on("declaration_pass") {
        must(vesper::pact_frontend::install_declaration_pass(
            VESPER_DECLARATION_PASS,
        ));
    }
    if on("pact_session") {
        must(vesper::pact_frontend::install_pact_session(
            VESPER_PACT_SESSION,
        ));
    }
    if on("builtin_wanted") {
        must(vesper::pact_frontend::install_builtin_wanted(
            VESPER_BUILTIN_WANTED,
        ));
    }
    if on("deps_pass") {
        must(vesper::pact_frontend::install_deps_pass(VESPER_DEPS_PASS));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: pact answered {} calls from Vesper",
        PACT_CALLS.load(Relaxed),
    );
    COST.report();
}
