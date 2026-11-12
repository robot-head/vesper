//! Malformed, oversized and failing guests: rejected by the real sandbox
//! or the runner, with nothing published and no expansion accepted.

mod common;

use common::*;
use vesper_forge::*;
use vesper_forge_runner::*;
use vesper_loom_sandbox::{SandboxError, transport::TransportViolation};

fn request(entry: &str) -> MacroRequest {
    MacroRequest::new(COMPONENT.to_vec(), MacroKind::ProcMacro, entry)
}

fn run(w: &World, req: &MacroRequest) -> Result<MacroOutcome, RunnerError> {
    let ctx = context(req, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    expand(&mut s, &w.runner, req, "", "", ExecOptions::default())
}

fn transport(e: &RunnerError) -> &TransportViolation {
    match e {
        RunnerError::Sandbox(SandboxError::Transport { violation, .. }) => violation,
        other => panic!("expected a transport rejection, got {other}"),
    }
}

#[test]
fn malformed_and_oversized_packed_results_are_rejected_unpublished() {
    let w = World::new();
    let mut small = request("flood").with_args(["9"]);
    small.limits.transport.max_tokens = 8;
    assert!(matches!(
        transport(&run(&w, &small).unwrap_err()),
        TransportViolation::TooManyTokens { count: 9, limit: 8 }
    ));
    // At the limit it is fine (and the output is just unbound names).
    let mut at = request("flood").with_args(["8"]);
    at.limits.transport.max_tokens = 8;
    assert!(run(&w, &at).unwrap().rejection().is_some());
    let published = w.records();
    assert_eq!(published, 1);

    for (entry, check) in [
        (
            "alias-bomb",
            (|v: &TransportViolation| matches!(v, TransportViolation::TooManyTokens { .. }))
                as fn(&TransportViolation) -> bool,
        ),
        ("bad-slice", |v| {
            matches!(v, TransportViolation::SliceOutOfBounds { .. })
        }),
        ("bad-utf8", |v| {
            matches!(v, TransportViolation::InvalidUtf8 { .. })
        }),
        ("error-with-tokens", |v| {
            matches!(v, TransportViolation::TokensInFailure { .. })
        }),
        ("unbalanced", |v| {
            matches!(v, TransportViolation::UnbalancedDelimiters { .. })
        }),
    ] {
        let err = run(&w, &request(entry)).unwrap_err();
        assert!(check(transport(&err)), "{entry}: {err}");
    }
    // Text amplification is rejected by bytes even under a token limit
    // that admits it.
    let mut caps = RunnerLimits::default();
    caps.transport.max_tokens = 100_000;
    let wide = w.reopen(engine()).with_caps(caps);
    let mut bomb = request("alias-bomb");
    bomb.limits.transport.max_tokens = 100_000;
    let ctx = context(&bomb, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    let err = expand(&mut s, &wide, &bomb, "", "", ExecOptions::default()).unwrap_err();
    assert!(matches!(
        transport(&err),
        TransportViolation::TooMuchText { .. }
    ));
    assert_eq!(w.records(), published, "nothing hostile was published");
}

#[test]
fn failed_guests_publish_nothing() {
    let w = World::new();
    for entry in ["fail", "error-diagnostic"] {
        let err = run(&w, &request(entry)).unwrap_err();
        assert!(
            matches!(&err, RunnerError::GuestFailed { diagnostics } if !diagnostics.is_empty()),
            "{entry}: {err}"
        );
    }
    assert!(matches!(
        run(&w, &request("panic")).unwrap_err(),
        RunnerError::Sandbox(SandboxError::Trap { .. })
    ));
    let mut spin = request("spin");
    spin.limits.fuel = 5_000_000;
    assert!(matches!(
        run(&w, &spin).unwrap_err(),
        RunnerError::Sandbox(SandboxError::FuelExhausted { limit: 5_000_000 })
    ));
    let mut write = request("write-out");
    write.limits.max_output_entries = 4;
    write.limits.max_output_bytes = 1024;
    assert!(matches!(
        run(&w, &write).unwrap_err(),
        RunnerError::OutputFilesWritten { files } if files[0].path == "stray.txt"
    ));
    assert!(matches!(
        run(&w, &request("no-such-entry")).unwrap_err(),
        RunnerError::GuestFailed { .. }
    ));
    assert_eq!(w.records(), 0);
}

/// A failed run consumes and closes the expansion it minted: it can never
/// be finished later or serve as a parent site. Nesting under an accepted
/// expansion works and intersects both macros' policies.
#[test]
fn failed_runs_close_their_expansion() {
    let w = World::new();
    let pure = w.template("t", "hash_into(1)");
    let ok_req = w.template_request(&pure);
    let fail_req = request("fail");
    let mut ctx = context(&ok_req, vec![], vec![]);
    // OTHER_MACRO runs the failing entry; DERIVE_MACRO the pure template.
    ctx.bindings.push(MacroBinding::for_component(
        OTHER_MACRO,
        &fail_req.component,
        fail_req.kind,
        &fail_req.entry,
    ));
    ctx.macro_policies.push(MacroPolicy {
        macro_def: OTHER_MACRO,
        version: 1,
        emits: vec![],
    });
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    let call = |macro_def, site, request| MacroCall {
        macro_def,
        site,
        input: vec![],
        before: vec![],
        after: vec![],
        request,
    };
    let err = s
        .expand(
            &w.runner,
            call(OTHER_MACRO, InvocationSite::Source(APP), &fail_req),
            ExecOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, RunnerError::GuestFailed { .. }));
    let failed: ExpansionId =
        serde_json::from_value(serde_json::json!({"session": 1, "index": 0})).unwrap();
    assert!(
        s.host().expansion(failed).is_ok(),
        "the failed run minted it"
    );

    let parent = s
        .expand(
            &w.runner,
            call(DERIVE_MACRO, InvocationSite::Source(APP), &ok_req),
            ExecOptions::default(),
        )
        .unwrap();
    assert_eq!(parent.expansion.unwrap().index(), 1);
    // The failed expansion cannot be a parent site.
    let under_failed = s
        .expand(
            &w.runner,
            call(DERIVE_MACRO, InvocationSite::Expansion(failed), &ok_req),
            ExecOptions::default(),
        )
        .unwrap();
    assert!(matches!(
        under_failed.rejection(),
        Some(Rejection::Hygiene(ForgeError::ExpansionNotFinished(x))) if *x == failed
    ));
    // Nesting under the accepted expansion is supported.
    let nested = s
        .expand(
            &w.runner,
            call(
                DERIVE_MACRO,
                InvocationSite::Expansion(parent.expansion.unwrap()),
                &ok_req,
            ),
            ExecOptions::default(),
        )
        .unwrap();
    let audit = &nested
        .accepted()
        .expect("nested pure output")
        .accepted
        .audit;
    assert_eq!(audit.policies.len(), 2, "both expansions are stamped");
    assert_eq!(w.records(), 1, "only the successful execution is recorded");
}

#[test]
fn requests_beyond_the_runner_caps_are_refused_before_running() {
    let w = World::new();
    let mut big = request("template");
    big.limits.transport.max_tokens = w.runner.caps().transport.max_tokens + 1;
    assert!(matches!(
        run(&w, &big).unwrap_err(),
        RunnerError::Request(RequestError::LimitAboveCap {
            field: "transport.max_tokens",
            ..
        })
    ));
    let mut mem = request("template");
    mem.limits.max_memory_bytes = w.runner.caps().max_memory_bytes + 1;
    assert!(matches!(
        run(&w, &mem).unwrap_err(),
        RunnerError::Request(RequestError::LimitAboveCap { .. })
    ));
    let mut zero = request("template");
    zero.limits.fuel = 0;
    assert!(matches!(
        run(&w, &zero).unwrap_err(),
        RunnerError::Request(RequestError::ZeroLimit { field: "fuel" })
    ));
    for guest in ["/out", "/out/x", "in", "/in/../x", "/"] {
        let r = request("template").with_input(w.dir.path(), guest);
        assert!(
            matches!(
                run(&w, &r).unwrap_err(),
                RunnerError::Request(RequestError::Mount { .. })
            ),
            "{guest}"
        );
    }
    let nested = request("template")
        .with_input(w.template("a", ""), "/in/a")
        .with_input(w.template("b", ""), "/in/a/b");
    assert!(matches!(
        run(&w, &nested).unwrap_err(),
        RunnerError::Request(RequestError::Mount { .. })
    ));
    // Inputs may not overlap the store or scratch area.
    let r = request("template").with_input(w.runner.store().root(), "/in/store");
    assert!(matches!(
        run(&w, &r).unwrap_err(),
        RunnerError::Request(RequestError::LayoutOverlap { .. })
    ));
    // A record bound larger than the cap is refused up front.
    let tight = RunnerLimits {
        max_record_bytes: 1024,
        ..RunnerLimits::default()
    };
    let runner = w.reopen(engine()).with_caps(tight);
    let req = request("template");
    let ctx = context(&req, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    assert!(matches!(
        expand(&mut s, &runner, &req, "", "", ExecOptions::default()).unwrap_err(),
        RunnerError::Request(RequestError::RecordBoundTooLarge { .. })
    ));
    assert_eq!(w.records(), 0);
}

#[test]
fn input_token_stream_limits_apply_before_execution() {
    let w = World::new();
    let dir = w.template("t", "$input");
    let mut req = w.template_request(&dir);
    req.limits.transport.max_tokens = 2;
    let ctx = context(&req, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    let before = w.executions();
    let err = expand(&mut s, &w.runner, &req, "a b c", "", ExecOptions::default()).unwrap_err();
    assert!(matches!(
        transport(&err),
        TransportViolation::TooManyTokens { count: 3, limit: 2 }
    ));
    assert_eq!(w.executions(), before);
}

#[test]
fn execute_revalidates_against_the_executing_runners_caps() {
    let w = World::new();
    let dir = w.template("t", "hash_into(1)");
    let req = w.template_request(&dir);
    let prepared = w.runner.prepare(&req, vec![]).unwrap();
    let caps = RunnerLimits {
        max_memory_bytes: req.limits.max_memory_bytes - 1,
        ..RunnerLimits::default()
    };
    let strict = w.reopen(engine()).with_caps(caps);
    assert!(matches!(
        strict
            .execute(&prepared, ExecOptions::default())
            .unwrap_err(),
        RunnerError::Request(RequestError::LimitAboveCap {
            field: "max_memory_bytes",
            ..
        })
    ));
    assert_eq!(w.records(), 0);
}
