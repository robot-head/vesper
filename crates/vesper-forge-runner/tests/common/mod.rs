#![allow(dead_code)]
//! Shared fixture world: a small trusted registry/graph, the committed real
//! macro component and per-test stores.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use vesper_forge::*;
use vesper_forge_runner::*;

pub const APP: PackageId = PackageId(0);
pub const HASH: PackageId = PackageId(1);
pub const DERIVE: PackageId = PackageId(2);
pub const NET: PackageId = PackageId(3);
pub const UTIL: PackageId = PackageId(5);
pub const OTHER: PackageId = PackageId(6);

pub const fn d(p: PackageId, item: u32) -> DefId {
    DefId { package: p, item }
}

pub const APP_PRINT: DefId = d(APP, 0);
pub const HASH_INTO: DefId = d(HASH, 0);
pub const DERIVE_MACRO: DefId = d(DERIVE, 0);
pub const PHONE_HOME: DefId = d(DERIVE, 1);
pub const OTHER_MACRO: DefId = d(DERIVE, 2);
pub const CONNECT: DefId = d(NET, 0);
pub const NET_EFFECT: DefId = d(NET, 1);
pub const TCP_HANDLER: DefId = d(NET, 2);
pub const QUIET: DefId = d(UTIL, 3);
pub const UTIL_PRIVATE: DefId = d(UTIL, 9);
pub const OTHER_FN: DefId = d(OTHER, 0);

pub const COMPONENT: &[u8] = include_bytes!("../../fixtures/bin/macro.component.wasm");
pub const SHA256SUMS: &str = include_str!("../../fixtures/bin/SHA256SUMS");

pub fn net() -> Authority {
    Authority::new(EffectId::new(NET, "Net"))
}

pub fn exact(a: Authority) -> AuthorityGrant {
    AuthorityGrant::Exact(a)
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
        pkg(APP, "app", &[DERIVE, NET, HASH]),
        pkg(HASH, "@vesper-wire/hash", &[]),
        pkg(DERIVE, "@vesper-wire/derive", &[HASH, NET, UTIL]),
        pkg(NET, "@std/net", &[]),
        pkg(UTIL, "@util/misc", &[NET]),
        pkg(OTHER, "other", &[]),
    ] {
        r.add_package(p).unwrap();
    }
    use DefKind::*;
    for x in [
        def(APP_PRINT, "app::print", Function),
        def(HASH_INTO, "@vesper-wire/hash::hash_into", Function),
        def(DERIVE_MACRO, "@vesper-wire/derive::derive_wire", Macro),
        def(PHONE_HOME, "@vesper-wire/derive::phone_home", Function),
        def(OTHER_MACRO, "@vesper-wire/derive::other_macro", Macro),
        def(CONNECT, "@std/net::connect", EffectOp),
        def(NET_EFFECT, "@std/net::Net", Effect),
        def(TCP_HANDLER, "@std/net::tcp_handler", Handler),
        def(QUIET, "@util/misc::quiet", Function),
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

pub fn globals() -> Vec<GlobalDecl> {
    let g = |package, name: &str, def| GlobalDecl {
        package,
        name: name.into(),
        def,
    };
    vec![
        // Caller environment.
        g(APP, "print", APP_PRINT),
        g(APP, "hash_into", HASH_INTO),
        g(APP, "connect", CONNECT),
        // Macro definition-site environment.
        g(DERIVE, "hash_into", HASH_INTO),
        g(DERIVE, "phone_home", PHONE_HOME),
        g(DERIVE, "connect", CONNECT),
        g(DERIVE, "quiet", QUIET),
        g(DERIVE, "Net", NET_EFFECT),
        g(DERIVE, "tcp_handler", TCP_HANDLER),
    ]
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
        n(OTHER_FN),
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

/// Context binding `request`'s component to `DERIVE_MACRO`, with the given
/// macro `emits` and app ceiling.
pub fn context(
    request: &MacroRequest,
    emits: Vec<AuthorityGrant>,
    ceiling: Vec<AuthorityGrant>,
) -> ApprovalContext {
    let mut c = ApprovalContext::new(registry(), graph());
    c.globals = globals();
    c.macro_policies = vec![MacroPolicy {
        macro_def: DERIVE_MACRO,
        version: 3,
        emits,
    }];
    c.ceilings = vec![CallerCeiling {
        package: APP,
        version: 11,
        grants: ceiling,
    }];
    c.bindings = vec![MacroBinding::for_component(
        DERIVE_MACRO,
        &request.component,
        request.kind,
        &request.entry,
    )];
    c
}

pub fn engine() -> Arc<MacroEngine> {
    static ENGINE: OnceLock<Arc<MacroEngine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| Arc::new(MacroEngine::new().unwrap()))
        .clone()
}

/// A temporary store, scratch area and inputs directory.
pub struct World {
    pub dir: tempfile::TempDir,
    pub runner: MacroRunner,
}

impl World {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // A private engine per world keeps execution counters exact while
        // tests run in parallel.
        let engine = Arc::new(MacroEngine::new().unwrap());
        let runner =
            MacroRunner::open(engine, dir.path().join("store"), dir.path().join("scratch"))
                .unwrap();
        Self { dir, runner }
    }

    pub fn reopen(&self, engine: Arc<MacroEngine>) -> MacroRunner {
        MacroRunner::open(
            engine,
            self.dir.path().join("store"),
            self.dir.path().join("scratch"),
        )
        .unwrap()
    }

    /// Writes `text` as `<name>/t.vsp` and returns the directory.
    pub fn template(&self, name: &str, text: &str) -> PathBuf {
        let dir = self.dir.path().join("inputs").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.vsp"), text).unwrap();
        dir
    }

    /// A `template` request reading `/in/tpl/t.vsp` from `dir`.
    pub fn template_request(&self, dir: &Path) -> MacroRequest {
        MacroRequest::new(COMPONENT.to_vec(), MacroKind::ProcMacro, "template")
            .with_args(["/in/tpl/t.vsp"])
            .with_input(dir, "/in/tpl")
    }

    pub fn executions(&self) -> u64 {
        self.runner.engine().executions()
    }

    /// Number of published execution records.
    pub fn records(&self) -> usize {
        fn count(p: &Path) -> usize {
            match std::fs::read_dir(p) {
                Err(_) => 0,
                Ok(rd) => rd
                    .flatten()
                    .map(|e| {
                        let t = e.file_type().unwrap();
                        if t.is_dir() { count(&e.path()) } else { 1 }
                    })
                    .sum(),
            }
        }
        count(&self.runner.store().root().join("derivations"))
    }
}

/// Expands `DERIVE_MACRO` at app source, with `input` as the macro input
/// and `before` lexed before the expansion.
pub fn expand(
    session: &mut ForgeSession,
    runner: &MacroRunner,
    request: &MacroRequest,
    input: &str,
    before: &str,
    options: ExecOptions,
) -> Result<MacroOutcome, RunnerError> {
    let input = session.lex_source(APP, input).unwrap();
    let before = session.lex_source(APP, before).unwrap();
    session.expand(
        runner,
        MacroCall {
            macro_def: DERIVE_MACRO,
            site: InvocationSite::Source(APP),
            input,
            before,
            after: Vec::new(),
            request,
        },
        options,
    )
}

pub fn texts(session: &ForgeSession, tokens: &[HostToken]) -> Vec<String> {
    tokens
        .iter()
        .map(|t| session.host().text(t.text()).to_owned())
        .collect()
}

pub fn exceeds_by(outcome: &MacroOutcome) -> Vec<Denial> {
    match outcome.rejection() {
        Some(Rejection::Authority(r)) => r
            .violations
            .iter()
            .flat_map(|v| match v {
                Violation::Exceeds { denied_by, .. } => denied_by.clone(),
                _ => vec![],
            })
            .collect(),
        other => panic!("expected an authority rejection, got {other:?}"),
    }
}
