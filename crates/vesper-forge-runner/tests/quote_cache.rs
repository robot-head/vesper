mod common;
use common::{APP, CONNECT, DERIVE, DERIVE_MACRO, exact, globals, net, registry};
use std::sync::Arc;
use vesper_forge::typed_quote::*;
use vesper_forge::*;
use vesper_forge_runner::{
    CachePolicy, CacheStatus, CorruptionPolicy, ExecOptions, MacroEngine, MacroKind, MacroRequest,
    quote_runner::QuoteRunner,
    quote_session::{self, ComponentBinding, QuoteCall},
};
use vesper_loom_sandbox::quote_transport as q;
const COMPONENT: &[u8] =
    include_bytes!("../../vesper-loom-sandbox/fixtures/quotation/bin/quotation.component.wasm");
fn runner(dir: &std::path::Path) -> QuoteRunner {
    QuoteRunner::open(
        Arc::new(MacroEngine::new().unwrap()),
        dir.join("store"),
        dir.join("scratch"),
    )
    .unwrap()
}
fn request(entry: &str) -> MacroRequest {
    MacroRequest::new(COMPONENT.to_vec(), MacroKind::ProcMacro, entry)
}
fn invocation(req: &MacroRequest) -> q::Invocation {
    q::Invocation {
        protocol_version: 1,
        kind: q::MacroKind::ProcMacro,
        entry: req.entry.clone(),
        args: req.args.clone(),
        slots: vec![q::InputSlot {
            category: q::Category::Expr,
            tokens: vec![q::InputToken {
                kind: vesper_loom_sandbox::TokenKind::Ident,
                text: "x".into(),
                reported_span: None,
            }],
        }],
    }
}
fn host(id: u64) -> HygieneHost {
    let mut h = HygieneHost::new(SessionId(id), registry());
    for g in globals() {
        h.declare_global(g.package, &g.name, g.def).unwrap();
    }
    h
}
fn policies(allow: bool) -> PolicySet {
    PolicySet::new()
        .macro_policy(MacroPolicy {
            macro_def: DERIVE_MACRO,
            version: 1,
            emits: if allow { vec![exact(net())] } else { vec![] },
        })
        .ceiling(CallerCeiling {
            package: APP,
            version: 1,
            grants: vec![exact(net())],
        })
}
fn graph(effectful: bool) -> AuthorityGraph {
    let mut g = AuthorityGraph::new("same-user-label");
    let n = AuthorityNode::new(CONNECT);
    g.add_node(if effectful { n.direct(net()) } else { n })
        .unwrap();
    g
}
fn span(s: &str, part: &str) -> std::ops::Range<usize> {
    let start = s.find(part).unwrap();
    start..start + part.len()
}
fn expand(
    runner: &QuoteRunner,
    req: &MacroRequest,
    host: &mut HygieneHost,
    text: &str,
    allow: bool,
    effectful: bool,
) -> quote_session::FragmentOutcome {
    let mut budget = TypedBudget::new(TypedLimits::default()).unwrap();
    let source = host.import_expr_source(APP, text, &mut budget).unwrap();
    let arg = span(text, "x)").start;
    let input = host.expr_input(&source, arg..arg + 1, &mut budget).unwrap();
    let binding = ComponentBinding::new(DERIVE_MACRO, req).unwrap();
    let body = span(text, &text[text.find('{').unwrap()..]);
    let invocation = span(text, "keep!(x)");
    quote_session::expand_fragment(
        runner,
        host,
        QuoteCall {
            binding: &binding,
            request: req,
            site: InvocationSite::Source(APP),
            source: &source,
            body,
            invocation,
            inputs: vec![input],
            limits: q::Limits::default(),
        },
        &graph(effectful),
        &policies(allow),
        &mut budget,
        ExecOptions::default(),
    )
    .unwrap()
}
#[test]
fn actual_raw_miss_hit_rebinds_current_caller_locals_and_source() {
    let dir = tempfile::tempdir().unwrap();
    let runner = runner(dir.path());
    let req = request("capture");
    let mut first = host(1);
    let a = expand(
        &runner,
        &req,
        &mut first,
        "fn main(){let x=1;keep!(x)}",
        false,
        false,
    );
    assert_eq!(a.execution().status, CacheStatus::Miss);
    assert!(a.accepted().is_some(), "{:?}", a.rejection());
    let mut second = host(2);
    let b = expand(
        &runner,
        &req,
        &mut second,
        "fn main(){let x=2;keep!(x)}",
        false,
        false,
    );
    assert_eq!(b.execution().status, CacheStatus::Hit);
    assert!(!b.execution().executed);
    let f = &b.accepted().unwrap().fragment;
    assert!(f.tokens.iter().any(|t| second.text(t.text()) == "2"));
    let caller = f
        .references
        .iter()
        .find(|r| matches!(r.origin, Origin::Source { .. }))
        .unwrap();
    let Resolution::Local(id) = caller.resolution else {
        panic!()
    };
    assert_eq!(f.binder(id).unwrap().token, f.binders[0].token);
    assert_eq!(runner.engine().executions(), 1);
}
#[test]
fn cache_hit_rechecks_changed_policy_and_actual_graph_under_same_labels() {
    let dir = tempfile::tempdir().unwrap();
    let runner = runner(dir.path());
    let req = request("call-connect");
    let text = "fn main(){let x=1;keep!(x)}";
    let denied = expand(&runner, &req, &mut host(1), text, false, true);
    assert_eq!(denied.execution().status, CacheStatus::Miss);
    assert!(denied.accepted().is_none());
    let accepted = expand(&runner, &req, &mut host(2), text, true, true);
    assert_eq!(accepted.execution().status, CacheStatus::Hit);
    assert!(accepted.accepted().is_some());
    let pure = expand(&runner, &req, &mut host(3), text, false, false);
    assert!(pure.accepted().is_some());
    let changed = expand(&runner, &req, &mut host(4), text, false, true);
    assert_eq!(changed.execution().status, CacheStatus::Hit);
    assert!(changed.accepted().is_none());
    assert_eq!(runner.engine().executions(), 1);
}
#[test]
fn key_binds_structured_invocation_limits_determinism_and_declared_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let runner = runner(dir.path());
    let req = request("identity");
    let inv = invocation(&req);
    let a = runner.prepare(&req, &inv, q::Limits::default()).unwrap();
    assert_eq!(
        a.derivation_inputs().builder,
        "vesper-forge-quotation-runner"
    );
    assert!(
        a.derivation_inputs()
            .inputs
            .contains_key("quote-invocation")
    );
    assert!(!a.derivation_inputs().inputs.contains_key("invocation"));
    let mut other = inv.clone();
    other.slots[0].tokens[0].text = "y".into();
    assert_ne!(
        a.key(),
        runner
            .prepare(&req, &other, q::Limits::default())
            .unwrap()
            .key()
    );
    assert_ne!(
        a.key(),
        runner
            .prepare(
                &req,
                &inv,
                q::Limits {
                    max_work: 15 << 20,
                    ..q::Limits::default()
                }
            )
            .unwrap()
            .key()
    );
    let mut req2 = req.clone();
    req2.determinism.random_seed = 7;
    assert_ne!(
        a.key(),
        runner
            .prepare(&req2, &inv, q::Limits::default())
            .unwrap()
            .key()
    );
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(input.join("value.txt"), "1").unwrap();
    let mut file_req = request("read-input");
    file_req.args = vec!["/in/value.txt".into()];
    file_req = file_req.with_input(&input, "/in");
    let inv = invocation(&file_req);
    let first = runner
        .prepare(&file_req, &inv, q::Limits::default())
        .unwrap();
    let out = runner.execute(&first, ExecOptions::default()).unwrap();
    assert!(matches!(&out.record.outcome.items[0],q::Item::Generated(t)if t.text=="1"));
    std::fs::write(input.join("value.txt"), "2").unwrap();
    let next = runner
        .prepare(&file_req, &inv, q::Limits::default())
        .unwrap();
    assert_ne!(first.key(), next.key());
    let out = runner.execute(&next, ExecOptions::default()).unwrap();
    assert!(matches!(&out.record.outcome.items[0],q::Item::Generated(t)if t.text=="2"));
}
#[test]
fn corrupt_record_fails_or_repairs_and_verify_reexecutes_without_cached_approval() {
    let dir = tempfile::tempdir().unwrap();
    let runner = runner(dir.path());
    let req = request("identity");
    let p = runner
        .prepare(&req, &invocation(&req), q::Limits::default())
        .unwrap();
    let first = runner.execute(&p, ExecOptions::default()).unwrap();
    let path = runner.store().object_path(&first.record_id);
    let mut perm = std::fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&path, perm).unwrap();
    std::fs::write(&path, b"corrupt").unwrap();
    assert!(runner.execute(&p, ExecOptions::default()).is_err());
    let repaired = runner
        .execute(
            &p,
            ExecOptions {
                on_corrupt: CorruptionPolicy::Rerun,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(repaired.status, CacheStatus::Repaired);
    let verified = runner
        .execute(
            &p,
            ExecOptions {
                cache: CachePolicy::Verify,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(verified.status, CacheStatus::Verified);
    assert_eq!(runner.engine().executions(), 3);
}
#[test]
fn output_files_guest_errors_mismatches_and_binding_profile_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let runner = runner(dir.path());
    for entry in ["write-out", "fail", "error-diagnostic"] {
        let req = request(entry);
        let p = runner
            .prepare(&req, &invocation(&req), q::Limits::default())
            .unwrap();
        assert!(runner.execute(&p, ExecOptions::default()).is_err());
        assert!(
            runner
                .store()
                .lookup_derivation(p.derivation_inputs())
                .unwrap()
                .is_none()
        );
    }
    let req = request("identity");
    let mut inv = invocation(&req);
    inv.entry = "other".into();
    assert!(runner.prepare(&req, &inv, q::Limits::default()).is_err());
    let binding = ComponentBinding::new(DERIVE_MACRO, &req).unwrap();
    assert_eq!(binding.profile(), vesper_forge_runner::quote_key::PROFILE);
    assert_eq!(binding.macro_def().package, DERIVE);
}

#[test]
fn structured_adapter_checks_actual_host_liveness_and_closes_rejected_expansions() {
    use vesper_forge::quote_transport as adapter;
    let mut current = host(7);
    // Numeric sessions match deliberately; only the private host identity counts.
    let mut foreign = host(7);
    let mut budget = TypedBudget::new(TypedLimits::default()).unwrap();
    let source = current.import_expr_source(APP, "x", &mut budget).unwrap();
    let input = current.expr_input(&source, 0..1, &mut budget).unwrap();
    let expansion = current
        .begin_expr_expansion(
            DERIVE_MACRO,
            InvocationSite::Source(APP),
            vec![input],
            &mut budget,
        )
        .unwrap();
    let id = expansion.id();
    assert!(
        adapter::invocation_for_expr(
            &foreign,
            &expansion,
            q::MacroKind::ProcMacro,
            "identity",
            &[],
            q::Limits::default(),
            &mut budget,
        )
        .is_err()
    );
    expansion.validate_host(&current).unwrap();
    let outcome = q::Outcome {
        items: vec![q::Item::InputSlot(0)],
        diagnostics: vec![],
    };
    assert!(
        adapter::finish_expr_outcome(
            &mut foreign,
            expansion,
            &outcome,
            q::Limits::default(),
            &mut budget,
        )
        .is_err()
    );
    assert!(current.finish_generated(id, vec![]).is_err());

    let input = current.expr_input(&source, 0..1, &mut budget).unwrap();
    let expansion = current
        .begin_expr_expansion(
            DERIVE_MACRO,
            InvocationSite::Source(APP),
            vec![input],
            &mut budget,
        )
        .unwrap();
    current.abandon(expansion.id()).unwrap();
    assert!(expansion.validate_host(&current).is_err());
    assert!(
        adapter::invocation_for_expr(
            &current,
            &expansion,
            q::MacroKind::ProcMacro,
            "identity",
            &[],
            q::Limits::default(),
            &mut budget,
        )
        .is_err()
    );

    let input = current.expr_input(&source, 0..1, &mut budget).unwrap();
    let expansion = current
        .begin_expr_expansion(
            DERIVE_MACRO,
            InvocationSite::Source(APP),
            vec![input],
            &mut budget,
        )
        .unwrap();
    let id = expansion.id();
    let invalid = q::Outcome {
        items: vec![q::Item::InputSlot(1)],
        diagnostics: vec![],
    };
    assert!(
        adapter::finish_expr_outcome(
            &mut current,
            expansion,
            &invalid,
            q::Limits::default(),
            &mut budget,
        )
        .is_err()
    );
    assert!(current.finish_generated(id, vec![]).is_err());

    let input = current.expr_input(&source, 0..1, &mut budget).unwrap();
    let expansion = current
        .begin_expr_expansion(
            DERIVE_MACRO,
            InvocationSite::Source(APP),
            vec![input],
            &mut budget,
        )
        .unwrap();
    let mut tiny_metadata = TypedBudget::new(TypedLimits {
        max_metadata_bytes: 1,
        ..TypedLimits::default()
    })
    .unwrap();
    assert!(
        adapter::invocation_for_expr(
            &current,
            &expansion,
            q::MacroKind::ProcMacro,
            "identity",
            &[],
            q::Limits::default(),
            &mut tiny_metadata,
        )
        .is_err()
    );
    let id = expansion.id();
    let repeated = q::Outcome {
        items: vec![q::Item::InputSlot(0), q::Item::InputSlot(0)],
        diagnostics: vec![],
    };
    assert!(
        adapter::finish_expr_outcome(
            &mut current,
            expansion,
            &repeated,
            q::Limits {
                max_expanded_tokens: 1,
                ..q::Limits::default()
            },
            &mut budget,
        )
        .is_err()
    );
    assert!(current.finish_generated(id, vec![]).is_err());
}

#[cfg(unix)]
#[test]
fn corrupt_cache_special_files_and_final_symlinks_are_rejected_without_execution() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let dir = tempfile::tempdir().unwrap();
    let runner = runner(dir.path());
    let req = request("identity");
    let prepared = runner
        .prepare(&req, &invocation(&req), q::Limits::default())
        .unwrap();
    let first = runner.execute(&prepared, ExecOptions::default()).unwrap();
    let path = runner.store().object_path(&first.record_id);
    let external = dir.path().join("external-record");
    std::fs::copy(&path, &external).unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&external, &path).unwrap();
    assert!(runner.execute(&prepared, ExecOptions::default()).is_err());
    std::fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated owned path; no pointers are retained.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(runner.execute(&prepared, ExecOptions::default()).is_err());
    assert_eq!(runner.engine().executions(), 1);
}
