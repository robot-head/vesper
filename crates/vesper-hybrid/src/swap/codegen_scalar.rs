// Scalars, locals and control flow (SH4 lane 6, #1280), from
// `modules/codegen_scalar`, through the `scalar` hook; whole scalar
// function bodies (native walk part 6a, #2094) through the `body` hook.

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

/// Function bodies the Vesper module emitted, and left to Rust (unplanned).
static BODY_CALLS: AtomicU64 = AtomicU64::new(0);
static BODY_DECLINED: AtomicU64 = AtomicU64::new(0);

/// `body`'s answer to words that are not a body plan.
const BODY_ERROR: u64 = 2;

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

/// One program's body plans: the plan words of each function Vesper
/// emits whole, by name.
struct BodyPlan {
    key: ((usize, usize, usize), bool),
    bodies: std::collections::HashMap<String, Vec<u64>>,
}

/// The body plans of one compilation (see [`ScalarPlans`]).
#[derive(Default)]
struct BodyPlans(Vec<BodyPlan>);

/// The body plan of `program` from `modules/codegen_scalar`: per function,
/// `[function, length, words...]`. A program the Place IR words cannot
/// encode has no bodies, so it stays on Rust.
fn body_plan(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    optimize: bool,
) -> Result<BodyPlan, String> {
    let mut bodies = std::collections::HashMap::new();
    if let Some((input, _)) = ir_words::program_input(world, program) {
        let names: Vec<&String> = program.functions.keys().collect();
        let words = codegen_scalar_binding::body_plan(&input);
        let mut rest = &words[..];
        while let [function, n, tail @ ..] = rest {
            let n = *n as usize;
            let (Some(name), Some(body)) = (names.get(*function as usize), tail.get(..n)) else {
                return Err(format!(
                    "codegen (body plan): a body of {n} words at {function}"
                ));
            };
            bodies.insert((*name).clone(), body.to_vec());
            rest = &tail[n..];
        }
    }
    Ok(BodyPlan {
        key: (program_key(program), optimize),
        bodies,
    })
}

/// The `body` hook of `vesper::codegen_swap` from `modules/codegen_scalar`
/// (#2094): a function the walk lends whole (a plain scalar frame) is
/// emitted from its body plan, with the walk's status flag, source
/// locations flag and parameter variables. Unplanned functions stay on
/// Rust.
fn vesper_body(lent: &mut vesper::codegen_swap::Lent<'_, '_>) -> Result<bool, String> {
    if lent.op != vesper::codegen_swap::body::FUNCTION {
        return Ok(false);
    }
    let key = (program_key(lent.program), lent.optimize);
    let words = vesper::swap_input::with(lent.world, BodyPlans::default, |plans| {
        let at = match plans.0.iter().position(|p| p.key == key) {
            Some(at) => at,
            None => {
                plans
                    .0
                    .push(body_plan(lent.world, lent.program, lent.optimize)?);
                plans.0.len() - 1
            }
        };
        Ok::<_, String>(plans.0[at].bodies.get(lent.site.function).cloned())
    })
    .transpose()?
    .flatten();
    let Some(words) = words else {
        BODY_DECLINED.fetch_add(1, Relaxed);
        return Ok(false);
    };
    // `[status, source locations, parameter variables...]` from the walk.
    let [status, locations, vars @ ..] = lent.args else {
        return Err("codegen (body): a request without its flags".into());
    };
    let mut args = vec![
        u64::from(lent.optimize),
        *status,
        *locations,
        vars.len() as u64,
    ];
    args.extend_from_slice(vars);
    args.extend(words);
    let kind = if lent.jit {
        vesper_cranelift_sys::ModuleKind::Jit
    } else {
        vesper_cranelift_sys::ModuleKind::Object
    };
    let (answer, crossing) = vesper_cranelift_sys::lend_as(
        kind,
        lent.builder,
        &mut *lent.module,
        vesper::codegen_swap::body::FUNCTION,
        &args,
        codegen_scalar_binding::body,
    );
    let function = lent.site.function;
    if let Some(e) = crossing.error {
        return Err(format!("codegen (body of {function}): {e}"));
    }
    if crossing.unreturned {
        return Err(format!("codegen (body of {function}) kept a lent handle"));
    }
    match answer {
        1 => {
            BODY_CALLS.fetch_add(1, Relaxed);
            Ok(true)
        }
        BODY_ERROR => Err(format!(
            "codegen (body of {function}): the plan's words do not read"
        )),
        _ => {
            BODY_DECLINED.fetch_add(1, Relaxed);
            Ok(false)
        }
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open("scalar", "0 diffs; no flip-gate configuration"),
    open("body", "0 diffs; no flip-gate configuration"),
];

pub(super) fn register(r: &mut Registry) {
    // `VESPER_HYBRID_SCALAR=0` leaves scalars to Rust (the scoreboard's
    // before side).
    if on("scalar") && !env_off("VESPER_HYBRID_SCALAR") {
        r.codegen.scalar = Some(vesper_scalar);
    }
    // `VESPER_HYBRID_BODY=0` leaves whole function bodies to the walk.
    if on("body") && !env_off("VESPER_HYBRID_BODY") {
        r.codegen.body = Some(vesper_body);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: codegen emitted {} scalar requests from Vesper ({} left to Rust)",
        SCALAR_CALLS.load(Relaxed),
        SCALAR_DECLINED.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: codegen emitted {} function bodies from Vesper ({} left to Rust)",
        BODY_CALLS.load(Relaxed),
        BODY_DECLINED.load(Relaxed),
    );
}
