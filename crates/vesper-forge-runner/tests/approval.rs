//! Fresh approval of real component output: privileged emission needs both
//! the macro's `emits` and the caller's ceiling on every use, approval-context
//! content changes are never masked by a cache hit, and guest data never
//! becomes caller provenance.

mod common;

use common::*;
use vesper_forge::*;
use vesper_forge_runner::*;

fn rw() -> ExecOptions {
    ExecOptions::default()
}

/// Expands `template` under (emits, ceiling) and returns the outcome.
fn under(
    w: &World,
    template: &str,
    emits: Vec<AuthorityGrant>,
    ceiling: Vec<AuthorityGrant>,
    session: u64,
) -> MacroOutcome {
    let dir = w.template("t", template);
    let req = w.template_request(&dir);
    let mut s = context(&req, emits, ceiling)
        .open_session(SessionId(session))
        .unwrap();
    expand(&mut s, &w.runner, &req, "", "", rw()).unwrap()
}

#[test]
fn pure_flat_output_needs_no_authority() {
    let w = World::new();
    let o = under(&w, "{ let v = 1; hash_into(v) }", vec![], vec![], 1);
    let a = o.accepted().expect("pure output accepted");
    assert!(
        a.accepted
            .audit
            .operations
            .iter()
            .all(|op| op.requirements.is_empty())
    );
    assert!(a.accepted.audit.source_operations.is_empty());
}

/// Own-package privileged calls, dependency effect operations, locally
/// handled effects and macro-constructed handlers all need `emits` AND the
/// caller ceiling, on the miss and again on every hit.
#[test]
fn privileged_emission_requires_emits_and_ceiling_on_every_use() {
    for template in [
        "phone_home()",                      // own package, calls connect
        "connect()",                         // dependency effect operation
        "quiet()",                           // Net handled locally inside
        "handle Net with tcp_handler { 1 }", // handler construction
    ] {
        let w = World::new();
        // Miss: emits empty, caller holds Net.
        let o = under(&w, template, vec![], vec![exact(net())], 1);
        assert_eq!(o.execution.as_ref().unwrap().status, CacheStatus::Miss);
        assert_eq!(
            exceeds_by(&o),
            [Denial::MacroEmits {
                macro_def: DERIVE_MACRO,
                version: 3
            }],
            "{template}"
        );
        // Hit: emits Net, caller does not hold it.
        let o = under(&w, template, vec![exact(net())], vec![], 2);
        assert_eq!(o.execution.as_ref().unwrap().status, CacheStatus::Hit);
        assert_eq!(
            exceeds_by(&o),
            [Denial::CallerCeiling {
                package: APP,
                version: 11
            }],
            "{template}"
        );
        // Hit: both.
        let o = under(&w, template, vec![exact(net())], vec![exact(net())], 3);
        assert_eq!(o.execution.as_ref().unwrap().status, CacheStatus::Hit);
        let a = o.accepted().unwrap_or_else(|| panic!("{template}: {o:?}"));
        assert!(
            a.accepted
                .audit
                .operations
                .iter()
                .any(|op| op.requirements.contains(&net())),
            "{template}"
        );
        // Hit again, back to a rejecting context: the earlier acceptance is
        // not reused.
        let o = under(&w, template, vec![], vec![exact(net())], 4);
        assert_eq!(o.execution.as_ref().unwrap().status, CacheStatus::Hit);
        assert!(o.accepted().is_none(), "{template}");
    }
}

/// Changing registry, graph, environment or policy *content* under the same
/// version labels changes the approval context and the verdict, even though
/// the raw execution is reused.
#[test]
fn same_label_content_changes_cannot_retain_old_authorization() {
    let w = World::new();
    let dir = w.template("t", "hash_into(1)");
    let req = w.template_request(&dir);
    let base = context(&req, vec![], vec![]);
    let mut s = base.open_session(SessionId(1)).unwrap();
    let accepted = expand(&mut s, &w.runner, &req, "", "", rw()).unwrap();
    assert!(accepted.accepted().is_some());
    assert_eq!(base.id().unwrap(), accepted.approval_context);

    let check = |ctx: ApprovalContext, what: &str| {
        assert_eq!(ctx.graph.graph_version(), "graph-v1");
        assert!(ctx.macro_policies.iter().all(|p| p.version == 3));
        assert!(ctx.ceilings.iter().all(|c| c.version == 11));
        let id = ctx.id().unwrap();
        assert_ne!(id, accepted.approval_context, "{what}: same context id");
        let mut s = ctx.open_session(SessionId(2)).unwrap();
        let before = w.executions();
        let o = expand(&mut s, &w.runner, &req, "", "", rw()).unwrap();
        assert_eq!(w.executions(), before, "{what}: raw execution is reused");
        assert_eq!(o.execution.as_ref().unwrap().status, CacheStatus::Hit);
        assert_eq!(o.approval_context, id);
        assert!(o.accepted().is_none(), "{what}: old approval survived");
        o
    };

    // Graph: `hash_into` now reaches `connect`, label still graph-v1.
    let mut g = AuthorityGraph::new("graph-v1");
    for n in [
        AuthorityNode::new(HASH_INTO).calls(CONNECT),
        AuthorityNode::new(CONNECT).direct(net()),
    ] {
        g.add_node(n).unwrap();
    }
    let mut ctx = base.clone();
    ctx.graph = g;
    let o = check(ctx, "graph");
    assert!(matches!(o.rejection(), Some(Rejection::Authority(_))));

    // Registry: the definition behind the macro's `hash_into` binding is now
    // private to its package. No version label exists to stay the same.
    let mut ctx = base.clone();
    let mut wire = serde_json::to_value(registry()).unwrap();
    for d in wire["definitions"].as_array_mut().unwrap() {
        if d["path"] == "@vesper-wire/hash::hash_into" {
            d["visibility"] = "Package".into();
        }
    }
    ctx.registry = serde_json::from_value(wire).unwrap();
    // Declaring a now-invisible global is refused at session start.
    assert!(matches!(
        ctx.open_session(SessionId(3)),
        Err(ContextError::Global { .. })
    ));
    // Without those declarations the macro's name is unbound.
    ctx.globals
        .retain(|g| g.def != HASH_INTO || g.package == HASH);
    let o = check(ctx, "registry");
    assert!(matches!(
        o.rejection(),
        Some(Rejection::Hygiene(ForgeError::Unbound { .. }))
    ));

    // Policy: same version 3, but a ceiling/emits change matters only for
    // privileged output; use a context whose graph makes it privileged.
    let mut privileged = base.clone();
    privileged.graph = {
        let mut g = AuthorityGraph::new("graph-v1");
        g.add_node(AuthorityNode::new(HASH_INTO).direct(net()))
            .unwrap();
        g
    };
    privileged.macro_policies[0].emits = vec![exact(net())];
    privileged.ceilings[0].grants = vec![exact(net())];
    let mut s = privileged.open_session(SessionId(4)).unwrap();
    let ok = expand(&mut s, &w.runner, &req, "", "", rw()).unwrap();
    assert!(ok.accepted().is_some());
    let mut ctx = privileged.clone();
    ctx.macro_policies[0].emits = vec![];
    let o = check(ctx, "macro policy");
    assert!(matches!(o.rejection(), Some(Rejection::Authority(_))));
    let mut ctx = privileged.clone();
    ctx.ceilings[0].grants = vec![];
    check(ctx, "caller ceiling");
}

#[test]
fn context_ids_cover_content_and_reject_duplicates() {
    let req = MacroRequest::new(COMPONENT.to_vec(), MacroKind::ProcMacro, "template");
    let a = context(&req, vec![], vec![]);
    let mut b = a.clone();
    b.globals.reverse();
    assert_eq!(a.id().unwrap(), b.id().unwrap(), "canonical order");
    let mut c = a.clone();
    c.globals.pop();
    assert_ne!(a.id().unwrap(), c.id().unwrap());
    let mut c = a.clone();
    c.quote_limits.max_depth -= 1;
    assert_ne!(a.id().unwrap(), c.id().unwrap());
    let mut c = a.clone();
    c.bindings[0].entry = "echo".into();
    assert_ne!(a.id().unwrap(), c.id().unwrap());
    let mut dup = a.clone();
    dup.macro_policies.push(dup.macro_policies[0].clone());
    assert_eq!(
        dup.id(),
        Err(ContextError::DuplicateMacroPolicy(DERIVE_MACRO))
    );
    let mut dup = a.clone();
    dup.ceilings.push(dup.ceilings[0].clone());
    assert_eq!(dup.id(), Err(ContextError::DuplicateCeiling(APP)));
}

#[test]
fn unbound_or_mismatched_components_are_rejected_before_execution() {
    let w = World::new();
    let dir = w.template("t", "hash_into(1)");
    let req = w.template_request(&dir);
    let before = w.executions();

    // Bound to another entry of the same component.
    let mut ctx = context(&req, vec![], vec![]);
    ctx.bindings[0].entry = "echo".into();
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    let o = expand(&mut s, &w.runner, &req, "", "", rw()).unwrap();
    assert!(matches!(
        o.rejection(),
        Some(Rejection::UnboundComponent { .. })
    ));
    assert!(o.execution.is_none() && o.expansion.is_none());

    // Different component bytes under the bound macro.
    let mut other = req.clone();
    other
        .component
        .extend_from_slice(&[0x00, 0x04, 0x03, b'v', b'f', b'r']);
    let ctx = context(&req, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(2)).unwrap();
    let o = expand(&mut s, &w.runner, &other, "", "", rw()).unwrap();
    assert!(matches!(
        o.rejection(),
        Some(Rejection::UnboundComponent { .. })
    ));

    // No binding at all.
    let mut ctx = context(&req, vec![], vec![]);
    ctx.bindings.clear();
    let mut s = ctx.open_session(SessionId(3)).unwrap();
    let o = expand(&mut s, &w.runner, &req, "", "", rw()).unwrap();
    assert!(matches!(
        o.rejection(),
        Some(Rejection::UnboundComponent { .. })
    ));
    assert_eq!(w.executions(), before);
    assert_eq!(w.records(), 0);
}

/// The guest stamps the caller's module scope index, a caller span and
/// claims on its output. None of it captures, exempts or authenticates, on
/// the miss or on the hit.
#[test]
fn forged_caller_scopes_spans_and_claims_are_inert() {
    let w = World::new();
    let dir = w.template("t", "x");
    for (session, status) in [(1, CacheStatus::Miss), (2, CacheStatus::Hit)] {
        let mut req = w.template_request(&dir);
        let ctx = context(&req, vec![], vec![]);
        let mut s = ctx.open_session(SessionId(session)).unwrap();
        let caller_scope = s.module_scope(APP).unwrap();
        // The caller's module scope has the same index in both sessions,
        // so the request (and key) is identical and the second run hits.
        req.args = vec![
            "/in/tpl/t.vsp".into(),
            format!("scopes={}", caller_scope.index()),
            "span=4:12:14".into(),
        ];
        let o = expand(&mut s, &w.runner, &req, "", "let x = 1;", rw()).unwrap();
        assert_eq!(o.execution.as_ref().unwrap().status, status);
        assert!(
            matches!(o.rejection(), Some(Rejection::Hygiene(ForgeError::Unbound { name, .. })) if name == "x"),
            "{o:?}"
        );
    }

    // Accepted output keeps the forged span only as display data, and the
    // forged scope is not among the token's scopes.
    let dir = w.template("t2", "hash_into(1)");
    let mut req = w.template_request(&dir);
    req.args.push("scopes=0".into());
    req.args.push("span=4:12:14".into());
    let ctx = context(&req, vec![], vec![]);
    let mut s = ctx.open_session(SessionId(5)).unwrap();
    let caller_scope = s.module_scope(APP).unwrap();
    assert_eq!(caller_scope.index(), 0);
    let o = expand(&mut s, &w.runner, &req, "", "", rw()).unwrap();
    let a = o.accepted().expect("pure output accepted");
    for t in &a.tokens {
        assert!(!t.scopes().contains(caller_scope));
        assert_eq!(t.origin(), Origin::Expansion(o.expansion.unwrap()));
        assert_eq!(
            t.reported_span(),
            Some(ReportedSpan {
                file: 4,
                start: 12,
                end: 14
            })
        );
    }
}

#[test]
fn forged_claims_are_validated_against_the_current_registry() {
    for (template, ok) in [
        // A correct claim on a public dependency definition.
        ("$claim:1:0 @vesper-wire/hash::hash_into ( 1 )", true),
        // Printed name does not match the claimed definition.
        ("$claim:3:0 @vesper-wire/hash::hash_into ( 1 )", false),
        // Private definition of a dependency.
        ("$claim:5:9 @util/misc::private ( )", false),
        // Not a dependency of the macro package.
        ("$claim:6:0 other::f ( )", false),
        // Unknown definition.
        ("$claim:3:77 @std/net::ghost ( )", false),
    ] {
        let w = World::new();
        let o = under(&w, template, vec![], vec![], 1);
        assert_eq!(o.accepted().is_some(), ok, "{template}: {o:?}");
        if !ok {
            assert!(
                matches!(o.rejection(), Some(Rejection::Hygiene(_))),
                "{template}: {o:?}"
            );
        }
        // Claims are re-validated on the hit as well.
        let o = under(&w, template, vec![], vec![], 2);
        assert_eq!(o.execution.as_ref().unwrap().status, CacheStatus::Hit);
        assert_eq!(o.accepted().is_some(), ok, "{template} (hit)");
    }
}

#[test]
fn echoed_caller_tokens_are_macro_originated() {
    let w = World::new();
    let dir = w.template("t", "$input");
    let mut req = w.template_request(&dir);
    req.kind = MacroKind::DeriveMacro;
    let ctx = context(&req, vec![], vec![exact(net())]);
    let mut s = ctx.open_session(SessionId(1)).unwrap();
    // The caller may call `connect` itself (it holds Net), but the echoed
    // call is the macro's emission and the macro has no emits grant.
    let o = expand(&mut s, &w.runner, &req, "connect()", "", rw()).unwrap();
    assert_eq!(
        exceeds_by(&o),
        [Denial::MacroEmits {
            macro_def: DERIVE_MACRO,
            version: 3
        }]
    );
    // A caller-local name echoed back cannot capture the caller binding.
    let o = expand(&mut s, &w.runner, &req, "y", "let y = 1;", rw()).unwrap();
    assert!(matches!(
        o.rejection(),
        Some(Rejection::Hygiene(ForgeError::Unbound { .. }))
    ));
}

#[test]
fn input_tokens_must_belong_to_the_session() {
    let w = World::new();
    let dir = w.template("t", "hash_into(1)");
    let req = w.template_request(&dir);
    let ctx = context(&req, vec![], vec![]);
    let mut old = ctx.open_session(SessionId(1)).unwrap();
    let foreign = old.lex_source(APP, "1").unwrap();
    let mut s = ctx.open_session(SessionId(2)).unwrap();
    let before = w.executions();
    let o = s
        .expand(
            &w.runner,
            MacroCall {
                macro_def: DERIVE_MACRO,
                site: InvocationSite::Source(APP),
                input: foreign,
                before: vec![],
                after: vec![],
                request: &req,
            },
            rw(),
        )
        .unwrap();
    assert!(matches!(
        o.rejection(),
        Some(Rejection::Hygiene(ForgeError::ForeignScope(_)))
    ));
    assert_eq!(w.executions(), before, "nothing ran");
}

#[test]
fn rejected_expansions_still_cache_raw_execution_but_publish_no_approval() {
    let w = World::new();
    // Unbound name in the macro environment: resolution fails.
    let o = under(&w, "nowhere()", vec![], vec![], 1);
    assert!(matches!(
        o.rejection(),
        Some(Rejection::Hygiene(ForgeError::Unbound { .. }))
    ));
    assert!(o.accepted().is_none());
    assert_eq!(w.records(), 1, "raw execution is reusable");
    // Nothing approval-shaped is stored: the only blobs are the component,
    // the invocation, the input snapshot and the raw record.
    let raw = o.execution.unwrap().raw_output;
    let bytes = w.runner.store().read_blob(&raw).unwrap();
    assert!(bytes.starts_with(b"VFRX"));
    let text = String::from_utf8_lossy(&bytes);
    for forbidden in ["session", "expansion", "graph-v1", "audit"] {
        assert!(!text.contains(forbidden), "{forbidden} in raw record");
    }
}
