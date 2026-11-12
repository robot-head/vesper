//! End-to-end Forge gate over untrusted flat sandbox output.
//!
//! A privileged application (holding `Net`) invokes a `derive` macro whose
//! package declares `emits = []`. The "guest" output is built as untrusted
//! `vesper-loom-sandbox` transport tokens. It forges caller scopes and spans
//! and injects a call to a dependency that transitively performs `Net`. The
//! host discards the guest's provenance claims, resolves bindings and
//! globals, and runs the authority gate against a trusted resolved graph.
//!
//! Run: `cargo run --example forge_gate`

use vesper_forge::*;
use vesper_loom_sandbox::transport::{self as t, Outcome, StepKind, TransportLimits};

const APP: PackageId = PackageId(0);
const HASH: PackageId = PackageId(1);
const DERIVE: PackageId = PackageId(2);
const NET: PackageId = PackageId(3);
const UTIL: PackageId = PackageId(4);

const HASH_INTO: DefId = DefId {
    package: HASH,
    item: 0,
};
const MACRO: DefId = DefId {
    package: DERIVE,
    item: 0,
};
const CONNECT: DefId = DefId {
    package: NET,
    item: 0,
};
const TELEMETRY: DefId = DefId {
    package: UTIL,
    item: 0,
};

fn registry() -> Result<DefinitionRegistry, ForgeError> {
    let mut r = DefinitionRegistry::new();
    for (id, name, deps) in [
        (APP, "app", vec![DERIVE, NET]),
        (HASH, "@vesper-wire/hash", vec![]),
        (DERIVE, "@vesper-wire/derive", vec![HASH, UTIL]),
        (NET, "@std/net", vec![]),
        (UTIL, "@util/telemetry", vec![NET]),
    ] {
        r.add_package(PackageInfo {
            id,
            name: name.into(),
            dependencies: deps.into_iter().collect(),
        })?;
    }
    for (id, path, kind) in [
        (HASH_INTO, "@vesper-wire/hash::hash_into", DefKind::Function),
        (
            MACRO,
            "@vesper-wire/derive::derive_wire_hash",
            DefKind::Macro,
        ),
        (CONNECT, "@std/net::connect", DefKind::EffectOp),
        (TELEMETRY, "@util/telemetry::report", DefKind::Function),
    ] {
        r.add_definition(DefInfo {
            id,
            path: path.into(),
            kind,
            visibility: Visibility::Public,
        })?;
    }
    Ok(r)
}

fn graph() -> Result<AuthorityGraph, ForgeError> {
    let net = Authority::new(EffectId::new(NET, "Net"));
    let mut g = AuthorityGraph::new("example-graph-1");
    g.add_node(AuthorityNode::new(HASH_INTO))?;
    g.add_node(AuthorityNode::new(CONNECT).direct(net))?;
    g.add_node(AuthorityNode::new(TELEMETRY).calls(CONNECT))?;
    Ok(g)
}

fn policies(emits: Vec<AuthorityGrant>) -> PolicySet {
    let net = Authority::new(EffectId::new(NET, "Net"));
    PolicySet::new()
        .macro_policy(MacroPolicy {
            macro_def: MACRO,
            version: 1,
            emits,
        })
        .ceiling(CallerCeiling {
            package: APP,
            version: 1,
            grants: vec![AuthorityGrant::Exact(net)],
        })
}

/// What an untrusted guest might return: a hash call plus an injected
/// telemetry call, all claiming caller scope 0 and a caller span.
fn guest_outcome(forged_scope: u32) -> Outcome {
    let span = t::Span {
        file: 1,
        start: 40,
        end: 52,
    };
    let tok = |kind, text: &str| t::Token {
        kind,
        text: text.into(),
        span,
        scopes: vec![forged_scope],
        pre_resolved: None,
    };
    use t::TokenKind::*;
    let paren = t::Delimiter::Paren;
    let mut report = tok(Ident, "@util/telemetry::report");
    report.pre_resolved = Some(t::DefId {
        package: TELEMETRY.package.0,
        item: TELEMETRY.item,
    });
    Outcome {
        tokens: vec![
            tok(Ident, "let"),
            tok(Ident, "h"),
            tok(Punct, "="),
            tok(Ident, "hash_into"),
            tok(Open(paren), "("),
            tok(Ident, "value"),
            tok(Close(paren), ")"),
            tok(Punct, ";"),
            report,
            tok(Open(paren), "("),
            tok(Ident, "h"),
            tok(Close(paren), ")"),
        ],
        diagnostics: vec![],
    }
}

fn expand(emits: Vec<AuthorityGrant>) -> Result<(), Box<dyn std::error::Error>> {
    let mut host = HygieneHost::new(SessionId(1), registry()?);
    host.declare_global(DERIVE, "hash_into", HASH_INTO)?;
    // Caller source: `let value = 7; derive!(value)`.
    let mut program = host.lex_source(APP, "let value = 7;")?;
    let arg = host.lex_source(APP, "value")?;
    let inv = host.invocation_for(StepKind::DeriveMacro, "derive_wire_hash", &arg);
    println!(
        "  guest receives {} tokens with no host scopes",
        inv.tokens.len()
    );

    let expansion = host.begin_expansion(MACRO, InvocationSite::Source(APP), vec![arg])?;
    let caller_scope = host.module_scope(APP)?.index();
    let outcome = guest_outcome(caller_scope);
    // Flat output: everything is macro-generated, guest scopes are dropped.
    let generated =
        host.finish_flat_outcome(expansion.id, &outcome, &TransportLimits::default())?;
    program.extend(generated);

    // The guest's `value` is not the caller's: it cannot see caller locals.
    match host.resolve(program.clone()) {
        Err(e) => println!("  hygiene: {e}"),
        Ok(_) => return Err("forged caller identifier resolved".into()),
    }

    // A well-behaved structured macro splices the caller's syntax instead.
    let arg = host.lex_source(APP, "value")?;
    let expansion = host.begin_expansion(MACRO, InvocationSite::Source(APP), vec![arg])?;
    let q = Quotation::new()
        .text("let h = hash_into(")
        .splice(expansion.splices[0])
        .text(");")
        .token(GeneratedToken::ident("@util/telemetry::report").with_def(TELEMETRY))
        .text("(h)");
    let mut program = host.lex_source(APP, "let value = 7;")?;
    program.extend(host.finish_quotation(expansion.id, &q)?);
    let fragment = host.resolve(program)?;
    for r in &fragment.references {
        println!("  {} ({:?}) -> {:?}", r.name, r.origin, r.resolution);
    }
    match check_authority(host.session(), fragment, &graph()?, &policies(emits)) {
        Ok(accepted) => println!("  accepted; audit {}", accepted.audit.to_json()),
        Err(rejection) => println!("  rejected:\n  {rejection}"),
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("macro declares emits = [] in a Net-holding caller:");
    expand(vec![])?;
    println!("macro declares emits = [Net]:");
    expand(vec![AuthorityGrant::Exact(Authority::new(EffectId::new(
        NET, "Net",
    )))])?;
    Ok(())
}
