//! The Vesper build of the hygiene core: thin adapters from the host's
//! types to the Vesper library `forge` (`vesper/`, self-hosting wave SH1
//! lane 14, #1157) through the generated binding (`port/binding.rs`).
//!
//! The host stays in Rust. Each adapter flattens what its kernel reads into
//! word tables, runs the kernel, and replays the identities the kernel
//! minted (joined path symbols, scopes, binders) on the host in the order
//! the Rust code mints them, so both builds leave the host in the same
//! state on success and on every error.

use std::collections::BTreeSet;

use crate::error::{ForgeError, IdentityKind};
use crate::host::HygieneHost;
use crate::identity::{BinderId, ScopeId, ScopeSet, Symbol};
use crate::registry::{DefId, DefKind, PackageId, Visibility};
use crate::resolve::{BinderInfo, EmittedOperation, OperationKind, Resolution, ResolvedRef};
use crate::syntax::{Path, Term};
use crate::token::{Delim, HostToken, Origin, TokenKind};

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, clippy::all)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod binding;

/// The implementation the crate was built with.
pub const IMPLEMENTATION: &str = "vesper";

pub(crate) fn live_allocations() -> i64 {
    binding::live_allocations()
}

fn kind(k: TokenKind) -> u64 {
    match k {
        TokenKind::Ident => 1,
        TokenKind::Punct => 2,
        TokenKind::Int => 3,
        TokenKind::Str => 4,
        TokenKind::Open(Delim::Paren) => 5,
        TokenKind::Open(Delim::Bracket) => 6,
        TokenKind::Open(Delim::Brace) => 7,
        TokenKind::Close(Delim::Paren) => 8,
        TokenKind::Close(Delim::Bracket) => 9,
        TokenKind::Close(Delim::Brace) => 10,
    }
}

fn w(x: usize) -> u64 {
    x as u64
}

fn u(x: u64) -> usize {
    x as usize
}

fn def(package: u64, item: u64) -> DefId {
    DefId {
        package: PackageId(package as u32),
        item: item as u32,
    }
}

fn def_kind(k: u64) -> DefKind {
    match k {
        0 => DefKind::Function,
        1 => DefKind::Effect,
        2 => DefKind::EffectOp,
        3 => DefKind::Handler,
        _ => DefKind::Macro,
    }
}

fn def_kind_code(k: DefKind) -> u64 {
    match k {
        DefKind::Function => 0,
        DefKind::Effect => 1,
        DefKind::EffectOp => 2,
        DefKind::Handler => 3,
        DefKind::Macro => 4,
    }
}

fn depth_error(max_depth: usize) -> ForgeError {
    ForgeError::QuoteLimit {
        what: "nesting depth",
        limit: max_depth,
    }
}

/// `check_delimiters`.
pub(crate) fn check_delimiters(tokens: &[HostToken], max_depth: usize) -> Result<(), ForgeError> {
    let kinds: Vec<u64> = tokens.iter().map(|t| kind(t.kind)).collect();
    match binding::fg_delimiters(&kinds, w(max_depth)) {
        0 => Ok(()),
        1 => Err(depth_error(max_depth)),
        e => Err(ForgeError::UnbalancedDelimiters { token: u(e - 2) }),
    }
}

const EXPECTED: [&str; 8] = [
    "end of input",
    "=",
    ";",
    "with",
    ")",
    "}",
    "{",
    "identifier",
];

/// `check_delimiters` followed by `syntax::parse`.
pub(crate) fn check_and_parse(
    host: &mut HygieneHost,
    tokens: &[HostToken],
) -> Result<Term, ForgeError> {
    let max_depth = host.quote_limits.max_depth;
    let mut rows = Vec::with_capacity(tokens.len() * 3);
    let mut text = Vec::new();
    for t in tokens {
        let s = host.text(t.text).as_bytes();
        rows.extend([kind(t.kind), w(text.len()), w(s.len())]);
        text.extend_from_slice(s);
    }
    let out = binding::fg_parse(&rows, &text, w(max_depth));
    let (err, e1, e2) = (out[0], out[1], out[2]);
    // Intern every finished path in creation order, as the parser does,
    // then report the error the parse stopped at (a failed delimiter check
    // finishes no path).
    let mut at = 5;
    let mut paths = Vec::with_capacity(u(out[3]));
    let mut joined = String::new();
    for _ in 0..out[3] {
        let n = u(out[at]);
        let toks: Vec<usize> = out[at + 1..at + 1 + n].iter().map(|&t| u(t)).collect();
        at += 1 + n;
        joined.clear();
        for (i, &t) in toks.iter().enumerate() {
            if i > 0 {
                joined.push_str("::");
            }
            joined.push_str(host.text(tokens[t].text));
        }
        let symbol = host.intern(&joined)?;
        paths.push(Path {
            tokens: toks,
            symbol,
        });
    }
    match err {
        0 => {}
        1 => return Err(depth_error(max_depth)),
        2 => return Err(ForgeError::UnbalancedDelimiters { token: u(e1) }),
        _ => {
            return Err(ForgeError::Parse {
                token: u(e1),
                expected: EXPECTED[u(e2) - 1],
            });
        }
    }
    let mut stack: Vec<Term> = Vec::new();
    let pop = |stack: &mut Vec<Term>| stack.pop().expect("post-order term");
    while at < out.len() {
        match out[at] {
            1 => {
                stack.push(Term::Ref(paths[u(out[at + 1])].clone()));
                at += 2;
            }
            2 => {
                stack.push(Term::Lit(u(out[at + 1])));
                at += 2;
            }
            3 => {
                let body = pop(&mut stack);
                let value = pop(&mut stack);
                stack.push(Term::Let {
                    binder: u(out[at + 1]),
                    value: Box::new(value),
                    body: Box::new(body),
                });
                at += 2;
            }
            4 => {
                let nargs = u(out[at + 3]);
                let args = stack.split_off(stack.len() - nargs);
                let callee = pop(&mut stack);
                stack.push(Term::Call {
                    callee: Box::new(callee),
                    open: u(out[at + 1]),
                    close: u(out[at + 2]),
                    args,
                });
                at += 4;
            }
            5 => {
                let body = pop(&mut stack);
                stack.push(Term::Handle {
                    keyword: u(out[at + 1]),
                    effect: paths[u(out[at + 2])].clone(),
                    handler: paths[u(out[at + 3])].clone(),
                    body: Box::new(body),
                });
                at += 4;
            }
            _ => {
                let n = u(out[at + 1]);
                let items = stack.split_off(stack.len() - n);
                stack.push(Term::Block(items));
                at += 2;
            }
        }
    }
    debug_assert_eq!(stack.len(), 1);
    Ok(pop(&mut stack))
}

fn encode_path(p: &Path, out: &mut Vec<u64>) {
    out.push(w(p.tokens.len()));
    out.extend(p.tokens.iter().map(|&t| w(t)));
    out.push(u64::from(p.symbol.0));
}

fn encode_term(t: &Term, out: &mut Vec<u64>) {
    match t {
        Term::Ref(p) => {
            out.push(1);
            encode_path(p, out);
        }
        Term::Lit(i) => out.extend([2, w(*i)]),
        Term::Let {
            binder,
            value,
            body,
        } => {
            out.extend([3, w(*binder)]);
            encode_term(value, out);
            encode_term(body, out);
        }
        Term::Call {
            callee,
            open,
            close,
            args,
        } => {
            out.extend([4, w(*open), w(*close), w(args.len())]);
            encode_term(callee, out);
            for a in args {
                encode_term(a, out);
            }
        }
        Term::Handle {
            keyword,
            effect,
            handler,
            body,
        } => {
            out.extend([5, w(*keyword)]);
            encode_path(effect, out);
            encode_path(handler, out);
            encode_term(body, out);
        }
        Term::Block(items) => {
            out.extend([6, w(items.len())]);
            for i in items {
                encode_term(i, out);
            }
        }
    }
}

/// The resolver kernel's host table: the globals, their definitions and
/// the registry's packages (layout in vesper/src/resolve.ves), for a host
/// with `globals` globals.
#[derive(Debug)]
pub(crate) struct HostTables {
    globals: usize,
    words: Vec<u64>,
    text: Vec<u8>,
    defs: Vec<DefId>,
}

fn host_tables(host: &HygieneHost) -> HostTables {
    let defs: Vec<DefId> = host
        .globals
        .iter()
        .map(|g| g.def)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|&d| host.registry.definition(d).is_some())
        .collect();
    let packages: Vec<_> = host.registry.packages().collect();
    let mut words = vec![0u64; 6];
    let mut text = Vec::new();
    words[0] = w(host.globals.len());
    words[1] = w(words.len());
    let global_base = words.len();
    words.resize(global_base + host.globals.len() * 5, 0);
    words[2] = w(defs.len());
    words[3] = w(words.len());
    for &d in &defs {
        let info = host.registry.definition(d).expect("registered definition");
        words.extend(def_row(info, &mut text));
    }
    words[4] = w(packages.len());
    words[5] = w(words.len());
    let pkg_base = words.len();
    words.resize(pkg_base + packages.len() * 3, 0);
    for (i, g) in host.globals.iter().enumerate() {
        let f = w(words.len());
        words.extend(g.scopes.iter().map(|s| u64::from(s.index)));
        let row = global_base + i * 5;
        words[row..row + 5].copy_from_slice(&[
            u64::from(g.symbol.0),
            f,
            w(g.scopes.len()),
            u64::from(g.def.package.0),
            u64::from(g.def.item),
        ]);
    }
    for (i, p) in packages.iter().enumerate() {
        let f = w(words.len());
        words.extend(p.dependencies.iter().map(|d| u64::from(d.0)));
        let row = pkg_base + i * 3;
        words[row..row + 3].copy_from_slice(&[u64::from(p.id.0), f, w(p.dependencies.len())]);
    }
    HostTables {
        globals: host.globals.len(),
        words,
        text,
        defs,
    }
}

fn def_row(info: &crate::registry::DefInfo, text: &mut Vec<u8>) -> [u64; 6] {
    let vis = match info.visibility {
        Visibility::Public => 0,
        Visibility::Package => 1,
    };
    let row = [
        u64::from(info.id.package.0),
        u64::from(info.id.item),
        def_kind_code(info.kind),
        vis,
        w(text.len()),
        w(info.path.len()),
    ];
    text.extend_from_slice(info.path.as_bytes());
    row
}

/// What the resolver kernel returns on success.
pub(crate) struct Resolved {
    pub(crate) binders: Vec<BinderInfo>,
    pub(crate) references: Vec<ResolvedRef>,
    pub(crate) operations: Vec<EmittedOperation>,
}

/// `check_delimiters` followed by the `Resolver` over validated tokens.
pub(crate) fn check_and_resolve(
    host: &mut HygieneHost,
    tokens: &[HostToken],
    term: &Term,
    parameters: &[usize],
) -> Result<Resolved, ForgeError> {
    if host
        .port_tables
        .as_ref()
        .is_none_or(|t| t.globals != host.globals.len())
    {
        host.port_tables = Some(host_tables(host));
    }
    let tables = host.port_tables.take().expect("host tables");
    let result = resolve_with(host, &tables, tokens, term, parameters);
    host.port_tables = Some(tables);
    result
}

fn resolve_with(
    host: &mut HygieneHost,
    tables: &HostTables,
    tokens: &[HostToken],
    term: &Term,
    parameters: &[usize],
) -> Result<Resolved, ForgeError> {
    let max_depth = host.quote_limits.max_depth;
    let mut origins: Vec<Origin> = tokens.iter().map(|t| t.origin).collect();
    origins.sort_unstable();
    origins.dedup();
    let origin_index = |o: Origin| w(origins.binary_search(&o).expect("token origin"));
    // Claimed definitions outside the host table.
    let mut claimed: Vec<DefId> = tokens
        .iter()
        .filter_map(|t| t.claimed_def)
        .filter(|d| tables.defs.binary_search(d).is_err())
        .collect();
    claimed.sort_unstable();
    claimed.dedup();

    let text_len: usize = tokens.iter().map(|t| host.text(t.text).len()).sum();
    let mut text = Vec::with_capacity(text_len);
    let mut words = Vec::with_capacity(18 + tokens.len() * 16 + origins.len() * 2);
    words.resize(18, 0);
    let (next_scope, scope_limit) = host.scope_counter();
    let (next_binder, binder_limit) = host.binder_counter();
    words[0] = w(max_depth);
    words[1] = u64::from(next_scope);
    words[2] = u64::from(scope_limit);
    words[3] = u64::from(next_binder);
    words[4] = u64::from(binder_limit);
    let tok_base = words.len();
    words[5] = w(tokens.len());
    words[6] = w(tok_base);
    words.resize(tok_base + tokens.len() * 10, 0);
    words[7] = w(origins.len());
    words[8] = w(words.len());
    for &o in &origins {
        let rank = match o {
            Origin::Source { .. } => 0,
            Origin::Expansion(x) => u64::from(x.index) + 1,
        };
        let p = host.emitting_package(o).unwrap_or(PackageId(u32::MAX));
        words.extend([rank, u64::from(p.0)]);
    }
    words[11] = w(claimed.len());
    words[12] = w(words.len());
    for &d in &claimed {
        if let Some(info) = host.registry.definition(d) {
            words.extend(def_row(info, &mut text));
        }
    }
    // Claims of unknown definitions have no row.
    words[11] = (w(words.len()) - words[12]) / 6;
    words[15] = w(parameters.len());
    words[16] = w(words.len());
    words.extend(parameters.iter().map(|&p| w(p)));
    words[17] = w(words.len());
    encode_term(term, &mut words);
    for (i, t) in tokens.iter().enumerate() {
        let f = w(words.len());
        words.extend(t.scopes.iter().map(|s| u64::from(s.index)));
        let s = host.text(t.text).as_bytes();
        let (claim, cp, ci) = match t.claimed_def {
            Some(d) => (1, u64::from(d.package.0), u64::from(d.item)),
            None => (0, 0, 0),
        };
        let row = tok_base + i * 10;
        words[row..row + 10].copy_from_slice(&[
            kind(t.kind),
            u64::from(t.text.0),
            origin_index(t.origin),
            claim,
            cp,
            ci,
            f,
            w(t.scopes.len()),
            w(text.len()),
            w(s.len()),
        ]);
        text.extend_from_slice(s);
    }

    let out = binding::fg_resolve(&words, &text, &tables.words, &tables.text);
    // Replay the identities the kernel minted.
    for _ in 0..out[1] {
        host.fresh_scope()?;
    }
    for _ in 0..out[2] {
        host.fresh_binder()?;
    }
    let (e1, e2, e3, e4) = (out[3], out[4], out[5], out[6]);
    let extra = &out[out.len() - u(out[7])..];
    let res = |tag: u64, a: u64, b: u64| match tag {
        0 => Resolution::Local(BinderId(a as u32)),
        _ => Resolution::Global(def(a, b)),
    };
    let name = |host: &HygieneHost, s: u64| host.text(Symbol(s as u32)).to_owned();
    match out[0] {
        0 => {}
        1 => return Err(depth_error(max_depth)),
        2 => return Err(ForgeError::UnbalancedDelimiters { token: u(e1) }),
        4 => {
            return Err(ForgeError::IdentityExhausted {
                kind: if e1 == 0 {
                    IdentityKind::Scope
                } else {
                    IdentityKind::Binder
                },
                limit: e2 as u32,
            });
        }
        5 => {
            return Err(ForgeError::PreResolvedBinder {
                token: u(e1),
                def: def(e2, e3),
            });
        }
        6 => return Err(ForgeError::MixedPath { token: u(e1) }),
        7 => {
            return Err(ForgeError::Unbound {
                token: u(e1),
                name: name(host, e2),
            });
        }
        8 => {
            return Err(ForgeError::Ambiguous {
                token: u(e1),
                name: name(host, e2),
                candidates: extra.chunks(3).map(|c| res(c[0], c[1], c[2])).collect(),
            });
        }
        9 => {
            return Err(ForgeError::WrongDefinitionKind {
                token: u(e1),
                def: def(e2, e3),
                expected: def_kind(e4),
            });
        }
        10 => return Err(ForgeError::ExpectedGlobal { token: u(e1) }),
        11 => return Err(ForgeError::UnknownDefinition(def(e1, e2))),
        12 => return Err(ForgeError::UnknownPackage(PackageId(e1 as u32))),
        13 => {
            return Err(ForgeError::NotADependency {
                from: PackageId(e1 as u32),
                def: def(e2, e3),
            });
        }
        14 => {
            return Err(ForgeError::PrivateDefinition {
                from: PackageId(e1 as u32),
                def: def(e2, e3),
            });
        }
        15 => {
            let d = def(e1, e2);
            return Err(ForgeError::PrintedNameMismatch {
                def: d,
                printed: name(host, e3),
                registered: host
                    .registry
                    .definition(d)
                    .map(|i| i.path.clone())
                    .unwrap_or_default(),
            });
        }
        _ => {
            return Err(ForgeError::Parse {
                token: u(e1),
                expected: if e2 == 0 {
                    "source parameter identifier"
                } else {
                    "distinct source parameter identifier"
                },
            });
        }
    }
    let session = host.session();
    let scope_set = |runs: &[u64]| {
        let mut s = ScopeSet::new();
        for &i in runs {
            s.insert(ScopeId {
                session,
                index: i as u32,
            });
        }
        s
    };
    let mut binders = Vec::new();
    let mut references = Vec::new();
    let mut operations = Vec::new();
    let mut at = 8;
    while at < out.len() {
        match out[at] {
            1 => {
                let (id, token, k) = (out[at + 1], u(out[at + 2]), u(out[at + 3]));
                let t = &tokens[token];
                binders.push(BinderInfo {
                    id: BinderId(id as u32),
                    token,
                    name: host.text(t.text).to_owned(),
                    origin: t.origin,
                    scopes: scope_set(&out[at + 4..at + 4 + k]),
                });
                at += 4 + k;
            }
            2 => {
                let r = &out[at + 1..at + 7];
                references.push(ResolvedRef {
                    token: u(r[0]),
                    name: name(host, r[1]),
                    origin: tokens[u(r[0])].origin,
                    resolution: res(r[2], r[3], r[4]),
                    pre_resolved: r[5] != 0,
                });
                at += 7;
            }
            _ => {
                let r = &out[at + 1..at + 8];
                let kind = match r[0] {
                    1 => OperationKind::Use {
                        def: def(r[1], r[2]),
                    },
                    2 => OperationKind::Handler {
                        effect: def(r[1], r[2]),
                        handler: def(r[3], r[4]),
                    },
                    _ => OperationKind::IndirectCall,
                };
                let k = u(r[6]);
                operations.push(EmittedOperation {
                    kind,
                    token: u(r[5]),
                    origins: out[at + 8..at + 8 + k]
                        .iter()
                        .map(|&o| origins[u(o)])
                        .collect(),
                });
                at += 8 + k;
            }
        }
    }
    Ok(Resolved {
        binders,
        references,
        operations,
    })
}
