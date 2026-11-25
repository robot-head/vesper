// Scalars, locals and control flow (SH4 lane 6, #1280), from
// `modules/codegen_scalar`, through the `scalar` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/codegen_scalar.rs"]
mod codegen_scalar_binding;

use super::{Registry, Swap, env_off, ir_words, on, open};

/// `scalar` requests (scalars, locals, control flow) the Vesper module
/// emitted, and left to Rust (unplanned, or declined by the hook).
static SCALAR_CALLS: AtomicU64 = AtomicU64::new(0);
static SCALAR_DECLINED: AtomicU64 = AtomicU64::new(0);

/// `scalar_hook`'s request numbers past the plan's (1-10) for the typed
/// requests of `codegen_swap::scalar` (@vesper/codegen-scalar-emit).
fn scalar_request(op: u32) -> Option<u32> {
    use vesper::codegen_swap::scalar as s;
    Some(match op {
        s::OVERFLOW => 11,
        s::DIVISOR => 12,
        s::NEGATE => 13,
        s::SHIFT_RANGE => 14,
        s::NARROW => 15,
        s::READ_LOCAL => 16,
        s::BIND_LOCAL => 17,
        s::WRITE_LOCAL => 18,
        _ => return None,
    })
}

/// One program's scalar plan: per `(function, block, instruction)`, the
/// `scalar_hook` request and its two immediates.
struct ScalarPlan {
    key: ((usize, usize, usize), bool),
    sites: std::collections::HashMap<(String, u32, u32), (u32, u64, u64)>,
}

/// The plans of one compilation, kept in its encode-once scope
/// (`vesper::swap_input`), so they are dropped when the compilation ends
/// and a later compilation's program at a reused address is planned anew.
#[derive(Default)]
struct ScalarPlans(Vec<ScalarPlan>);

/// Identifies a lent program within one compilation: its address,
/// function count and instruction count.
fn program_key(p: &vesper::ir::Program) -> (usize, usize, usize) {
    let insts = p
        .functions
        .values()
        .flat_map(|f| f.blocks.iter())
        .map(|b| b.instructions.len())
        .sum();
    (std::ptr::from_ref(p) as usize, p.functions.len(), insts)
}

/// The plan of `program` from `modules/codegen_scalar`. A program the
/// Place IR words cannot encode has no sites, so it stays on Rust.
fn scalar_plan(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    optimize: bool,
) -> ScalarPlan {
    const RECORD: usize = 6;
    let mut sites = std::collections::HashMap::new();
    if let Some((input, _)) = ir_words::program_input(world, program) {
        let names: Vec<&String> = program.functions.keys().collect();
        let words = codegen_scalar_binding::plan(&input);
        for r in words.as_chunks::<RECORD>().0 {
            let Some(name) = names.get(r[0] as usize) else {
                continue;
            };
            sites.insert(
                ((*name).clone(), r[1] as u32, r[2] as u32),
                (r[3] as u32, r[4], r[5]),
            );
        }
    }
    ScalarPlan {
        key: (program_key(program), optimize),
        sites,
    }
}

/// The `scalar` hook of `vesper::codegen_swap` from
/// `modules/codegen_scalar` (#1280). A graph-walk `INSTRUCTION` request
/// emits the planned request of its instruction; the typed requests of
/// `codegen_swap::scalar` (raising arms' tests, plain locals) go to the
/// hook as they come. Unplanned instructions, and operands Rust has no
/// value for, stay on Rust.
fn vesper_scalar(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    let (request, args) = if lent.op == vesper::codegen_swap::INSTRUCTION {
        // Outside a compilation's scope (`VESPER_HYBRID_SCOPES=0`, or a
        // re-entrant call) there is no plan, so scalars stay on Rust.
        let key = (program_key(lent.program), lent.optimize);
        let planned = vesper::swap_input::with(lent.world, ScalarPlans::default, |plans| {
            let at = match plans.0.iter().position(|p| p.key == key) {
                Some(at) => at,
                None => {
                    plans
                        .0
                        .push(scalar_plan(lent.world, lent.program, lent.optimize));
                    plans.0.len() - 1
                }
            };
            plans.0[at]
                .sites
                .get(&(
                    lent.site.function.to_string(),
                    lent.site.block,
                    lent.site.inst,
                ))
                .copied()
        })
        .flatten();
        let Some((request, a, b)) = planned else {
            SCALAR_DECLINED.fetch_add(1, Relaxed);
            return Ok(false);
        };
        if lent.args.contains(&vesper::codegen_swap::NO_VALUE) {
            SCALAR_DECLINED.fetch_add(1, Relaxed);
            return Ok(false);
        }
        let mut args = vec![a, b, u64::from(lent.optimize)];
        args.extend_from_slice(lent.args);
        (request, args)
    } else {
        let Some(request) = scalar_request(lent.op) else {
            return Ok(false);
        };
        let mut args = lent.args.to_vec();
        if lent.op == vesper::codegen_swap::scalar::READ_LOCAL {
            args.push(u64::from(lent.optimize));
        }
        (request, args)
    };
    let kind = if lent.jit {
        vesper_cranelift_sys::ModuleKind::Jit
    } else {
        vesper_cranelift_sys::ModuleKind::Object
    };
    let (answer, crossing) = vesper_cranelift_sys::lend_as(
        kind,
        lent.builder,
        &mut *lent.module,
        request,
        &args,
        codegen_scalar_binding::scalar,
    );
    if let Some(e) = crossing.error {
        return Err(format!("codegen (scalar request {request}): {e}"));
    }
    if crossing.unreturned {
        return Err(format!(
            "codegen (scalar request {request}) kept a lent handle"
        ));
    }
    if answer == 1 {
        SCALAR_CALLS.fetch_add(1, Relaxed);
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        SCALAR_DECLINED.fetch_add(1, Relaxed);
        Ok(false)
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("scalar", "0 diffs; no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    // `VESPER_HYBRID_SCALAR=0` leaves scalars to Rust (the scoreboard's
    // before side).
    if on("scalar") && !env_off("VESPER_HYBRID_SCALAR") {
        r.codegen.scalar = Some(vesper_scalar);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted {} scalar requests from Vesper ({} left to Rust)",
        SCALAR_CALLS.load(Relaxed),
        SCALAR_DECLINED.load(Relaxed),
    );
}
