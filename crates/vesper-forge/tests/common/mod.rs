#![allow(dead_code)]
//! Shared fixture world for Forge tests.

use std::collections::BTreeSet;

use vesper_forge::*;

pub const APP: PackageId = PackageId(0);
pub const HASH: PackageId = PackageId(1);
pub const DERIVE: PackageId = PackageId(2);
pub const NET: PackageId = PackageId(3);
pub const FFI: PackageId = PackageId(4);
pub const UTIL: PackageId = PackageId(5);
pub const OTHER: PackageId = PackageId(6);

pub const fn d(p: PackageId, item: u32) -> DefId {
    DefId { package: p, item }
}

pub const APP_PRINT: DefId = d(APP, 0);
pub const HASH_INTO: DefId = d(HASH, 0);
pub const DERIVE_MACRO: DefId = d(DERIVE, 0);
pub const PHONE_HOME: DefId = d(DERIVE, 1);
pub const NET_MACRO: DefId = d(DERIVE, 2);
pub const CONNECT: DefId = d(NET, 0);
pub const NET_EFFECT: DefId = d(NET, 1);
pub const TCP_HANDLER: DefId = d(NET, 2);
pub const FFI_CALL_A: DefId = d(FFI, 0);
pub const FFI_CALL_B: DefId = d(FFI, 1);
pub const LIB_A: DefId = d(FFI, 10);
pub const LIB_B: DefId = d(FFI, 11);
pub const SNEAKY: DefId = d(UTIL, 0);
pub const CYCLE_A: DefId = d(UTIL, 1);
pub const CYCLE_B: DefId = d(UTIL, 2);
pub const QUIET: DefId = d(UTIL, 3);
pub const LATE: DefId = d(UTIL, 4);
pub const GHOST: DefId = d(UTIL, 5);
pub const UTIL_PRIVATE: DefId = d(UTIL, 9);
pub const OTHER_FN: DefId = d(OTHER, 0);

pub fn net() -> Authority {
    Authority::new(EffectId::new(NET, "Net"))
}

pub fn ffi(lib: DefId) -> Authority {
    Authority::new(EffectId::new(FFI, "Ffi")).with_arg(AuthorityArg::Def(lib))
}

fn pkg(id: PackageId, name: &str, deps: &[PackageId]) -> PackageInfo {
    PackageInfo {
        id,
        name: name.into(),
        dependencies: deps.iter().copied().collect(),
    }
}

fn def(id: DefId, path: &str, kind: DefKind) -> DefInfo {
    DefInfo {
        id,
        path: path.into(),
        kind,
        visibility: Visibility::Public,
    }
}

pub fn registry() -> DefinitionRegistry {
    let mut r = DefinitionRegistry::new();
    for p in [
        pkg(APP, "app", &[DERIVE, NET, FFI, HASH]),
        pkg(HASH, "@vesper-wire/hash", &[]),
        pkg(DERIVE, "@vesper-wire/derive", &[HASH, NET, UTIL, FFI]),
        pkg(NET, "@std/net", &[]),
        pkg(FFI, "@std/ffi", &[]),
        pkg(UTIL, "@util/misc", &[NET]),
        pkg(OTHER, "other", &[]),
    ] {
        r.add_package(p).unwrap();
    }
    use DefKind::*;
    for x in [
        def(APP_PRINT, "app::print", Function),
        def(HASH_INTO, "@vesper-wire/hash::hash_into", Function),
        def(DERIVE_MACRO, "@vesper-wire/derive::derive_wire_hash", Macro),
        def(PHONE_HOME, "@vesper-wire/derive::phone_home", Function),
        def(NET_MACRO, "@vesper-wire/derive::net_macro", Macro),
        def(CONNECT, "@std/net::connect", EffectOp),
        def(NET_EFFECT, "@std/net::Net", Effect),
        def(TCP_HANDLER, "@std/net::tcp_handler", Handler),
        def(FFI_CALL_A, "@std/ffi::call_a", Function),
        def(FFI_CALL_B, "@std/ffi::call_b", Function),
        def(SNEAKY, "@util/misc::sneaky", Function),
        def(CYCLE_A, "@util/misc::cycle_a", Function),
        def(CYCLE_B, "@util/misc::cycle_b", Function),
        def(QUIET, "@util/misc::quiet", Function),
        def(LATE, "@util/misc::late", Function),
        def(GHOST, "@util/misc::ghost", Function),
        def(OTHER_FN, "other::f", Function),
    ] {
        r.add_definition(x).unwrap();
    }
    r.add_definition(DefInfo {
        id: UTIL_PRIVATE,
        path: "@util/misc::private".into(),
        kind: Function,
        visibility: Visibility::Package,
    })
    .unwrap();
    r
}

pub fn host() -> HygieneHost {
    host_session(SessionId(7))
}

pub fn host_session(session: SessionId) -> HygieneHost {
    let mut h = HygieneHost::new(session, registry());
    // Caller environment.
    h.declare_global(APP, "print", APP_PRINT).unwrap();
    h.declare_global(APP, "connect", CONNECT).unwrap();
    h.declare_global(APP, "hash_into", HASH_INTO).unwrap();
    // Macro definition-site environment.
    h.declare_global(DERIVE, "hash_into", HASH_INTO).unwrap();
    h.declare_global(DERIVE, "phone_home", PHONE_HOME).unwrap();
    h
}

pub fn graph() -> AuthorityGraph {
    let mut g = AuthorityGraph::new("graph-v1");
    let n = AuthorityNode::new;
    for node in [
        n(APP_PRINT),
        n(HASH_INTO),
        n(PHONE_HOME).calls(CONNECT),
        n(CONNECT).direct(net()),
        n(TCP_HANDLER).direct(net()),
        n(FFI_CALL_A).direct(ffi(LIB_A)),
        n(FFI_CALL_B).direct(ffi(LIB_B)),
        n(SNEAKY).calls(CYCLE_A),
        n(CYCLE_A).calls(CYCLE_B),
        n(CYCLE_B).calls(CYCLE_A).calls(CONNECT),
        AuthorityNode {
            def: QUIET,
            direct: [net()].into_iter().collect(),
            // Net is handled locally: the residual row is empty.
            residual_row: BTreeSet::new(),
            edges: vec![GraphEdge::Handler {
                effect: NET_EFFECT,
                handler: TCP_HANDLER,
            }],
        },
        n(LATE).edge(GraphEdge::Unresolved {
            note: "late-bound method `send`".into(),
        }),
    ] {
        g.add_node(node).unwrap();
    }
    g.add_handler_rule(HandlerRule {
        effect: NET_EFFECT,
        requires: [net()].into_iter().collect(),
    })
    .unwrap();
    g
}

pub fn emits(macro_def: DefId, grants: Vec<AuthorityGrant>) -> MacroPolicy {
    MacroPolicy {
        macro_def,
        version: 3,
        emits: grants,
    }
}

pub fn ceiling(package: PackageId, grants: Vec<AuthorityGrant>) -> CallerCeiling {
    CallerCeiling {
        package,
        version: 11,
        grants,
    }
}

pub fn exact(a: Authority) -> AuthorityGrant {
    AuthorityGrant::Exact(a)
}

/// Privileged caller (holds Net and any Ffi), macro with the given emits.
pub fn policies(macro_emits: Vec<AuthorityGrant>) -> PolicySet {
    PolicySet::new()
        .macro_policy(emits(DERIVE_MACRO, macro_emits))
        .ceiling(ceiling(
            APP,
            vec![
                exact(net()),
                AuthorityGrant::AnyArguments(EffectId::new(FFI, "Ffi")),
            ],
        ))
}

/// Expands `DERIVE_MACRO` at the app's source with `quote`, returning all
/// tokens of `prefix ++ expansion`.
pub fn expand_in_app(
    h: &mut HygieneHost,
    prefix: &str,
    args: &[&str],
    build: impl FnOnce(&Expansion) -> Quotation,
) -> Result<Vec<HostToken>, ForgeError> {
    let mut program = h.lex_source(APP, prefix)?;
    let args = args
        .iter()
        .map(|a| h.lex_source(APP, a))
        .collect::<Result<Vec<_>, _>>()?;
    let x = h.begin_expansion(DERIVE_MACRO, InvocationSite::Source(APP), args)?;
    let q = build(&x);
    program.extend(h.finish_quotation(x.id, &q)?);
    Ok(program)
}

/// A generated identifier carrying a pre-resolved claim on its registry path.
pub fn claimed(path: &str, def: DefId) -> GeneratedToken {
    GeneratedToken::ident(path).with_def(def)
}

/// Lexed generated tokens, with `path` tokens joined into one identifier
/// token each (claims are attached to whole identifier tokens).
pub fn call_claimed(path: &str, def: DefId) -> Quotation {
    Quotation::new().token(claimed(path, def)).text("()")
}
