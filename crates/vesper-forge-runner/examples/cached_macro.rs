//! Runs the committed macro fixture twice through the cached runner and
//! shows that the second (cached) use is approved afresh:
//!
//! ```text
//! cargo run --example cached_macro
//! ```
//!
//! The macro emits `connect()` (a Net effect operation). The first session's
//! context grants Net in both the macro's `emits` and the caller's ceiling;
//! the second reuses the raw execution under a context whose `emits` policy
//! was emptied without changing its version label, and is rejected.

use std::sync::Arc;

use vesper_forge::{
    Authority, AuthorityGrant, AuthorityGraph, AuthorityNode, CallerCeiling, DefId, DefInfo,
    DefKind, DefinitionRegistry, EffectId, InvocationSite, MacroPolicy, PackageId, PackageInfo,
    SessionId, Visibility,
};
use vesper_forge_runner::{
    ApprovalContext, ExecOptions, GlobalDecl, MacroBinding, MacroCall, MacroEngine, MacroKind,
    MacroRequest, MacroRunner, Verdict,
};

const COMPONENT: &[u8] = include_bytes!("../fixtures/bin/macro.component.wasm");
const APP: PackageId = PackageId(0);
const MACROS: PackageId = PackageId(1);
const NET: PackageId = PackageId(2);
const MACRO: DefId = DefId {
    package: MACROS,
    item: 0,
};
const CONNECT: DefId = DefId {
    package: NET,
    item: 0,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let inputs = dir.path().join("tpl");
    std::fs::create_dir(&inputs)?;
    std::fs::write(inputs.join("t.vsp"), "connect()")?;

    let net = Authority::new(EffectId::new(NET, "Net"));
    let mut registry = DefinitionRegistry::new();
    for (id, name, deps) in [
        (APP, "app", vec![MACROS]),
        (MACROS, "macros", vec![NET]),
        (NET, "@std/net", vec![]),
    ] {
        registry.add_package(PackageInfo {
            id,
            name: name.into(),
            dependencies: deps.into_iter().collect(),
        })?;
    }
    for (id, path, kind) in [
        (MACRO, "macros::emit_connect", DefKind::Macro),
        (CONNECT, "@std/net::connect", DefKind::EffectOp),
    ] {
        registry.add_definition(DefInfo {
            id,
            path: path.into(),
            kind,
            visibility: Visibility::Public,
        })?;
    }
    let mut graph = AuthorityGraph::new("graph-v1");
    graph.add_node(AuthorityNode::new(CONNECT).direct(net.clone()))?;

    let request = MacroRequest::new(COMPONENT.to_vec(), MacroKind::ProcMacro, "template")
        .with_args(["/in/tpl/t.vsp"])
        .with_input(&inputs, "/in/tpl");
    let mut context = ApprovalContext::new(registry, graph);
    context.globals = vec![GlobalDecl {
        package: MACROS,
        name: "connect".into(),
        def: CONNECT,
    }];
    context.macro_policies = vec![MacroPolicy {
        macro_def: MACRO,
        version: 1,
        emits: vec![AuthorityGrant::Exact(net.clone())],
    }];
    context.ceilings = vec![CallerCeiling {
        package: APP,
        version: 1,
        grants: vec![AuthorityGrant::Exact(net)],
    }];
    context.bindings = vec![MacroBinding::for_component(
        MACRO,
        COMPONENT,
        MacroKind::ProcMacro,
        "template",
    )];

    let engine = Arc::new(MacroEngine::new()?);
    let runner = MacroRunner::open(
        engine.clone(),
        dir.path().join("store"),
        dir.path().join("scratch"),
    )?;
    let mut stricter = context.clone();
    stricter.macro_policies[0].emits.clear(); // same version label 1

    for (session, ctx) in [(1, &context), (2, &stricter)] {
        let mut s = ctx.open_session(SessionId(session))?;
        let outcome = s.expand(
            &runner,
            MacroCall {
                macro_def: MACRO,
                site: InvocationSite::Source(APP),
                input: vec![],
                before: vec![],
                after: vec![],
                request: &request,
            },
            ExecOptions::default(),
        )?;
        let ex = outcome.execution.as_ref().expect("executed or hit");
        println!(
            "session {session}: execution {:?} (executed: {}, raw output {}), approval context {}",
            ex.status, ex.executed, ex.raw_output, outcome.approval_context
        );
        match &outcome.verdict {
            Verdict::Accepted(a) => println!("  accepted: {}", a.accepted.audit.to_json()),
            Verdict::Rejected(r) => println!("  rejected: {r:?}"),
        }
    }
    println!("guest executions: {}", engine.executions());
    Ok(())
}
