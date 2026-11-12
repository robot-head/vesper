//! Execution caching with the real component: hits are equivalent to misses
//! but freshly approved, keys bind every execution input, and corrupt or
//! stale entries are rejected or repaired through the documented policy.

mod common;

use common::*;
use sha2::{Digest, Sha256};
use vesper_forge::*;
use vesper_forge_runner::*;
use vesper_loom_cas::{DerivationInputs, Domain, Store};

fn rw() -> ExecOptions {
    ExecOptions::default()
}

#[test]
fn committed_fixture_matches_its_checksum() {
    let hex: String = Sha256::digest(COMPONENT)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        SHA256SUMS.split_whitespace().next(),
        Some(hex.as_str()),
        "fixtures/bin/macro.component.wasm differs from SHA256SUMS; rebuild with fixtures/build.sh"
    );
}

#[test]
fn host_identity_is_measured_and_complete() {
    let id = HostIdentity::current();
    assert!(id.is_complete(), "missing {:?}", id.missing());
    assert_eq!(id.executable.as_ref().unwrap().len(), 64);
    if cfg!(target_os = "linux") {
        assert!(id.mapped_objects.is_some());
    }
}

#[test]
fn real_miss_then_hit_gives_equivalent_fresh_expansions_in_new_sessions() {
    let w = World::new();
    let dir = w.template("pure", "{ let y = hash_into($input); y }");
    let req = w.template_request(&dir);
    let ctx = context(&req, vec![], vec![]);

    let mut a = ctx.open_session(SessionId(1)).unwrap();
    let before = w.executions();
    let first = expand(&mut a, &w.runner, &req, "1", "", rw()).unwrap();
    let ex1 = first.execution.clone().unwrap();
    assert_eq!(ex1.status, CacheStatus::Miss);
    assert!(ex1.executed && ex1.newly_recorded);
    assert!(ex1.fuel_consumed > 0);
    assert_eq!(w.executions(), before + 1);
    assert!(ex1.diagnostics.iter().any(|d| d.message.contains("tokens")));
    let acc1 = first.accepted().expect("pure output is accepted").clone();

    // A different session over the same context hits without executing.
    let mut b = ctx.open_session(SessionId(2)).unwrap();
    let second = expand(&mut b, &w.runner, &req, "1", "", rw()).unwrap();
    let ex2 = second.execution.clone().unwrap();
    assert_eq!(ex2.status, CacheStatus::Hit);
    assert!(!ex2.executed && !ex2.newly_recorded);
    assert_eq!(w.executions(), before + 1);
    assert_eq!((ex2.key, ex2.raw_output), (ex1.key, ex1.raw_output));
    assert_eq!(ex2.fuel_consumed, ex1.fuel_consumed);
    let acc2 = second.accepted().expect("hit is approved afresh").clone();

    // Equivalent expansions ...
    assert_eq!(texts(&a, &acc1.tokens), texts(&b, &acc2.tokens));
    assert_eq!(
        texts(&a, &acc1.tokens),
        [
            "{",
            "let",
            "y",
            "=",
            "hash_into",
            "(",
            "1",
            ")",
            ";",
            "y",
            "}"
        ]
    );
    let strip = |audit: &AuthorityAudit| {
        let mut ops: Vec<_> = audit
            .operations
            .iter()
            .map(|o| (o.token, o.operation, o.requirements.clone()))
            .collect();
        ops.sort();
        let stamps: Vec<_> = audit
            .policies
            .iter()
            .map(|p| {
                (
                    p.macro_def,
                    p.macro_policy_version,
                    p.caller,
                    p.caller_ceiling_version,
                )
            })
            .collect();
        (audit.graph_version.clone(), stamps, ops)
    };
    assert_eq!(strip(&acc1.accepted.audit), strip(&acc2.accepted.audit));
    assert_eq!(first.approval_context, second.approval_context);

    // ... with identities minted by each session, never carried over.
    assert_eq!(acc1.accepted.audit.session, SessionId(1));
    assert_eq!(acc2.accepted.audit.session, SessionId(2));
    assert_eq!(first.expansion.unwrap().session(), SessionId(1));
    assert_eq!(second.expansion.unwrap().session(), SessionId(2));
    for t in &acc2.tokens {
        assert_eq!(t.origin(), Origin::Expansion(second.expansion.unwrap()));
        assert!(t.scopes().iter().all(|s| s.session() == SessionId(2)));
    }
    // Old scopes are foreign to the new session.
    let old = acc1.tokens[0].clone();
    assert!(matches!(
        b.host().validate_token(&old),
        Err(ForgeError::ForeignScope(_))
    ));

    // Reusing the record twice in one session still mints distinct
    // expansions and introduction scopes.
    let third = expand(&mut b, &w.runner, &req, "1", "", rw()).unwrap();
    assert_eq!(third.execution.as_ref().unwrap().status, CacheStatus::Hit);
    assert_ne!(third.expansion, second.expansion);
    let s2: Vec<_> = acc2.tokens[0].scopes().iter().collect();
    let s3: Vec<_> = third.accepted().unwrap().tokens[0]
        .scopes()
        .iter()
        .collect();
    assert_ne!(s2, s3);
}

#[test]
fn a_fresh_runner_process_state_reuses_records_from_the_store() {
    let w = World::new();
    let dir = w.template("pure", "hash_into(1)");
    let req = w.template_request(&dir);
    let ctx = context(&req, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    expand(&mut s, &w.runner, &req, "", "", rw()).unwrap();
    // A new engine (no compiled components, no counters) sees the record.
    let engine = std::sync::Arc::new(MacroEngine::new().unwrap());
    let runner = w.reopen(engine.clone());
    let mut s = ctx.open_session(SessionId(2)).unwrap();
    let o = expand(&mut s, &runner, &req, "", "", rw()).unwrap();
    assert_eq!(o.execution.as_ref().unwrap().status, CacheStatus::Hit);
    assert_eq!(engine.executions(), 0);
    assert!(o.accepted().is_some());
}

/// Every observable execution input changes the key.
#[test]
fn changed_component_input_settings_or_host_invalidate_execution() {
    let w = World::new();
    let dir = w.template("seeded", "f($seed, $input)");
    let base = w.template_request(&dir);
    let key = |req: &MacroRequest, input: &str| {
        let mut s = context(req, vec![], vec![])
            .open_session(SessionId(9))
            .unwrap();
        let toks = s.lex_source(APP, input).unwrap();
        let inv = s.host().invocation_for(req.kind.step(), &req.entry, &toks);
        w.runner.prepare(req, inv.tokens).unwrap().key()
    };
    let k0 = key(&base, "a");
    assert_eq!(k0, key(&base, "a"), "keys are deterministic");

    // Component bytes: append a custom section (still a valid component).
    let mut bytes = COMPONENT.to_vec();
    bytes.extend_from_slice(&[0x00, 0x04, 0x03, b'v', b'f', b'r']);
    let mut other = base.clone();
    other.component = bytes;
    assert_ne!(key(&other, "a"), k0);

    // Input tokens, args, entry and kind.
    assert_ne!(key(&base, "b"), k0);
    let mut args = base.clone();
    args.args.push("span=1:2:3".into());
    assert_ne!(key(&args, "a"), k0);
    let mut kind = base.clone();
    kind.kind = MacroKind::DeriveMacro;
    assert_ne!(key(&kind, "a"), k0);

    // Declared input contents.
    let changed = w.template("seeded2", "f($seed, $input) ");
    let mut input = base.clone();
    input.inputs[0].host_path = changed;
    assert_ne!(key(&input, "a"), k0);
    // Same contents in another host directory: same key.
    let same = w.template("seeded3", "f($seed, $input)");
    let mut moved = base.clone();
    moved.inputs[0].host_path = same;
    assert_eq!(key(&moved, "a"), k0);
    // Guest mount point.
    let mut mount = base.clone();
    mount.inputs[0].guest_path = "/in/other".into();
    assert_ne!(key(&mount, "a"), k0);

    // Determinism settings and every budget.
    let mut seed = base.clone();
    seed.determinism.random_seed = 1;
    assert_ne!(key(&seed, "a"), k0);
    let mut clock = base.clone();
    clock.determinism.wall_clock_ns = 5;
    assert_ne!(key(&clock, "a"), k0);
    let mut fuel = base.clone();
    fuel.limits.fuel += 1;
    assert_ne!(key(&fuel, "a"), k0);
    let mut transport = base.clone();
    transport.limits.transport.max_tokens -= 1;
    assert_ne!(key(&transport, "a"), k0);
    let mut backstop = base.clone();
    backstop.limits.wall_clock_backstop += std::time::Duration::from_nanos(1);
    assert_ne!(key(&backstop, "a"), k0);

    // Host implementation identity.
    let mut id = HostIdentity::current();
    id.executable = Some("0".repeat(64));
    let other_host = w.reopen(engine()).with_identity_unchecked(id);
    let mut s = context(&base, vec![], vec![])
        .open_session(SessionId(9))
        .unwrap();
    let toks = s.lex_source(APP, "a").unwrap();
    let inv = s
        .host()
        .invocation_for(base.kind.step(), &base.entry, &toks);
    assert_ne!(other_host.prepare(&base, inv.tokens).unwrap().key(), k0);
}

#[test]
fn settings_are_observable_and_deterministic() {
    let w = World::new();
    let dir = w.template("seeded", "hash_into($seed, $clock)");
    let req = w.template_request(&dir);
    let ctx = context(&req, vec![], vec![]);
    let run = |req: &MacroRequest, options| {
        let mut s = ctx.open_session(SessionId(1)).unwrap();
        // Bind the request's own component (seed changes do not rebind).
        let o = expand(&mut s, &w.runner, req, "", "", options).unwrap();
        let toks = texts(&s, &o.accepted().unwrap().tokens);
        (o.execution.unwrap(), toks)
    };
    let uncached = ExecOptions {
        cache: CachePolicy::Uncached,
        ..ExecOptions::default()
    };
    let (e1, t1) = run(&req, uncached);
    let (e2, t2) = run(&req, uncached);
    assert_eq!(e1.status, CacheStatus::Uncached);
    assert_eq!((e1.raw_output, &t1), (e2.raw_output, &t2));
    assert_eq!(w.records(), 0, "uncached runs publish nothing");
    assert_eq!(t1[4], "0", "fixed wall clock");

    let mut seeded = req.clone();
    seeded.determinism.random_seed = 99;
    seeded.determinism.wall_clock_ns = 7_000_000_000;
    let (e3, t3) = run(&seeded, uncached);
    assert_ne!(e3.raw_output, e1.raw_output);
    assert_ne!(t3[2], t1[2], "seed reaches the guest");
    assert_eq!(t3[4], "7");

    // Verify mode re-executes and matches the published record.
    let (m, _) = run(&req, ExecOptions::default());
    assert_eq!(m.status, CacheStatus::Miss);
    let before = w.executions();
    let (v, _) = run(
        &req,
        ExecOptions {
            cache: CachePolicy::Verify,
            ..ExecOptions::default()
        },
    );
    assert_eq!(v.status, CacheStatus::Verified);
    assert!(v.executed && !v.newly_recorded);
    assert_eq!(v.raw_output, m.raw_output);
    assert_eq!(w.executions(), before + 1);
}

#[test]
fn incomplete_host_identity_refuses_the_cache() {
    let w = World::new();
    let dir = w.template("pure", "hash_into(1)");
    let req = w.template_request(&dir);
    let mut id = HostIdentity::current();
    id.executable = None;
    let runner = w.reopen(engine()).with_identity_unchecked(id);
    let ctx = context(&req, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    let err = expand(&mut s, &runner, &req, "", "", rw()).unwrap_err();
    assert!(matches!(
        err,
        RunnerError::IncompleteIdentity { ref missing } if missing == &["executable"]
    ));
    let ok = expand(
        &mut s,
        &runner,
        &req,
        "",
        "",
        ExecOptions {
            cache: CachePolicy::Uncached,
            ..ExecOptions::default()
        },
    )
    .unwrap();
    assert_eq!(ok.execution.unwrap().status, CacheStatus::Uncached);
    assert_eq!(w.records(), 0);
}

/// Shared setup: publish one record and return what the tests tamper with.
fn published(
    w: &World,
) -> (
    MacroRequest,
    ApprovalContext,
    PreparedExecution,
    RawExecution,
) {
    let dir = w.template("pure", "hash_into(1)");
    let req = w.template_request(&dir);
    let ctx = context(&req, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    let toks = s.lex_source(APP, "").unwrap();
    let inv = s.host().invocation_for(req.kind.step(), &req.entry, &toks);
    let prepared = w.runner.prepare(&req, inv.tokens).unwrap();
    let raw = w.runner.execute(&prepared, rw()).unwrap();
    assert_eq!(raw.status, CacheStatus::Miss);
    (req, ctx, prepared, raw)
}

fn rerun() -> ExecOptions {
    ExecOptions {
        cache: CachePolicy::ReadWrite,
        on_corrupt: CorruptionPolicy::Rerun,
    }
}

fn overwrite(path: &std::path::Path, bytes: &[u8]) {
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(path, perm).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn quarantined(store: &Store) -> usize {
    std::fs::read_dir(store.root().join("quarantine"))
        .unwrap()
        .count()
}

#[test]
fn corrupt_raw_output_fails_closed_or_is_quarantined_and_rerun() {
    let w = World::new();
    let (req, ctx, prepared, raw) = published(&w);
    let store = w.runner.store();
    overwrite(&store.object_path(&raw.record_id), b"VFRX tampered");

    let err = w.runner.execute(&prepared, rw()).unwrap_err();
    assert!(matches!(err, RunnerError::CorruptCache { .. }), "{err}");
    // Approval is never reached from a corrupt entry.
    let mut s = ctx.open_session(SessionId(2)).unwrap();
    assert!(expand(&mut s, &w.runner, &req, "", "", rw()).is_err());

    let before = w.executions();
    let fixed = w.runner.execute(&prepared, rerun()).unwrap();
    assert_eq!(fixed.status, CacheStatus::Repaired);
    assert_eq!(fixed.record_id, raw.record_id);
    assert_eq!(w.executions(), before + 1);
    assert_eq!(quarantined(store), 1);
    assert_eq!(
        w.runner.execute(&prepared, rw()).unwrap().status,
        CacheStatus::Hit
    );
}

#[test]
fn corrupt_execution_records_fail_closed_or_are_repaired() {
    let w = World::new();
    let (_req, _ctx, prepared, raw) = published(&w);
    let key = prepared.key().0.to_hex();
    let path = w
        .runner
        .store()
        .root()
        .join("derivations/v1")
        .join(&key[..2])
        .join(&key);
    overwrite(&path, b"VLDR garbage");
    assert!(matches!(
        w.runner.execute(&prepared, rw()),
        Err(RunnerError::CorruptCache { .. })
    ));
    let fixed = w.runner.execute(&prepared, rerun()).unwrap();
    assert_eq!(fixed.status, CacheStatus::Repaired);
    assert_eq!(fixed.record_id, raw.record_id);
    assert_eq!(
        w.runner.execute(&prepared, rw()).unwrap().status,
        CacheStatus::Hit
    );
}

/// A well-formed record that names another execution's (or undecodable,
/// or oversized) output is stale: rejected, never replaced.
#[test]
fn stale_or_crossed_records_are_rejected_and_never_replaced() {
    let w = World::new();
    let (req, _ctx, prepared, raw) = published(&w);
    let store = w.runner.store();

    // Another execution of the same request with a different seed.
    let mut other = req.clone();
    other.determinism.random_seed = 5;
    let other_prep = w.runner.prepare(&other, vec![]).unwrap();
    assert_ne!(other_prep.key(), prepared.key());
    let cross = |inputs: &DerivationInputs, id| store.publish_derivation(inputs, id).unwrap();
    cross(other_prep.derivation_inputs(), raw.record_id);
    let err = w.runner.execute(&other_prep, rw()).unwrap_err();
    assert!(
        matches!(&err, RunnerError::CorruptCache { reason, .. } if reason.contains("another execution")),
        "{err}"
    );
    let err = w.runner.execute(&other_prep, rerun()).unwrap_err();
    assert!(
        matches!(err, RunnerError::DivergentExecution { .. }),
        "{err}"
    );

    // A record naming a well-hashed blob that is not a valid raw record.
    let mut junk = req.clone();
    junk.determinism.random_seed = 6;
    let junk_prep = w.runner.prepare(&junk, vec![]).unwrap();
    let garbage = store.put_blob(Domain::Artifact, b"not a record").unwrap();
    cross(junk_prep.derivation_inputs(), garbage.id);
    assert!(matches!(
        w.runner.execute(&junk_prep, rw()),
        Err(RunnerError::CorruptCache { reason, .. }) if reason.contains("BadMagic")
    ));

    // An oversized object is rejected by size before it is read.
    let mut big = req.clone();
    big.determinism.random_seed = 7;
    let big_prep = w.runner.prepare(&big, vec![]).unwrap();
    let huge = vec![b'x'; w.runner.caps().max_record_bytes + 1];
    let huge = store.put_blob(Domain::Artifact, &huge).unwrap();
    cross(big_prep.derivation_inputs(), huge.id);
    assert!(matches!(
        w.runner.execute(&big_prep, rw()),
        Err(RunnerError::CorruptCache { reason, .. }) if reason.contains("exceeds")
    ));

    // The original entry is untouched.
    assert_eq!(
        w.runner.execute(&prepared, rw()).unwrap().status,
        CacheStatus::Hit
    );
}

#[test]
fn records_are_strictly_bounded_by_the_requests_limits() {
    let w = World::new();
    let (req, _ctx, _prepared, raw) = published(&w);
    // The same raw bytes under a stricter request no longer decode: the
    // stored record is checked against the limits it is used under.
    let bytes = w.runner.store().read_blob(&raw.record_id).unwrap();
    let mut strict = req.clone();
    strict.limits.transport.max_tokens = 2;
    let limits = RecordLimits {
        transport: strict.limits.transport,
        max_stdio_bytes: strict.limits.max_stdio_bytes,
        max_record_bytes: w.runner.caps().max_record_bytes,
    };
    assert!(matches!(
        RawRecord::decode(&bytes, &limits),
        Err(RecordError::OverLimit {
            field: "tokens",
            ..
        })
    ));
}
