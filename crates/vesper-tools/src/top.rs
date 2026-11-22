//! Zero-instrumentation Pulse fiber telemetry and live linear obligation inspector (§14.6).
//!
//! Supports:
//! - `vesper top --trace <telemetry.json>` (or `VESPER_PULSE_TRACE=1` / `VESPER_PULSE_TRACE_PATH`):
//!   analyzes TelemetryPulse fiber snapshots, computes run/idle histograms and
//!   wake-to-poll scheduling latency percentiles, inspects live `Mode::Linear`
//!   obligations, and detects wait-for deadlock cycles.
//! - `vesper top --chrono <bundle.vchrono> (--key <hex> | --key-file <path>)`:
//!   authenticates a Chrono flight-recorder bundle and extracts fiber timing,
//!   active effects, open/leaked linear obligations, and wait-for cycles.
//! - `vesper top <SOURCE.ves> [--entry <fn>] [--fuel <steps>]`:
//!   runs a checked Vesper module and emits TelemetryPulse metrics for the run.
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};
use vesper::{
    ast::Mode,
    check, parser,
    runtime::{
        Runtime,
        resource_trace::{ResourceTraceConfig, ResourceTraceLimits},
    },
};
use vesper_chrono::{BundleKey, BundleLimits, HopKind, ReplaySession, simpulse::TraceEvent};

const MAX_TRACE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

const HELP: &str = "Usage:
  vesper top --trace <pulse-trace.json> [options]
  vesper top --chrono <bundle.vchrono> (--key <hex> | --key-file <path>) [options]
  vesper top <SOURCE.ves> [--entry <fn>] [--fuel <steps>] [options]

Inspect live/recorded Pulse fiber run/idle histograms, wake-to-poll latency,
active lexical effect stacks, live Mode::Linear obligations, and wait-for cycles.

Options:
  --trace <path>       Read TelemetryPulse JSON trace (also VESPER_PULSE_TRACE_PATH)
  --chrono <path>      Authenticate and summarize a Chrono .vchrono bundle
  --key <hex>          32-byte bundle key as 64 hex digits (for --chrono)
  --key-file <path>    File holding bundle key (32 raw bytes or 64 hex digits)
  --entry <name>       Entry function when running a .ves module (default: main)
  --fuel <steps>       Evaluation fuel limit for .ves module execution
  --write-trace <path> Atomically write captured TelemetryPulse snapshot JSON
  --fail-on-cycle      Exit non-zero when a wait-for deadlock cycle is detected
  --no-banner          Omit the Appendix B.4 header banner in human output
  --json               Emit structured machine-readable JSON telemetry
  -h, --help           Show this help text
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiberStatus {
    Running,
    Runnable,
    Parked,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationEntry {
    pub obligation_id: u64,
    pub kind: String,
    #[serde(default)]
    pub parked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on_fiber: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on_obligation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiberTelemetry {
    pub fiber_id: u64,
    #[serde(default)]
    pub worker_core: u16,
    pub status: FiberStatus,
    pub run_ns: u64,
    pub idle_ns: u64,
    #[serde(default)]
    pub wake_to_poll_ns: Vec<u64>,
    #[serde(default)]
    pub effect_stack: Vec<String>,
    #[serde(default)]
    pub obligations: Vec<ObligationEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on_fiber: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySnapshot {
    pub schema_version: u32,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub observer_truncated: bool,
    pub fibers: Vec<FiberTelemetry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LatencyStats {
    samples: usize,
    min_ns: u64,
    p50_ns: u64,
    p95_ns: u64,
    max_ns: u64,
    buckets: Vec<(String, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WaitEdge {
    from_fiber: u64,
    to_fiber: u64,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TelemetryReport {
    schema_version: u32,
    source: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    observer_truncated: bool,
    total_fibers: usize,
    parked_fibers: usize,
    total_run_ns: u64,
    total_idle_ns: u64,
    total_live_obligations: usize,
    wake_to_poll: LatencyStats,
    wait_edges: Vec<WaitEdge>,
    deadlock_cycles: Vec<Vec<u64>>,
    fibers: Vec<FiberSummary>,
    live_obligations: Vec<ObligationReportRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FiberSummary {
    fiber_id: u64,
    worker_core: u16,
    status: FiberStatus,
    run_ns: u64,
    idle_ns: u64,
    run_percent_tenths: u64,
    p95_wake_to_poll_ns: u64,
    effect_stack: Vec<String>,
    live_linear_obligations: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ObligationReportRow {
    obligation_id: u64,
    kind: String,
    owner_fiber_id: u64,
    parked: bool,
    waiting_on_fiber: Option<u64>,
    waiting_on_obligation: Option<u64>,
}

fn read_bounded(path: &PathBuf, max: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > max {
        return Err(format!(
            "{}: file exceeds {} byte limit",
            path.display(),
            max
        ));
    }
    Ok(bytes)
}

fn parse_key(text: &str) -> Result<BundleKey, String> {
    let text = text.trim();
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("bundle key must be 64 hex digits (32 bytes)".into());
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(BundleKey::new(key))
}

fn canonical_banner() -> String {
    format!(
        "  \\\\    //   vesper {} (stage3-ddc)\n   \\\\  //    target: {}-{} (cranelift + lean4)\n    \\\\||/    modes: copy <= affine <= linear | 0 sorryAx\n     \\/\n",
        env!("CARGO_PKG_VERSION"),
        env::consts::ARCH,
        env::consts::OS,
    )
}

fn compute_latency(samples: &[u64]) -> LatencyStats {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let bucket_defs: [(&str, u64, u64); 5] = [
        ("<1us", 0, 1_000),
        ("1-10us", 1_000, 10_000),
        ("10-100us", 10_000, 100_000),
        ("100us-1ms", 100_000, 1_000_000),
        (">=1ms", 1_000_000, u64::MAX),
    ];
    let buckets = bucket_defs
        .into_iter()
        .map(|(label, lo, hi)| {
            let count = sorted.iter().filter(|&&v| v >= lo && v < hi).count();
            (label.to_string(), count)
        })
        .collect();
    if sorted.is_empty() {
        return LatencyStats {
            samples: 0,
            min_ns: 0,
            p50_ns: 0,
            p95_ns: 0,
            max_ns: 0,
            buckets,
        };
    }
    let idx = |num: usize, den: usize| -> usize { ((sorted.len() - 1) * num) / den };
    LatencyStats {
        samples: sorted.len(),
        min_ns: sorted[0],
        p50_ns: sorted[idx(1, 2)],
        p95_ns: sorted[idx(95, 100)],
        max_ns: sorted[sorted.len() - 1],
        buckets,
    }
}

/// Deterministic canonical wait-for graph cycle detection.
/// Restricts cycle enumeration to strongly connected components via Tarjan's
/// algorithm and Johnson's circuit search so acyclic DAGs run in O(V + E) time
/// while preserving all distinct canonical elementary cycles.
fn detect_wait_cycles(edges: &[WaitEdge]) -> Vec<Vec<u64>> {
    let mut nodes_set: BTreeSet<u64> = BTreeSet::new();
    for e in edges {
        nodes_set.insert(e.from_fiber);
        nodes_set.insert(e.to_fiber);
    }
    if nodes_set.is_empty() {
        return Vec::new();
    }
    let nodes: Vec<u64> = nodes_set.into_iter().collect();
    let idx_of: BTreeMap<u64, usize> = nodes.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let n = nodes.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        let u = idx_of[&e.from_fiber];
        let v = idx_of[&e.to_fiber];
        adj[u].push(v);
    }
    for neighbors in &mut adj {
        neighbors.sort_unstable();
        neighbors.dedup();
    }

    let sccs = tarjan_sccs(&adj);
    let mut found: BTreeSet<Vec<u64>> = BTreeSet::new();
    let mut in_scc = vec![false; n];
    let mut blocked = vec![false; n];
    let mut b_map: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];

    struct CircuitFrame {
        v: usize,
        next_edge: usize,
        found_cycle: bool,
    }

    for mut scc in sccs {
        if scc.len() == 1 {
            let u = scc[0];
            if adj[u].binary_search(&u).is_ok() {
                found.insert(vec![nodes[u], nodes[u]]);
            }
            continue;
        }
        scc.sort_unstable();
        for &u in &scc {
            in_scc[u] = true;
        }
        for (pos, &start) in scc.iter().enumerate() {
            for &u in &scc[pos..] {
                blocked[u] = false;
                b_map[u].clear();
            }

            let mut path = vec![start];
            blocked[start] = true;
            let mut frames = vec![CircuitFrame {
                v: start,
                next_edge: 0,
                found_cycle: false,
            }];

            'dfs: while let Some(top) = frames.last_mut() {
                let v = top.v;
                while top.next_edge < adj[v].len() {
                    let w = adj[v][top.next_edge];
                    top.next_edge += 1;
                    if w < start || !in_scc[w] {
                        continue;
                    }
                    if w == start {
                        let mut cycle: Vec<u64> = path.iter().map(|&idx| nodes[idx]).collect();
                        cycle.push(nodes[start]);
                        found.insert(cycle);
                        top.found_cycle = true;
                    } else if !blocked[w] {
                        blocked[w] = true;
                        path.push(w);
                        frames.push(CircuitFrame {
                            v: w,
                            next_edge: 0,
                            found_cycle: false,
                        });
                        continue 'dfs;
                    }
                }

                let frame = frames.pop().expect("non-empty circuit frame stack");
                path.pop();
                if frame.found_cycle {
                    let mut unblock_stack = vec![frame.v];
                    while let Some(u) = unblock_stack.pop() {
                        if blocked[u] {
                            blocked[u] = false;
                            let deps = std::mem::take(&mut b_map[u]);
                            for w in deps {
                                if blocked[w] {
                                    unblock_stack.push(w);
                                }
                            }
                        }
                    }
                    if let Some(parent) = frames.last_mut() {
                        parent.found_cycle = true;
                    }
                } else {
                    for &w in &adj[frame.v] {
                        if w >= start && in_scc[w] {
                            b_map[w].insert(frame.v);
                        }
                    }
                }
            }

            in_scc[start] = false;
        }
    }

    found.into_iter().collect()
}

fn tarjan_sccs(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index: Vec<Option<usize>> = vec![None; n];
    let mut lowlink: Vec<usize> = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut tarjan_stack = Vec::new();
    let mut call_stack: Vec<(usize, usize)> = Vec::new();
    let mut next_index = 0usize;
    let mut sccs = Vec::new();

    for root in 0..n {
        if index[root].is_some() {
            continue;
        }
        index[root] = Some(next_index);
        lowlink[root] = next_index;
        next_index += 1;
        on_stack[root] = true;
        tarjan_stack.push(root);
        call_stack.push((root, 0));

        while let Some((u, edge_i)) = call_stack.last_mut() {
            let u = *u;
            if *edge_i < adj[u].len() {
                let v = adj[u][*edge_i];
                *edge_i += 1;
                if index[v].is_none() {
                    index[v] = Some(next_index);
                    lowlink[v] = next_index;
                    next_index += 1;
                    on_stack[v] = true;
                    tarjan_stack.push(v);
                    call_stack.push((v, 0));
                } else if on_stack[v] {
                    lowlink[u] = lowlink[u].min(index[v].expect("visited index"));
                }
            } else {
                call_stack.pop();
                if let Some(&(parent, _)) = call_stack.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[u]);
                }
                if lowlink[u] == index[u].expect("root index") {
                    let mut scc = Vec::new();
                    while let Some(w) = tarjan_stack.pop() {
                        on_stack[w] = false;
                        scc.push(w);
                        if w == u {
                            break;
                        }
                    }
                    sccs.push(scc);
                }
            }
        }
    }

    sccs
}

fn analyze_snapshot(snapshot: &TelemetrySnapshot) -> TelemetryReport {
    let mut owner_by_obligation: BTreeMap<u64, u64> = BTreeMap::new();
    let mut all_samples = Vec::new();
    let mut total_run_ns = 0u64;
    let mut total_idle_ns = 0u64;
    let mut parked_fibers = 0usize;

    for f in &snapshot.fibers {
        total_run_ns = total_run_ns.saturating_add(f.run_ns);
        total_idle_ns = total_idle_ns.saturating_add(f.idle_ns);
        if f.status == FiberStatus::Parked {
            parked_fibers += 1;
        }
        all_samples.extend_from_slice(&f.wake_to_poll_ns);
        for ob in &f.obligations {
            owner_by_obligation.insert(ob.obligation_id, f.fiber_id);
        }
    }

    let mut wait_edges = Vec::new();
    let mut live_obligations = Vec::new();
    let mut fibers = Vec::new();

    let mut sorted_fibers = snapshot.fibers.clone();
    sorted_fibers.sort_by_key(|f| f.fiber_id);

    for f in &sorted_fibers {
        let total = f.run_ns.saturating_add(f.idle_ns);
        let run_percent_tenths = f
            .run_ns
            .saturating_mul(1000)
            .checked_div(total)
            .unwrap_or(0);
        let stats = compute_latency(&f.wake_to_poll_ns);
        fibers.push(FiberSummary {
            fiber_id: f.fiber_id,
            worker_core: f.worker_core,
            status: f.status,
            run_ns: f.run_ns,
            idle_ns: f.idle_ns,
            run_percent_tenths,
            p95_wake_to_poll_ns: stats.p95_ns,
            effect_stack: f.effect_stack.clone(),
            live_linear_obligations: f.obligations.len(),
        });

        if let Some(target) = f.waiting_on_fiber {
            wait_edges.push(WaitEdge {
                from_fiber: f.fiber_id,
                to_fiber: target,
                reason: "fiber wait".into(),
            });
        }
        for ob in &f.obligations {
            live_obligations.push(ObligationReportRow {
                obligation_id: ob.obligation_id,
                kind: ob.kind.clone(),
                owner_fiber_id: f.fiber_id,
                parked: ob.parked,
                waiting_on_fiber: ob.waiting_on_fiber,
                waiting_on_obligation: ob.waiting_on_obligation,
            });
            if let Some(target) = ob.waiting_on_fiber {
                wait_edges.push(WaitEdge {
                    from_fiber: f.fiber_id,
                    to_fiber: target,
                    reason: format!("obligation {} ({})", ob.obligation_id, ob.kind),
                });
            } else if let Some(target_ob) = ob.waiting_on_obligation
                && let Some(&target_fiber) = owner_by_obligation.get(&target_ob)
            {
                wait_edges.push(WaitEdge {
                    from_fiber: f.fiber_id,
                    to_fiber: target_fiber,
                    reason: format!(
                        "obligation {} ({}) -> obligation {target_ob}",
                        ob.obligation_id, ob.kind
                    ),
                });
            }
        }
    }

    let deadlock_cycles = detect_wait_cycles(&wait_edges);
    TelemetryReport {
        schema_version: 1,
        source: snapshot.source.clone(),
        observer_truncated: snapshot.observer_truncated,
        total_fibers: fibers.len(),
        parked_fibers,
        total_run_ns,
        total_idle_ns,
        total_live_obligations: live_obligations.len(),
        wake_to_poll: compute_latency(&all_samples),
        wait_edges,
        deadlock_cycles,
        fibers,
        live_obligations,
    }
}

fn chrono_to_snapshot(path: &PathBuf, key: &BundleKey) -> Result<TelemetrySnapshot, String> {
    let bytes = read_bounded(path, MAX_TRACE_BYTES)?;
    let session = ReplaySession::open_bundle(&bytes, key, &BundleLimits::default())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let snapshot = chrono_session_to_snapshot(path, &session);
    let _ = session.close();
    Ok(snapshot)
}

fn chrono_session_to_snapshot(path: &Path, session: &ReplaySession) -> TelemetrySnapshot {
    let bundle = session.bundle();
    #[derive(Clone, Copy, Default)]
    enum SchedState {
        #[default]
        Unknown,
        RunnableSince(u64),
        RunningSince(u64),
        ParkedSince(u64),
        Finished,
    }
    #[derive(Default)]
    struct Acc {
        run_ns: u64,
        idle_ns: u64,
        wake_to_poll_ns: Vec<u64>,
        sched_state: SchedState,
        effects: BTreeSet<String>,
        waiting_on: Option<u64>,
    }
    let mut per_fiber: BTreeMap<u64, Acc> = BTreeMap::new();
    let mut sim_now_ns: Option<u64> = None;
    for (idx, rec) in bundle.records.iter().enumerate() {
        let label = match rec.effect_id {
            0xFFF0 => "InOutWrite".to_string(),
            0xFFF1 => "LinearObligation".to_string(),
            0xFFF2 => "Snapshot".to_string(),
            0xFFF3 => "Scheduler".to_string(),
            other => format!("Effect#{other:#06x}"),
        };
        if rec.effect_id == 0xFFF3
            && let Ok(event) = serde_json::from_slice::<TraceEvent>(&rec.payload)
        {
            match event {
                TraceEvent::ClockAdvance { to_ns, .. } => {
                    sim_now_ns = Some(to_ns);
                    continue;
                }
                TraceEvent::Spawn { task, at_ns } => {
                    sim_now_ns = Some(at_ns);
                    let entry = per_fiber.entry(task.get()).or_default();
                    entry.effects.insert(label);
                    entry.sched_state = SchedState::RunnableSince(at_ns);
                }
                TraceEvent::Dispatch { task, at_ns, .. } => {
                    sim_now_ns = Some(at_ns);
                    let entry = per_fiber.entry(task.get()).or_default();
                    entry.effects.insert(label);
                    match entry.sched_state {
                        SchedState::RunnableSince(woke) => {
                            let wait = at_ns.saturating_sub(woke);
                            entry.wake_to_poll_ns.push(wait);
                            entry.idle_ns = entry.idle_ns.saturating_add(wait);
                        }
                        SchedState::ParkedSince(parked) => {
                            entry.idle_ns =
                                entry.idle_ns.saturating_add(at_ns.saturating_sub(parked));
                        }
                        SchedState::RunningSince(start) => {
                            entry.run_ns = entry.run_ns.saturating_add(at_ns.saturating_sub(start));
                        }
                        SchedState::Unknown | SchedState::Finished => {}
                    }
                    entry.sched_state = SchedState::RunningSince(at_ns);
                }
                TraceEvent::Yield { task, at_ns } => {
                    sim_now_ns = Some(at_ns);
                    let entry = per_fiber.entry(task.get()).or_default();
                    entry.effects.insert(label);
                    if let SchedState::RunningSince(start) = entry.sched_state {
                        entry.run_ns = entry.run_ns.saturating_add(at_ns.saturating_sub(start));
                    }
                    entry.sched_state = SchedState::RunnableSince(at_ns);
                }
                TraceEvent::Sleep { task, .. } | TraceEvent::SubmitIo { task, .. } => {
                    let next_timeline_ns = bundle.records[idx + 1..].iter().find_map(|future| {
                        if future.effect_id != 0xFFF3 {
                            return None;
                        }
                        let future_ev =
                            serde_json::from_slice::<TraceEvent>(&future.payload).ok()?;
                        match future_ev {
                            TraceEvent::ClockAdvance { from_ns, .. } => Some(from_ns),
                            TraceEvent::Spawn { at_ns, .. }
                            | TraceEvent::Dispatch { at_ns, .. }
                            | TraceEvent::Yield { at_ns, .. }
                            | TraceEvent::TimerElapsed { at_ns, .. }
                            | TraceEvent::IoCompleted { at_ns, .. }
                            | TraceEvent::CancelRequested { at_ns, .. }
                            | TraceEvent::Finish { at_ns, .. } => Some(at_ns),
                            TraceEvent::Sleep { .. } | TraceEvent::SubmitIo { .. } => None,
                        }
                    });
                    let at_ns = match (sim_now_ns, next_timeline_ns) {
                        (Some(prev), Some(next_bound)) if rec.time_ns > next_bound => {
                            prev.max(next_bound)
                        }
                        (None, Some(next_bound)) if rec.time_ns > next_bound => next_bound,
                        _ => rec.time_ns,
                    };
                    sim_now_ns = Some(at_ns);
                    let entry = per_fiber.entry(task.get()).or_default();
                    entry.effects.insert(label);
                    if let SchedState::RunningSince(start) = entry.sched_state {
                        entry.run_ns = entry.run_ns.saturating_add(at_ns.saturating_sub(start));
                    }
                    entry.sched_state = SchedState::ParkedSince(at_ns);
                }
                TraceEvent::TimerElapsed { task, at_ns, .. }
                | TraceEvent::IoCompleted { task, at_ns, .. } => {
                    sim_now_ns = Some(at_ns);
                    let entry = per_fiber.entry(task.get()).or_default();
                    entry.effects.insert(label);
                    if let SchedState::ParkedSince(parked) = entry.sched_state {
                        entry.idle_ns = entry.idle_ns.saturating_add(at_ns.saturating_sub(parked));
                    }
                    entry.sched_state = SchedState::RunnableSince(at_ns);
                }
                TraceEvent::CancelRequested { task, at_ns } => {
                    sim_now_ns = Some(at_ns);
                    let entry = per_fiber.entry(task.get()).or_default();
                    entry.effects.insert(label);
                }
                TraceEvent::Finish { task, at_ns } => {
                    sim_now_ns = Some(at_ns);
                    let entry = per_fiber.entry(task.get()).or_default();
                    entry.effects.insert(label);
                    if let SchedState::RunningSince(start) = entry.sched_state {
                        entry.run_ns = entry.run_ns.saturating_add(at_ns.saturating_sub(start));
                    }
                    entry.sched_state = SchedState::Finished;
                }
            }
            continue;
        }
        let entry = per_fiber.entry(rec.fiber_id).or_default();
        entry.effects.insert(label);
    }

    let lineage = session.lineage();
    let open = lineage.open_obligations();
    let mut ob_by_fiber: BTreeMap<u64, Vec<ObligationEntry>> = BTreeMap::new();
    for ob_id in open {
        if let Ok(chain) = lineage.chain(ob_id) {
            let owner = match chain.last() {
                Some(hop) => match hop.kind {
                    HopKind::ChannelHandoff { to_fiber } => to_fiber,
                    _ => hop.fiber_id,
                },
                None => 0,
            };
            ob_by_fiber.entry(owner).or_default().push(ObligationEntry {
                obligation_id: ob_id,
                kind: "ChronoObligation".into(),
                parked: true,
                waiting_on_fiber: None,
                waiting_on_obligation: None,
            });
            per_fiber.entry(owner).or_default();
        }
    }

    let fibers = per_fiber
        .into_iter()
        .map(|(fiber_id, acc)| {
            let obs = ob_by_fiber.remove(&fiber_id).unwrap_or_default();
            let status = if !obs.is_empty() || matches!(acc.sched_state, SchedState::ParkedSince(_))
            {
                FiberStatus::Parked
            } else {
                match acc.sched_state {
                    SchedState::RunningSince(_) => FiberStatus::Running,
                    SchedState::RunnableSince(_) => FiberStatus::Runnable,
                    SchedState::ParkedSince(_) => FiberStatus::Parked,
                    SchedState::Finished | SchedState::Unknown => FiberStatus::Completed,
                }
            };
            FiberTelemetry {
                fiber_id,
                worker_core: 0,
                status,
                run_ns: acc.run_ns,
                idle_ns: acc.idle_ns,
                wake_to_poll_ns: acc.wake_to_poll_ns,
                effect_stack: acc.effects.into_iter().collect(),
                obligations: obs,
                waiting_on_fiber: acc.waiting_on,
            }
        })
        .collect();

    TelemetrySnapshot {
        schema_version: 1,
        source: format!("chrono:{}", path.display()),
        observer_truncated: false,
        fibers,
    }
}

fn run_module_snapshot(
    path: &PathBuf,
    entry: &str,
    fuel: u64,
) -> Result<TelemetrySnapshot, String> {
    let bytes = read_bounded(path, MAX_SOURCE_BYTES)?;
    let source = String::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let parsed = parser::parse(&source);
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "{}: {} syntax error(s)",
            path.display(),
            parsed.diagnostics.len()
        ));
    }
    let checked = check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return Err(format!(
            "{}: {} semantic error(s)",
            path.display(),
            checked.diagnostics.len()
        ));
    }
    let f = checked
        .world
        .functions
        .get(entry)
        .ok_or_else(|| format!("missing entry function '{entry}'"))?;
    if !f.params.is_empty() {
        return Err("entry function must take no arguments".into());
    }
    if !f.effects.instances.is_empty() || f.effects.tail.is_some() {
        return Err("entry function must handle all effects".into());
    }
    let linear_types: Vec<String> = checked
        .world
        .adts
        .values()
        .filter(|adt| {
            adt.mode == Mode::Linear
                && !adt.is_enum
                && adt.generics.is_empty()
                && adt.variants.len() == 1
                && adt.variants[0].name == adt.name
                && adt.variants[0].fields.is_empty()
        })
        .map(|adt| adt.name.clone())
        .take(ResourceTraceLimits::HARD.max_selected_types)
        .collect();
    let started = Instant::now();
    let mut runtime = Runtime::new(&checked.world);
    runtime.fuel = fuel;
    let observer = if linear_types.is_empty() {
        None
    } else {
        let mut limits = ResourceTraceLimits::HARD;
        limits.max_selected_types = linear_types.len();
        let total_name_bytes: usize = linear_types.iter().map(String::len).sum();
        if total_name_bytes <= ResourceTraceLimits::HARD.max_selected_name_bytes {
            limits.max_selected_name_bytes = total_name_bytes.max(1);
        }
        let selected_refs: Vec<&str> = linear_types.iter().map(String::as_str).collect();
        let config = ResourceTraceConfig::new(&checked.world, &selected_refs, limits)
            .map_err(|e| e.to_string())?
            .with_best_effort(true);
        Some(
            runtime
                .observe_resources(config)
                .map_err(|e| e.to_string())?,
        )
    };
    let _val = runtime.run(entry, vec![])?;
    let elapsed_ns = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
    let effect_stack = runtime.active_effects();
    let mut obligations = Vec::new();
    let mut observer_truncated = false;
    if let Some(observer) = observer {
        let report = observer.snapshot().map_err(|e| e.to_string())?;
        observer_truncated = report.truncated();
        let selected = report.selected_types();
        for live in report.live() {
            obligations.push(ObligationEntry {
                obligation_id: live.identity().serial().saturating_add(1),
                kind: selected
                    .get(live.kind())
                    .cloned()
                    .unwrap_or_else(|| "LinearResource".into()),
                parked: false,
                waiting_on_fiber: None,
                waiting_on_obligation: None,
            });
        }
        obligations.sort_by_key(|ob| ob.obligation_id);
    }

    Ok(TelemetrySnapshot {
        schema_version: 1,
        source: format!("vesper:{}#{entry}", path.display()),
        observer_truncated,
        fibers: vec![FiberTelemetry {
            fiber_id: 1,
            worker_core: 0,
            status: FiberStatus::Completed,
            run_ns: elapsed_ns.max(1),
            idle_ns: 0,
            wake_to_poll_ns: vec![elapsed_ns.min(1_000)],
            effect_stack,
            obligations,
            waiting_on_fiber: None,
        }],
    })
}

fn print_human(report: &TelemetryReport, no_banner: bool) {
    if !no_banner {
        print!("{}", canonical_banner());
    }
    println!(
        "pulse top [{}] — {} fiber(s) ({} parked), {} live linear obligation(s)",
        report.source, report.total_fibers, report.parked_fibers, report.total_live_obligations
    );
    if report.observer_truncated {
        println!("note: linear obligation trace truncated at observer capacity");
    }
    println!(
        "wake-to-poll latency ({} samples): min={}ns p50={}ns p95={}ns max={}ns",
        report.wake_to_poll.samples,
        report.wake_to_poll.min_ns,
        report.wake_to_poll.p50_ns,
        report.wake_to_poll.p95_ns,
        report.wake_to_poll.max_ns
    );
    let max_bucket = report
        .wake_to_poll
        .buckets
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(0);
    for (label, count) in &report.wake_to_poll.buckets {
        let width = if max_bucket == 0 {
            0
        } else {
            (*count * 16).div_ceil(max_bucket)
        };
        let bar = "#".repeat(width);
        println!("  {label:<10} | {bar:<16} {count}");
    }
    println!("\nFibers:");
    for f in &report.fibers {
        let status = match f.status {
            FiberStatus::Running => "running",
            FiberStatus::Runnable => "runnable",
            FiberStatus::Parked => "parked",
            FiberStatus::Completed => "completed",
        };
        let effects = if f.effect_stack.is_empty() {
            "-".to_string()
        } else {
            f.effect_stack.join(" -> ")
        };
        println!(
            "  fiber {:<4} core={} state={:<9} run={}ns idle={}ns ({}.{}% active) p95_wake={}ns obs={} eff=[{}]",
            f.fiber_id,
            f.worker_core,
            status,
            f.run_ns,
            f.idle_ns,
            f.run_percent_tenths / 10,
            f.run_percent_tenths % 10,
            f.p95_wake_to_poll_ns,
            f.live_linear_obligations,
            effects,
        );
    }
    if !report.live_obligations.is_empty() {
        println!("\nLive Linear Obligations:");
        for ob in &report.live_obligations {
            println!(
                "  ob {:<4} kind={:<16} owner=fiber {:<4} parked={} wait_fiber={:?} wait_ob={:?}",
                ob.obligation_id,
                ob.kind,
                ob.owner_fiber_id,
                ob.parked,
                ob.waiting_on_fiber,
                ob.waiting_on_obligation,
            );
        }
    }
    if report.deadlock_cycles.is_empty() {
        println!("\nWait-For Cycles: none");
    } else {
        println!(
            "\nWait-For Cycles ({} detected):",
            report.deadlock_cycles.len()
        );
        for cyc in &report.deadlock_cycles {
            let chain: Vec<String> = cyc.iter().map(|id| format!("fiber {id}")).collect();
            println!("  DEADLOCK CYCLE: {}", chain.join(" -> "));
        }
    }
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut trace_path: Option<PathBuf> = None;
    let mut chrono_path: Option<PathBuf> = None;
    let mut key: Option<BundleKey> = None;
    let mut entry = "main".to_string();
    let mut fuel = 10_000_000u64;
    let mut write_trace: Option<PathBuf> = None;
    let mut fail_on_cycle = false;
    let mut no_banner = false;
    let mut json_out = false;
    let mut module_path: Option<PathBuf> = None;

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "--trace" => {
                if trace_path.is_some() {
                    return Err("--trace may only be specified once".into());
                }
                trace_path = Some(PathBuf::from(it.next().ok_or("--trace requires a path")?));
            }
            "--chrono" => {
                if chrono_path.is_some() {
                    return Err("--chrono may only be specified once".into());
                }
                chrono_path = Some(PathBuf::from(it.next().ok_or("--chrono requires a path")?));
            }
            "--key" => {
                let raw = it.next().ok_or("--key requires 64 hex digits")?;
                key = Some(parse_key(&raw)?);
            }
            "--key-file" => {
                let p = PathBuf::from(it.next().ok_or("--key-file requires a path")?);
                let bytes = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                key = Some(match <[u8; 32]>::try_from(bytes.as_slice()) {
                    Ok(raw) => BundleKey::new(raw),
                    Err(_) => parse_key(&String::from_utf8_lossy(&bytes))?,
                });
            }
            "--entry" => entry = it.next().ok_or("--entry requires a function name")?,
            "--fuel" => {
                fuel = it
                    .next()
                    .ok_or("--fuel requires an integer")?
                    .parse()
                    .map_err(|_| "invalid --fuel value")?;
            }
            "--write-trace" => {
                write_trace = Some(PathBuf::from(
                    it.next().ok_or("--write-trace requires a path")?,
                ));
            }
            "--fail-on-cycle" => fail_on_cycle = true,
            "--no-banner" => no_banner = true,
            "--json" => json_out = true,
            _ if arg.starts_with('-') => return Err(format!("unknown top option '{arg}'")),
            _ => {
                if module_path.replace(PathBuf::from(arg)).is_some() {
                    return Err("top accepts at most one .ves source path".into());
                }
            }
        }
    }

    if trace_path.is_none()
        && chrono_path.is_none()
        && module_path.is_none()
        && let Ok(env_path) = env::var("VESPER_PULSE_TRACE_PATH")
        && !env_path.is_empty()
    {
        trace_path = Some(PathBuf::from(env_path));
    }

    let input_count = [
        trace_path.is_some(),
        chrono_path.is_some(),
        module_path.is_some(),
    ]
    .into_iter()
    .filter(|&b| b)
    .count();
    if input_count != 1 {
        return Err(
            "top requires exactly one input source: --trace, --chrono, or <SOURCE.ves>; see top --help"
                .into(),
        );
    }

    let snapshot = if let Some(path) = trace_path {
        let bytes = read_bounded(&path, MAX_TRACE_BYTES)?;
        let mut parsed: TelemetrySnapshot = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{}: invalid telemetry snapshot: {e}", path.display()))?;
        if parsed.schema_version != 1 {
            return Err(format!(
                "unsupported telemetry schema_version {}; expected 1",
                parsed.schema_version
            ));
        }
        if parsed.source.is_empty() {
            parsed.source = path.display().to_string();
        }
        parsed
    } else if let Some(path) = chrono_path {
        let k = key.ok_or("--chrono requires --key <hex> or --key-file <path>")?;
        chrono_to_snapshot(&path, &k)?
    } else {
        let path = module_path.expect("validated input count");
        run_module_snapshot(&path, &entry, fuel)?
    };

    if let Some(out) = write_trace.as_deref() {
        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?;
        vesper::atomic_file::atomic_write(out, &bytes)?;
    }

    let report = analyze_snapshot(&snapshot);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!(report)).map_err(|e| e.to_string())?
        );
    } else {
        print_human(&report, no_banner);
    }

    if fail_on_cycle && !report.deadlock_cycles.is_empty() {
        return Err(format!(
            "wait-for deadlock detected ({} cycle(s))",
            report.deadlock_cycles.len()
        ));
    }
    Ok(())
}
