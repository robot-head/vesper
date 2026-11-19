//! Property/concolic execution, `SimPulse` + DPOR fault-profile simulation,
//! and `.vchrono` failure capsule emission for Vesper Anvil
//! (`docs/rfcs/vesper-anvil.md`, Phase 4).

use crate::{
    ast::{Pass, Type},
    check::{self, World},
    parser,
    runtime::{Runtime, Value},
};
use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
};
use vesper_chrono::{
    Bundle, BundleKey, FlightRecorder, RecorderConfig, RedactionPolicy,
};
use vesper_crucible::{
    Arbitrary, ChoiceTape, ExploreConfig, Footprint, Invalid, PropError, ShrinkConfig, SimFiber,
    SimStep, Source, SplitMix64, StepCx, explore_dpor, format_schedule, shrink,
};

const DEFAULT_PROPERTY_SEED: u64 = 0x00c0_ffee;
const DEFAULT_SIM_FUEL: u64 = 100_000;

/// Supported scalar/boolean parameter carriers for Crucible property testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PropCarrier {
    I64,
    I32,
    I16,
    I8,
    U64,
    U32,
    U16,
    U8,
    Bool,
}

impl PropCarrier {
    fn of(ty: &Type) -> Option<Self> {
        match ty.name() {
            "i64" => Some(Self::I64),
            "i32" => Some(Self::I32),
            "i16" => Some(Self::I16),
            "i8" => Some(Self::I8),
            "u64" => Some(Self::U64),
            "u32" => Some(Self::U32),
            "u16" => Some(Self::U16),
            "u8" => Some(Self::U8),
            "bool" => Some(Self::Bool),
            _ => None,
        }
    }

    fn draw(self, src: &mut Source) -> Result<Value, Invalid> {
        Ok(match self {
            Self::I64 => Value::Int(i64::arbitrary(src)?),
            Self::I32 => Value::SInt(i64::from(i64::arbitrary(src)? as i32), 32),
            Self::I16 => Value::SInt(i64::from(i64::arbitrary(src)? as i16), 16),
            Self::I8 => Value::SInt(i64::from(i64::arbitrary(src)? as i8), 8),
            Self::U64 => Value::UInt(src.draw_u64()?, 64),
            Self::U32 => Value::UInt(u64::from(src.draw_u32()?), 32),
            Self::U16 => Value::UInt(u64::from(u16::arbitrary(src)?), 16),
            Self::U8 => Value::UInt(u64::from(src.draw_u8()?), 8),
            Self::Bool => Value::Bool(src.draw_bool()?),
        })
    }
}

/// Format a [`Value`] as a Vesper source literal suitable for `@test_case(...)`.
pub fn format_value_literal(value: &Value) -> String {
    match value {
        Value::Int(n) => n.to_string(),
        Value::SInt(n, _) => n.to_string(),
        Value::UInt(n, _) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Float(f) => format!("{f:?}"),
        Value::Float32(f) => format!("{f:?}"),
        Value::String(s) => format!("{s:?}"),
        other => other.to_string(),
    }
}

/// Format a list of argument values as an `@test_case(...)` attribute pin.
pub fn format_test_case_pin(args: &[Value]) -> String {
    let rendered: Vec<String> = args.iter().map(format_value_literal).collect();
    format!("@test_case({})", rendered.join(", "))
}

/// Outcome of a parameterized property test run via [`run_property_test`].
#[derive(Clone, Debug)]
pub struct PropertyOutcome {
    pub passed: bool,
    pub cases_run: usize,
    pub explicit_cases_run: usize,
    pub failure_message: Option<String>,
    pub counterexample: Option<Vec<Value>>,
    pub test_case_pin: Option<String>,
    pub failing_tape: Option<Vec<u64>>,
    pub seed: u64,
}

impl PropertyOutcome {
    fn error(msg: impl Into<String>, seed: u64) -> Self {
        Self {
            passed: false,
            cases_run: 0,
            explicit_cases_run: 0,
            failure_message: Some(msg.into()),
            counterexample: None,
            test_case_pin: None,
            failing_tape: None,
            seed,
        }
    }
}

/// Evaluate a single property invocation. A `bool` return of `false` or any
/// runtime error (such as an unhandled `Fail` or assertion failure) counts as a
/// property failure.
fn eval_property_case(
    runtime: &RefCell<Runtime<'_>>,
    fn_name: &str,
    args: Vec<Value>,
    fuel: u64,
) -> Result<(), String> {
    let mut rt = runtime.borrow_mut();
    rt.fuel = fuel;
    match rt.run(fn_name, args) {
        Ok(Value::Bool(false)) => Err(format!("property '{fn_name}' returned false")),
        Ok(_) => Ok(()),
        Err(msg) => Err(msg),
    }
}

fn stable_failure_kind(msg: &str) -> String {
    let class: String = msg
        .chars()
        .filter(|c| !c.is_ascii_digit())
        .take(96)
        .collect();
    format!("property: {class}")
}

/// Run a parameterized property test:
/// 1. Replays every explicit `@test_case(...)` pin in `explicit_cases` first.
/// 2. Drives `vesper_crucible::{ChoiceTape, Source, shrink}` over the
///    function's scalar/bool parameters for up to `cases` generated inputs.
/// 3. On any generated failure, shrinks the failing `ChoiceTape` using
///    Crucible's shortlex shrinker into a minimal `Vec<Value>` and formatted
///    `@test_case(...)` pin.
pub fn run_property_test(
    world: &World,
    fn_name: &str,
    cases: usize,
    explicit_cases: &[Vec<Value>],
    fuel: u64,
) -> PropertyOutcome {
    let seed = DEFAULT_PROPERTY_SEED;
    let Some(func) = world.functions.get(fn_name) else {
        return PropertyOutcome::error(format!("unknown property test function '{fn_name}'"), seed);
    };
    if !func.generics.is_empty() {
        return PropertyOutcome::error(
            format!("property test '{fn_name}' cannot be generic"),
            seed,
        );
    }
    if func.body.is_none() {
        return PropertyOutcome::error(format!("property test '{fn_name}' has no body"), seed);
    }

    let mut carriers = Vec::with_capacity(func.params.len());
    for param in &func.params {
        if param.pass != Pass::In {
            return PropertyOutcome::error(
                format!(
                    "property test '{fn_name}' parameter '{}' must be passed by value (`in`)",
                    param.name
                ),
                seed,
            );
        }
        let Some(carrier) = PropCarrier::of(&param.ty) else {
            return PropertyOutcome::error(
                format!(
                    "property test '{fn_name}' parameter '{}' has unsupported type {}; supported: i64, i32, i16, i8, u64, u32, u16, u8, bool",
                    param.name, param.ty
                ),
                seed,
            );
        };
        carriers.push(carrier);
    }

    let runtime = RefCell::new(Runtime::new(world));
    let mut explicit_run = 0usize;

    // 1. Replay explicit `@test_case(...)` regression pins first.
    for pin_args in explicit_cases {
        if pin_args.len() != carriers.len() {
            return PropertyOutcome::error(
                format!(
                    "@test_case for '{fn_name}' supplied {} args, expected {}",
                    pin_args.len(),
                    carriers.len()
                ),
                seed,
            );
        }
        explicit_run += 1;
        if let Err(msg) = eval_property_case(&runtime, fn_name, pin_args.clone(), fuel) {
            let pin = format_test_case_pin(pin_args);
            return PropertyOutcome {
                passed: false,
                cases_run: explicit_run,
                explicit_cases_run: explicit_run,
                failure_message: Some(msg),
                counterexample: Some(pin_args.clone()),
                test_case_pin: Some(pin),
                failing_tape: None,
                seed,
            };
        }
    }

    // 2. Drive Crucible ChoiceTape generation over `cases` iterations.
    let mut rng = SplitMix64(seed);
    let mut generated_run = 0usize;
    for _ in 0..cases {
        let case_seed = rng.next_u64();
        let mut src = Source::random(case_seed);
        let mut args = Vec::with_capacity(carriers.len());
        let mut draw_ok = true;
        for &carrier in &carriers {
            match carrier.draw(&mut src) {
                Ok(v) => args.push(v),
                Err(_) => {
                    draw_ok = false;
                    break;
                }
            }
        }
        if !draw_ok {
            continue;
        }
        generated_run += 1;
        if let Err(initial_msg) = eval_property_case(&runtime, fn_name, args.clone(), fuel) {
            if initial_msg.contains("fuel") {
                // Skip fuel-exhausted generated inputs as invalid runs.
                continue;
            }
            let failing_tape: ChoiceTape = src.realized();

            // 3. Shrink the failing ChoiceTape via Crucible's shortlex reducer.
            let mut prop = |s: &mut Source| -> Result<(), PropError> {
                let mut drawn = Vec::with_capacity(carriers.len());
                for &carrier in &carriers {
                    drawn.push(carrier.draw(s)?);
                }
                match eval_property_case(&runtime, fn_name, drawn, fuel) {
                    Ok(()) => Ok(()),
                    Err(msg) if msg.contains("fuel") => Err(PropError::Invalid(Invalid::Rejected)),
                    Err(msg) => Err(PropError::fail(stable_failure_kind(&msg), msg)),
                }
            };

            let (minimal_tape, final_msg) =
                match shrink(&mut prop, &failing_tape, &ShrinkConfig::default()) {
                    Ok(shrunk) => (shrunk.tape, shrunk.failure.detail),
                    Err(_) => (failing_tape, initial_msg),
                };

            // Re-draw minimal concrete arguments from the shrunk ChoiceTape.
            let mut replay_src = Source::replay(&minimal_tape);
            let mut minimal_args = Vec::with_capacity(carriers.len());
            let mut replay_ok = true;
            for &carrier in &carriers {
                match carrier.draw(&mut replay_src) {
                    Ok(v) => minimal_args.push(v),
                    Err(_) => {
                        replay_ok = false;
                        break;
                    }
                }
            }
            let ce = if replay_ok { minimal_args } else { args };
            let pin = format_test_case_pin(&ce);

            return PropertyOutcome {
                passed: false,
                cases_run: explicit_run + generated_run,
                explicit_cases_run: explicit_run,
                failure_message: Some(final_msg),
                counterexample: Some(ce),
                test_case_pin: Some(pin),
                failing_tape: Some(minimal_tape.values),
                seed: case_seed,
            };
        }
    }

    PropertyOutcome {
        passed: true,
        cases_run: explicit_run + generated_run,
        explicit_cases_run: explicit_run,
        failure_message: None,
        counterexample: None,
        test_case_pin: None,
        failing_tape: None,
        seed,
    }
}

// ---------------------------------------------------------------------------
// SimPulse + DPOR Fault-Profile Simulation
// ---------------------------------------------------------------------------

/// Fault injection profiles supported by [`run_simpulse_test`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultProfile {
    None,
    TornWalSector,
    NetPartition,
    ClockSkew,
}

impl FaultProfile {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name.trim() {
            "" | "None" | "none" => Ok(Self::None),
            "TornWalSector" | "torn_wal_sector" => Ok(Self::TornWalSector),
            "NetPartition" | "net_partition" => Ok(Self::NetPartition),
            "ClockSkew" | "clock_skew" => Ok(Self::ClockSkew),
            other => Err(format!(
                "unknown SimPulse fault profile '{other}'; expected None, TornWalSector, NetPartition, or ClockSkew"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::TornWalSector => "TornWalSector",
            Self::NetPartition => "NetPartition",
            Self::ClockSkew => "ClockSkew",
        }
    }

    fn fault_obj_id(self) -> u64 {
        match self {
            Self::None => 0,
            Self::TornWalSector => 1,
            Self::NetPartition => 2,
            Self::ClockSkew => 3,
        }
    }
}

/// Outcome of a deterministic `SimPulse` + DPOR scenario run.
#[derive(Clone, Debug)]
pub struct SimOutcome {
    pub passed: bool,
    pub schedules_run: usize,
    pub fault_profile: String,
    pub failing_seed: Option<u64>,
    pub trace: Vec<String>,
    pub failure_message: Option<String>,
    pub dpor_executions: u64,
    pub dpor_sleep_blocked: u64,
}

struct SimWorld<'a> {
    checked: &'a World,
    fn_name: &'a str,
    profile: FaultProfile,
    seed: u64,
    fault_injected: bool,
    clock_skew_ns: u64,
    steps: Vec<String>,
}

enum AnvilSimFiber {
    Workload { phase: u8, obj: u64 },
    FaultInjector { injected: bool, profile: FaultProfile },
}

impl<'a> SimFiber<SimWorld<'a>> for AnvilSimFiber {
    fn footprint(&self) -> Footprint {
        match self {
            Self::Workload { obj, .. } => Footprint::none().read(*obj).write(*obj),
            Self::FaultInjector { profile, .. } => {
                let obj = profile.fault_obj_id();
                Footprint::none().write(obj)
            }
        }
    }

    fn step(&mut self, world: &mut SimWorld<'a>, cx: &mut StepCx) -> Result<SimStep, PropError> {
        match self {
            Self::Workload { phase, obj } => {
                cx.read(*obj);
                if *phase == 0 {
                    *phase = 1;
                    world.steps.push(format!(
                        "t={}ns fiber={} prepare(fault_injected={})",
                        cx.now_ns(),
                        cx.task(),
                        world.fault_injected
                    ));
                    return Ok(SimStep::Yield);
                }
                cx.write(*obj);
                let mut rt = Runtime::new(world.checked);
                rt.fuel = DEFAULT_SIM_FUEL;
                let func = &world.checked.functions[world.fn_name];
                let args = build_sim_args(func, world.seed, world.fault_injected, world.clock_skew_ns);
                world.steps.push(format!(
                    "t={}ns fiber={} call {}(fault={:?}, injected={})",
                    cx.now_ns(),
                    cx.task(),
                    world.fn_name,
                    world.profile,
                    world.fault_injected
                ));
                match rt.run(world.fn_name, args) {
                    Ok(Value::Bool(false)) => Err(PropError::fail(
                        "simpulse_false",
                        format!(
                            "SimPulse test '{}' returned false under {:?} (injected={})",
                            world.fn_name, world.profile, world.fault_injected
                        ),
                    )),
                    Ok(_) => Ok(SimStep::Done),
                    Err(msg) => Err(PropError::fail("simpulse_error", msg)),
                }
            }
            Self::FaultInjector { injected, profile } => {
                let obj = profile.fault_obj_id();
                cx.write(obj);
                if !*injected {
                    *injected = true;
                    match profile {
                        FaultProfile::None => {}
                        FaultProfile::TornWalSector => {
                            world.fault_injected = true;
                            world.steps.push(format!(
                                "t={}ns fiber={} inject TornWalSector",
                                cx.now_ns(),
                                cx.task()
                            ));
                        }
                        FaultProfile::NetPartition => {
                            world.fault_injected = true;
                            world.steps.push(format!(
                                "t={}ns fiber={} inject NetPartition",
                                cx.now_ns(),
                                cx.task()
                            ));
                        }
                        FaultProfile::ClockSkew => {
                            world.fault_injected = true;
                            world.clock_skew_ns = 1_000_000 + (world.seed % 500_000);
                            world.steps.push(format!(
                                "t={}ns fiber={} inject ClockSkew(+{}ns)",
                                cx.now_ns(),
                                cx.task(),
                                world.clock_skew_ns
                            ));
                            return Ok(SimStep::Sleep { ns: 10 });
                        }
                    }
                }
                Ok(SimStep::Done)
            }
        }
    }
}

fn build_sim_args(
    func: &crate::ast::Function,
    seed: u64,
    fault_injected: bool,
    clock_skew_ns: u64,
) -> Vec<Value> {
    let mut args = Vec::with_capacity(func.params.len());
    for (idx, p) in func.params.iter().enumerate() {
        let val = match p.ty.name() {
            "bool" => Value::Bool(fault_injected),
            "u64" => {
                if idx == 0 {
                    Value::UInt(seed, 64)
                } else {
                    Value::UInt(clock_skew_ns, 64)
                }
            }
            "i64" => {
                if idx == 0 {
                    Value::Int(seed as i64)
                } else {
                    Value::Int(clock_skew_ns as i64)
                }
            }
            _ => Value::Tuple(vec![]),
        };
        args.push(val);
    }
    args
}

/// Run deterministic `SimPulse` + DPOR scenarios across `schedules` seeds with
/// the requested `fault_profile` (`None`, `TornWalSector`, `NetPartition`, or
/// `ClockSkew`), reporting any failing seed and schedule trace.
pub fn run_simpulse_test(
    source: &str,
    fn_name: &str,
    schedules: usize,
    fault_profile: &str,
    seed: u64,
) -> SimOutcome {
    let profile = match FaultProfile::parse(fault_profile) {
        Ok(p) => p,
        Err(msg) => {
            return SimOutcome {
                passed: false,
                schedules_run: 0,
                fault_profile: fault_profile.to_string(),
                failing_seed: Some(seed),
                trace: Vec::new(),
                failure_message: Some(msg),
                dpor_executions: 0,
                dpor_sleep_blocked: 0,
            };
        }
    };

    let parsed = parser::parse(source);
    if let Some(d) = parsed.diagnostics.first() {
        return SimOutcome {
            passed: false,
            schedules_run: 0,
            fault_profile: profile.as_str().to_string(),
            failing_seed: Some(seed),
            trace: Vec::new(),
            failure_message: Some(format!("parse error {}: {}", d.code, d.message)),
            dpor_executions: 0,
            dpor_sleep_blocked: 0,
        };
    }
    let checked = check::check(&parsed.module);
    if let Some(d) = checked.diagnostics.first() {
        return SimOutcome {
            passed: false,
            schedules_run: 0,
            fault_profile: profile.as_str().to_string(),
            failing_seed: Some(seed),
            trace: Vec::new(),
            failure_message: Some(format!("check error {}: {}", d.code, d.message)),
            dpor_executions: 0,
            dpor_sleep_blocked: 0,
        };
    }
    run_simpulse_test_on_world(&checked.world, fn_name, schedules, fault_profile, seed)
}

/// Run a deterministic `SimPulse` + DPOR schedule suite directly against an already-checked `World`.
pub fn run_simpulse_test_on_world(
    world: &World,
    fn_name: &str,
    schedules: usize,
    fault_profile: &str,
    seed: u64,
) -> SimOutcome {
    let profile = match FaultProfile::parse(fault_profile) {
        Ok(p) => p,
        Err(msg) => {
            return SimOutcome {
                passed: false,
                schedules_run: 0,
                fault_profile: fault_profile.to_string(),
                failing_seed: Some(seed),
                trace: Vec::new(),
                failure_message: Some(msg),
                dpor_executions: 0,
                dpor_sleep_blocked: 0,
            };
        }
    };
    if !world.functions.contains_key(fn_name) {
        return SimOutcome {
            passed: false,
            schedules_run: 0,
            fault_profile: profile.as_str().to_string(),
            failing_seed: Some(seed),
            trace: Vec::new(),
            failure_message: Some(format!("unknown SimPulse test function '{fn_name}'")),
            dpor_executions: 0,
            dpor_sleep_blocked: 0,
        };
    }

    let rounds = schedules.max(1);
    let mut rng = SplitMix64(seed);
    let mut total_execs = 0u64;
    let mut total_sleep_blocked = 0u64;
    let mut completed = 0usize;

    for round in 0..rounds {
        let round_seed = if round == 0 { seed } else { rng.next_u64() };
        let cfg = ExploreConfig {
            seed: round_seed,
            max_depth: 256,
            max_executions: 64,
            max_fibers: 8,
            preemption_bound: None,
        };
        let obj = profile.fault_obj_id();
        let exploration = explore_dpor(
            &cfg,
            || {
                let sim_world = SimWorld {
                    checked: world,
                    fn_name,
                    profile,
                    seed: round_seed,
                    fault_injected: false,
                    clock_skew_ns: 0,
                    steps: Vec::new(),
                };
                let mut fibers = vec![AnvilSimFiber::Workload { phase: 0, obj }];
                if profile != FaultProfile::None {
                    fibers.push(AnvilSimFiber::FaultInjector {
                        injected: false,
                        profile,
                    });
                }
                (sim_world, fibers)
            },
            |w| Ok(w.steps.clone()),
        );

        match exploration {
            Ok(exp) => {
                completed += 1;
                total_execs += exp.executions;
                total_sleep_blocked += exp.sleep_blocked;
                if let Some((failure, sched)) = exp.first_failure() {
                    let trace = vec![
                        format!("fault_profile={}", profile.as_str()),
                        format!("seed={round_seed:#x}"),
                        format!("schedule={}", format_schedule(sched)),
                        format!("failure={failure}"),
                    ];
                    return SimOutcome {
                        passed: false,
                        schedules_run: completed,
                        fault_profile: profile.as_str().to_string(),
                        failing_seed: Some(round_seed),
                        trace,
                        failure_message: Some(failure.detail.clone()),
                        dpor_executions: total_execs,
                        dpor_sleep_blocked: total_sleep_blocked,
                    };
                }
            }
            Err(err) => {
                return SimOutcome {
                    passed: false,
                    schedules_run: completed,
                    fault_profile: profile.as_str().to_string(),
                    failing_seed: Some(round_seed),
                    trace: vec![format!("dpor_error={err}")],
                    failure_message: Some(format!("SimPulse DPOR error: {err}")),
                    dpor_executions: total_execs,
                    dpor_sleep_blocked: total_sleep_blocked,
                };
            }
        }
    }

    SimOutcome {
        passed: true,
        schedules_run: completed,
        fault_profile: profile.as_str().to_string(),
        failing_seed: None,
        trace: Vec::new(),
        failure_message: None,
        dpor_executions: total_execs,
        dpor_sleep_blocked: total_sleep_blocked,
    }
}

// ---------------------------------------------------------------------------
// `.vchrono` Failure Capsule Bridge
// ---------------------------------------------------------------------------

const ANVIL_EFFECT_ID: u16 = 1;
const OP_SEED: u16 = 1;
const OP_TAPE_CHUNK: u16 = 2;
const OP_SUMMARY: u16 = 3;

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "anvil_test".to_string()
    } else {
        cleaned
    }
}

/// Derive a deterministic 32-byte `BundleKey` and its hex representation for
/// an Anvil failure capsule.
pub fn derive_capsule_key(test_name: &str, seed: u64) -> (BundleKey, String) {
    let material = format!("vesper-anvil-capsule/1:{test_name}:{seed}");
    let hash = blake3::hash(material.as_bytes());
    let key = BundleKey::new(*hash.as_bytes());
    let hex = hash.to_hex().to_string();
    (key, hex)
}

/// Record a failing test's seed, choice tape, and summary into a sealed
/// `vesper_chrono` `.vchrono` failure capsule at `<out_dir>/<test_name>.vchrono`
/// (with a companion `.vchrono.key` file) and print the `vesper replay` command.
pub fn write_vchrono_capsule(
    out_dir: &Path,
    test_name: &str,
    seed: u64,
    tape_choices: &[u64],
    summary: &str,
) -> Result<PathBuf, String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("failed to create capsule dir {}: {e}", out_dir.display()))?;

    let config = RecorderConfig::small();
    let mut recorder =
        FlightRecorder::new(config).map_err(|e| format!("FlightRecorder init failed: {e}"))?;

    // Record seed.
    recorder
        .record_effect(0, 1, ANVIL_EFFECT_ID, OP_SEED, &seed.to_le_bytes())
        .map_err(|e| format!("record seed failed: {e}"))?;

    // Record choice tape in chunks fitting `config.max_payload`.
    let words_per_chunk = (config.max_payload / 8).max(1);
    let mut time_ns = 2u64;
    for chunk in tape_choices.chunks(words_per_chunk) {
        let mut bytes = Vec::with_capacity(chunk.len() * 8);
        for &w in chunk {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        recorder
            .record_effect(0, time_ns, ANVIL_EFFECT_ID, OP_TAPE_CHUNK, &bytes)
            .map_err(|e| format!("record tape chunk failed: {e}"))?;
        time_ns += 1;
    }

    // Record human-readable summary (bounded to max_payload).
    let summary_bytes = summary.as_bytes();
    let bounded_len = summary_bytes.len().min(config.max_payload);
    recorder
        .record_effect(
            0,
            time_ns,
            ANVIL_EFFECT_ID,
            OP_SUMMARY,
            &summary_bytes[..bounded_len],
        )
        .map_err(|e| format!("record summary failed: {e}"))?;

    let metadata = serde_json::json!({
        "schema": "vesper.anvil.capsule/1",
        "test": test_name,
        "seed": seed,
        "tape_len": tape_choices.len(),
        "summary": summary,
    })
    .to_string();

    let (key, key_hex) = derive_capsule_key(test_name, seed);
    let policy = RedactionPolicy::new();
    let bundle = Bundle::capture(&recorder, &policy, metadata.as_bytes());
    let sealed = bundle
        .seal(&key)
        .map_err(|e| format!("Bundle::seal failed: {e}"))?;

    let safe_name = sanitize_filename(test_name);
    let capsule_path = out_dir.join(format!("{safe_name}.vchrono"));
    let key_path = out_dir.join(format!("{safe_name}.vchrono.key"));

    fs::write(&capsule_path, &sealed)
        .map_err(|e| format!("failed to write {}: {e}", capsule_path.display()))?;
    fs::write(&key_path, format!("{key_hex}\n"))
        .map_err(|e| format!("failed to write {}: {e}", key_path.display()))?;

    eprintln!(
        "Replay capsule: vesper replay {} --key-file {} --timeline",
        capsule_path.display(),
        key_path.display()
    );

    Ok(capsule_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesper_chrono::{BundleLimits, ReplaySession};

    fn check_world(src: &str) -> World {
        let parsed = parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        checked.world
    }

    #[test]
    fn property_test_passes_for_valid_invariant() {
        let world = check_world(
            r#"
fn prop_comm(a: i64, b: i64) -> bool {
    (a == b) == (b == a) && ((a < b) == (b > a))
}
"#,
        );
        let outcome = run_property_test(&world, "prop_comm", 64, &[], 50_000);
        assert!(outcome.passed, "unexpected failure: {:?}", outcome.failure_message);
        assert_eq!(outcome.cases_run, 64);
        assert!(outcome.counterexample.is_none());
    }

    #[test]
    fn property_test_replays_explicit_pin_first() {
        let world = check_world(
            r#"
fn prop_nonzero(x: i64) -> bool {
    x != 7
}
"#,
        );
        let pins = vec![vec![Value::Int(7)]];
        let outcome = run_property_test(&world, "prop_nonzero", 100, &pins, 50_000);
        assert!(!outcome.passed);
        assert_eq!(outcome.explicit_cases_run, 1);
        assert_eq!(outcome.cases_run, 1);
        assert_eq!(outcome.test_case_pin.as_deref(), Some("@test_case(7)"));
    }

    #[test]
    fn property_test_shrinks_counterexample_and_formats_pin() {
        let world = check_world(
            r#"
fn prop_small(x: u64) -> bool {
    x < 10
}
"#,
        );
        let outcome = run_property_test(&world, "prop_small", 256, &[], 50_000);
        assert!(!outcome.passed, "expected counterexample for x >= 10");
        assert_eq!(outcome.test_case_pin.as_deref(), Some("@test_case(10)"));
        assert!(outcome.failing_tape.is_some());
    }

    #[test]
    fn simpulse_dpor_passes_and_detects_fault_profile_failure() {
        let passing_src = r#"
fn sim_ok(injected: bool) -> bool {
    injected || !injected
}
"#;
        let ok = run_simpulse_test(passing_src, "sim_ok", 4, "TornWalSector", 42);
        assert!(ok.passed, "unexpected sim failure: {:?}", ok.failure_message);
        assert_eq!(ok.schedules_run, 4);
        assert!(ok.dpor_executions >= 4);

        let failing_src = r#"
fn sim_breaks_on_fault(injected: bool) -> bool {
    !injected
}
"#;
        let bad = run_simpulse_test(failing_src, "sim_breaks_on_fault", 4, "NetPartition", 99);
        assert!(!bad.passed);
        assert_eq!(bad.failing_seed, Some(99));
        assert!(!bad.trace.is_empty());

        let unknown = run_simpulse_test(passing_src, "sim_ok", 1, "BogusProfile", 1);
        assert!(!unknown.passed);
        assert!(unknown.failure_message.unwrap().contains("unknown SimPulse fault profile"));
    }

    #[test]
    fn vchrono_capsule_writes_and_opens_with_replay_session() {
        let tmp = std::env::temp_dir().join(format!(
            "vesper_anvil_capsule_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);

        let choices = vec![10u64, 0, 42];
        let capsule_path = write_vchrono_capsule(
            &tmp,
            "prop_small",
            0xdead_beef,
            &choices,
            "counterexample: @test_case(10)",
        )
        .expect("write capsule");

        assert!(capsule_path.exists());
        let bytes = fs::read(&capsule_path).expect("read capsule bytes");
        let (key, _) = derive_capsule_key("prop_small", 0xdead_beef);

        let bundle = Bundle::open(&bytes, &key, &BundleLimits::default())
            .expect("authenticate and open capsule");
        assert_eq!(bundle.records.len(), 3);
        let meta = String::from_utf8(bundle.metadata.clone()).expect("utf8 metadata");
        assert!(meta.contains("prop_small"));
        assert!(meta.contains("@test_case(10)"));

        let session = ReplaySession::open_bundle(&bytes, &key, &BundleLimits::default())
            .expect("open replay session");
        assert_eq!(session.bundle().records.len(), 3);
        let _ = session.close();

        let _ = fs::remove_dir_all(&tmp);
    }
}
