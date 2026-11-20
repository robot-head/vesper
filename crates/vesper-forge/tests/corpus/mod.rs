//! The random fragment corpus shared by tests/port_differential.rs and
//! examples/abba.rs: callers, nested expansions with splices and claims, and
//! tight identity and nesting limits, over a fixed registry.
#![allow(dead_code)]

// One case, built and resolved with the crate at `$f`.

macro_rules! world {
    ($m:ident, $f:path, $crate_case:ty) => {
        #[allow(dead_code)]
        mod $m {
            use $f as f;
            use f::*;

            const APP: PackageId = PackageId(0);
            const HASH: PackageId = PackageId(1);
            const DERIVE: PackageId = PackageId(2);
            const NET: PackageId = PackageId(3);
            const UTIL: PackageId = PackageId(5);
            const OTHER: PackageId = PackageId(6);

            const fn d(package: PackageId, item: u32) -> DefId {
                DefId { package, item }
            }

            const DEFS: &[(DefId, &str, DefKind, bool)] = &[
                (d(APP, 0), "app::print", DefKind::Function, true),
                (d(HASH, 0), "@vesper-wire/hash::hash_into", DefKind::Function, true),
                (d(DERIVE, 0), "@vesper-wire/derive::derive", DefKind::Macro, true),
                (d(DERIVE, 1), "@vesper-wire/derive::phone_home", DefKind::Function, true),
                (d(DERIVE, 2), "@vesper-wire/derive::inner", DefKind::Macro, true),
                (d(NET, 0), "@std/net::connect", DefKind::EffectOp, true),
                (d(NET, 1), "@std/net::Net", DefKind::Effect, true),
                (d(NET, 2), "@std/net::tcp", DefKind::Handler, true),
                (d(UTIL, 0), "@util/misc::sneaky", DefKind::Function, true),
                (d(UTIL, 9), "@util/misc::private", DefKind::Function, false),
                (d(OTHER, 0), "other::f", DefKind::Function, true),
            ];

            fn registry() -> DefinitionRegistry {
                let mut r = DefinitionRegistry::new();
                for (id, name, deps) in [
                    (APP, "app", vec![DERIVE, NET, HASH]),
                    (HASH, "@vesper-wire/hash", vec![]),
                    (DERIVE, "@vesper-wire/derive", vec![HASH, NET, UTIL]),
                    (NET, "@std/net", vec![]),
                    (UTIL, "@util/misc", vec![NET]),
                    (OTHER, "other", vec![]),
                ] {
                    r.add_package(PackageInfo {
                        id,
                        name: name.into(),
                        dependencies: deps.into_iter().collect(),
                    })
                    .unwrap();
                }
                for &(id, path, kind, public) in DEFS {
                    r.add_definition(DefInfo {
                        id,
                        path: path.into(),
                        kind,
                        visibility: if public {
                            Visibility::Public
                        } else {
                            Visibility::Package
                        },
                    })
                    .unwrap();
                }
                r
            }

            fn host(tight: bool) -> HygieneHost {
                let (ids, quote) = if tight {
                    (
                        IdentityLimits {
                            max_scopes: 8,
                            max_expansions: 4,
                            max_splices: 8,
                            max_binders: 2,
                            max_symbols: 40,
                        },
                        QuoteLimits {
                            max_depth: 6,
                            ..QuoteLimits::default()
                        },
                    )
                } else {
                    (IdentityLimits::default(), QuoteLimits::default())
                };
                let mut h = HygieneHost::with_limits(SessionId(3), registry(), ids, quote);
                for (p, name, def) in [
                    (APP, "print", d(APP, 0)),
                    (APP, "connect", d(NET, 0)),
                    (APP, "hash_into", d(HASH, 0)),
                    (APP, "Net", d(NET, 1)),
                    (APP, "tcp", d(NET, 2)),
                    (APP, "@std/net::Net", d(NET, 1)),
                    (DERIVE, "hash_into", d(HASH, 0)),
                    (DERIVE, "phone_home", d(DERIVE, 1)),
                    (DERIVE, "x", d(DERIVE, 1)),
                    (DERIVE, "Net", d(NET, 1)),
                    (DERIVE, "tcp", d(NET, 2)),
                    (DERIVE, "sneaky", d(UTIL, 0)),
                    // Two equally specific bindings: references are ambiguous.
                    (APP, "dup", d(APP, 0)),
                    (APP, "dup", d(HASH, 0)),
                ] {
                    h.declare_global(p, name, def).unwrap();
                }
                h
            }

            fn quotation(text: &str, splices: &[SpliceHandle], picks: &[u64], claims: &[(String, DefId)]) -> Quotation {
                let mut q = Quotation::new();
                for (i, part) in text.split('$').enumerate() {
                    if i > 0 {
                        let k = picks[i % picks.len()] as usize;
                        if !splices.is_empty() {
                            q = q.splice(splices[k % splices.len()]);
                        } else if let Some((name, def)) = claims.get(k % claims.len().max(1)) {
                            q = q.token(GeneratedToken::ident(name.clone()).with_def(*def));
                        }
                    }
                    q = q.text(part);
                }
                q
            }

            /// Runs one case: `case.0` is the caller prefix, then up to two
            /// nested expansions (quotation text with `$` splice points),
            /// then a caller suffix.
            pub fn run(case: &$crate_case) -> String {
                let (mut h, program) = prepare(case);
                let result = program.and_then(|p| h.resolve(p));
                let scope = h.fresh_scope();
                let symbol = h.intern("zz-probe");
                format!("{result:#?}\nnext scope {scope:?}\nnext symbol {symbol:?}")
            }

            /// The host and the expanded tokens of a case.
            pub fn prepare(case: &$crate_case) -> (HygieneHost, Result<Vec<HostToken>, ForgeError>) {
                let mut h = host(case.tight);
                let result = prepare_on(&mut h, case);
                (h, result)
            }

            /// The expanded tokens of a case, built on the host `h`.
            pub fn prepare_on(h: &mut HygieneHost, case: &$crate_case) -> Result<Vec<HostToken>, ForgeError> {
                {
                    // prefix `{` (output `;`).. suffix `}`
                    let mut program = h.lex_source(APP, &case.prefix)?;
                    program.extend(h.lex_source(APP, "{")?);
                    let mut site = InvocationSite::Source(APP);
                    let mut last = vec![];
                    for (n, x) in case.expansions.iter().enumerate() {
                        let mut args = x
                            .args
                            .iter()
                            .map(|a| h.lex_source(APP, a))
                            .collect::<Result<Vec<_>, _>>()?;
                        if n > 0 && !last.is_empty() {
                            args.push(last.clone());
                        }
                        let macro_def = if n == 0 { d(DERIVE, 0) } else { d(DERIVE, 2) };
                        let e = h.begin_expansion(macro_def, site, args)?;
                        let claims: Vec<(String, DefId)> = x
                            .claims
                            .iter()
                            .map(|&(i, ref name)| {
                                let (def, path, _, _) = DEFS[i % DEFS.len()];
                                (name.clone().unwrap_or_else(|| path.to_owned()), def)
                            })
                            .collect();
                        let q = quotation(&x.text, &e.splices, &x.picks, &claims);
                        let out = h.finish_quotation(e.id, &q)?;
                        site = InvocationSite::Expansion(e.id);
                        last = out.clone();
                        if n + 1 == case.expansions.len() || x.keep {
                            program.extend(out);
                            program.extend(h.lex_source(APP, ";")?);
                        }
                    }
                    program.extend(h.lex_source(APP, &case.suffix)?);
                    program.extend(h.lex_source(APP, "}")?);
                    if case.drop_first && !program.is_empty() {
                        program.remove(0);
                    }
                    Ok(program)
                }
            }

            pub fn implementation() -> &'static str {
                f::IMPLEMENTATION
            }
        }
    };
}

#[derive(Debug, Default)]
pub struct Expansion {
    pub text: String,
    pub args: Vec<String>,
    pub picks: Vec<u64>,
    pub claims: Vec<(usize, Option<String>)>,
    pub keep: bool,
}

#[derive(Debug, Default)]
pub struct Case {
    pub prefix: String,
    pub expansions: Vec<Expansion>,
    pub suffix: String,
    pub tight: bool,
    pub drop_first: bool,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len() as u64) as usize]
    }
}

const NAMES: &[&str] = &[
    "x",
    "y",
    "tmp",
    "print",
    "connect",
    "hash_into",
    "phone_home",
    "sneaky",
    "Net",
    "tcp",
    "dup",
    "a::b",
    "@std/net::Net",
    "app::print",
    "missing",
    "x",
    "y",
    "tmp",
    "print",
];

fn expr(r: &mut Rng, depth: u32, splice: bool, out: &mut String) {
    let choice = if depth == 0 { r.below(3) } else { r.below(10) };
    match choice {
        0 => out.push_str(r.pick(NAMES)),
        1 => out.push_str(r.pick(&["1", "42", "\"s\""])),
        2 if splice => out.push_str(r.pick(&[" $ ", " $ ", " x :: $ ", " $ :: y "])),
        2 => out.push_str(r.pick(NAMES)),
        3 | 4 => {
            out.push_str("let ");
            if splice && r.below(4) == 0 {
                out.push_str(" $ ");
            } else {
                out.push_str(r.pick(&["x", "y", "tmp", "print", "hash_into", "x", "y"]));
            }
            out.push_str(" = ");
            expr(r, depth - 1, splice, out);
            out.push_str("; ");
            expr(r, depth - 1, splice, out);
        }
        5 | 6 => {
            if r.below(3) == 0 {
                out.push('(');
                expr(r, depth - 1, splice, out);
                out.push(')');
            } else {
                out.push_str(r.pick(NAMES));
            }
            for _ in 0..=r.below(2) {
                out.push('(');
                for i in 0..r.below(3) {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    expr(r, depth - 1, splice, out);
                }
                out.push(')');
            }
        }
        7 => {
            out.push_str("handle ");
            out.push_str(r.pick(&["Net", "@std/net::Net", "tcp", "x"]));
            out.push_str(" with ");
            out.push_str(r.pick(&["tcp", "Net", "x"]));
            out.push_str(" { ");
            expr(r, depth - 1, splice, out);
            out.push_str("; ");
            expr(r, depth - 1, splice, out);
            out.push_str(" }");
        }
        8 => {
            out.push_str("{ ");
            for _ in 0..r.below(3) {
                expr(r, depth - 1, splice, out);
                out.push_str("; ");
            }
            out.push('}');
        }
        _ => {
            out.push('(');
            expr(r, depth - 1, splice, out);
            out.push(')');
        }
    }
}

/// Occasionally breaks a generated text: drops, doubles or swaps a
/// character, or adds a stray delimiter.
fn mutate(r: &mut Rng, s: &mut String) {
    if s.is_empty() || r.below(10) != 0 {
        return;
    }
    let mut chars: Vec<char> = s.chars().collect();
    let i = r.below(chars.len() as u64) as usize;
    match r.below(4) {
        0 => {
            chars.remove(i);
        }
        1 => chars.insert(i, chars[i]),
        2 => chars.insert(
            i,
            ['(', ')', '{', '}', '[', ']', ';', ','][r.below(8) as usize],
        ),
        _ => {
            let j = r.below(chars.len() as u64) as usize;
            chars.swap(i, j);
        }
    }
    *s = chars.into_iter().collect();
}

/// The case of `seed`.
pub fn case(seed: u64) -> Case {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut c = Case {
        tight: r.below(3) == 0,
        drop_first: r.below(20) == 0,
        ..Case::default()
    };
    let depth = 1 + r.below(4) as u32;
    if r.below(2) == 0 {
        out_let(&mut r, depth, &mut c.prefix);
    }
    for _ in 0..r.below(3) {
        let mut x = Expansion {
            keep: r.below(3) == 0,
            picks: (0..4).map(|_| r.below(4)).collect(),
            ..Expansion::default()
        };
        for _ in 0..r.below(3) {
            let mut a = String::new();
            expr(&mut r, 1, false, &mut a);
            x.args.push(a);
        }
        for _ in 0..r.below(3) {
            let name = match r.below(4) {
                0 => Some(r.pick(&["wrong::name", "app::print", "x"]).to_owned()),
                _ => None,
            };
            x.claims.push((r.below(11) as usize, name));
        }
        expr(&mut r, depth, true, &mut x.text);
        mutate(&mut r, &mut x.text);
        c.expansions.push(x);
    }
    if c.expansions.is_empty() || r.below(2) == 0 {
        expr(&mut r, depth, false, &mut c.suffix);
    }
    mutate(&mut r, &mut c.suffix);
    c
}

fn out_let(r: &mut Rng, depth: u32, out: &mut String) {
    out.push_str("let ");
    out.push_str(r.pick(&["x", "y", "tmp", "hash_into", "print"]));
    out.push_str(" = ");
    expr(r, depth, false, out);
    out.push_str("; ");
}
