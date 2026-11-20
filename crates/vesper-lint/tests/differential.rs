//! The Vesper port (`--features vesper-port`) against the frozen Rust seed
//! copy, `vesper-lint-seed`: the same facts, policy documents and edits
//! give the same reports, policies and errors (self-hosting SH1 lane 13,
//! #1156). Cargo-only: port/test-port.sh runs it in Linux port shard 7.
#![cfg(vesper_port)]

mod support;

use lint_seed as seed;
use std::collections::BTreeMap;
use support::{Rng, to_port};
use vesper_core::ast::{
    Effect, Effects, Expr, ExprKind, Function, Generic, Mode, Param, Pass, Type,
};

fn report_text(r: Result<vesper_lint::LintReport, vesper_lint::LintError>) -> String {
    match r {
        Ok(r) => serde_json::to_string(&r).unwrap(),
        Err(e) => format!("error {e}"),
    }
}

fn seed_report_text(r: Result<seed::LintReport, seed::LintError>) -> String {
    match r {
        Ok(r) => serde_json::to_string(&r).unwrap(),
        Err(e) => format!("error {e}"),
    }
}

fn port_limits(l: seed::Limits) -> vesper_lint::Limits {
    vesper_lint::Limits {
        functions: l.functions,
        actions: l.actions,
        work: l.work,
        call_depth: l.call_depth,
    }
}

/// Runs both builds on `program` with `limits`; returns the seed's result
/// text, or panics with both.
fn same_report(what: &str, program: &seed::CheckedProgram, limits: seed::Limits) -> String {
    let expected = seed_report_text(seed::analyze(program, limits));
    let actual = report_text(vesper_lint::analyze(&to_port(program), port_limits(limits)));
    assert_eq!(actual, expected, "{what} with {limits:?}");
    expected
}

#[test]
fn repository_sources_lint_identically() {
    let corpus = support::corpus(256 * 1024);
    assert!(corpus.len() > 100, "only {} checked sources", corpus.len());
    let mut findings = 0;
    let mut skipped = 0;
    for (path, program) in &corpus {
        let what = path.display().to_string();
        let text = same_report(&what, program, seed::Limits::default());
        if let Ok(r) = seed::analyze(program, seed::Limits::default()) {
            findings += r.findings.len();
            skipped += r.skipped.len();
            // The work limit at every point up to the work used, sparsely.
            let step = (r.work / 16).max(1);
            let mut work = 1;
            while work <= r.work {
                same_report(
                    &what,
                    program,
                    seed::Limits {
                        work,
                        ..seed::Limits::default()
                    },
                );
                work += step;
            }
        }
        // Every function as source-owned: generated bodies take part too.
        let mut owned = program.clone();
        for f in owned.functions.values_mut() {
            f.source_owned = true;
        }
        same_report(&what, &owned, seed::Limits::default());
        for call_depth in [1, 2, 3] {
            same_report(
                &what,
                &owned,
                seed::Limits {
                    call_depth,
                    ..seed::Limits::default()
                },
            );
        }
        assert!(!text.is_empty());
    }
    eprintln!(
        "{} sources, {findings} findings, {skipped} skipped functions",
        corpus.len()
    );
    assert!(findings > 0);
}

// ---------------------------------------------------------------- facts

const NAMES: [&str; 7] = ["u32", "u64", "T", "U", "dyn", "Vec", "Result"];
const EFFECTS: [&str; 3] = ["Capability", "Fail", "Io"];
const LABELS: [Option<&str>; 3] = [None, Some("a"), Some("b")];
const FIELDS: [&str; 5] = ["x", "y", "z", "w", "é"];
const PARAMS: [&str; 4] = ["p", "q", "r", "s"];

fn gen_type(rng: &mut Rng, depth: u32) -> Type {
    let k = rng.below(100);
    if depth > 3 || k < 50 {
        return Type::named(rng.pick(&NAMES));
    }
    if k < 75 {
        let n = rng.below(3);
        Type::Named(
            rng.pick(&NAMES).to_string(),
            (0..n).map(|_| gen_type(rng, depth + 1)).collect(),
        )
    } else if k < 90 {
        let n = rng.below(3);
        Type::Tuple((0..n).map(|_| gen_type(rng, depth + 1)).collect())
    } else if k < 94 {
        Type::Function {
            params: (0..rng.below(3))
                .map(|_| {
                    (
                        *rng.pick(&[Pass::In, Pass::Inout, Pass::Sink]),
                        gen_type(rng, depth + 1),
                    )
                })
                .collect(),
            result: Box::new(gen_type(rng, depth + 1)),
            effects: gen_effects(rng, depth + 1),
            mode: *rng.pick(&[Mode::Copy, Mode::Affine, Mode::Linear]),
        }
    } else if k < 96 {
        Type::Infer
    } else if k < 98 {
        Type::IntLiteral(rng.next() as i64 as i128 * 3)
    } else {
        Type::Row(gen_effects(rng, depth + 1))
    }
}

fn gen_effect(rng: &mut Rng, depth: u32) -> Effect {
    Effect {
        label: rng.pick(&LABELS).map(str::to_owned),
        name: rng.pick(&EFFECTS).to_string(),
        args: (0..rng.below(3))
            .map(|_| gen_type(rng, depth + 1))
            .collect(),
    }
}

fn gen_effects(rng: &mut Rng, depth: u32) -> Effects {
    Effects {
        instances: (0..rng.below(4)).map(|_| gen_effect(rng, depth)).collect(),
        tail: if rng.chance(5) {
            Some("E".into())
        } else {
            None
        },
    }
}

fn gen_path(rng: &mut Rng) -> Vec<String> {
    (0..rng.below(4))
        .map(|_| rng.pick(&FIELDS).to_string())
        .collect()
}

fn gen_param(rng: &mut Rng, name: &str) -> Param {
    Param {
        name: name.into(),
        pass: *rng.pick(&[Pass::In, Pass::Inout, Pass::Inout, Pass::Sink]),
        projections: if rng.chance(10) {
            vec![gen_path(rng)]
        } else {
            vec![]
        },
        ty: gen_type(rng, 2),
        span: rng.below(50) as usize..50 + rng.below(50) as usize,
    }
}

fn gen_program(rng: &mut Rng) -> seed::CheckedProgram {
    let nf = 1 + rng.below(6);
    let names: Vec<String> = (0..nf).map(|i| format!("f{i}")).collect();
    let mut functions = BTreeMap::new();
    for name in &names {
        let effects = gen_effects(rng, 1);
        let mut generics = Vec::new();
        for g in ["T", "U"] {
            if rng.chance(30) {
                generics.push(Generic {
                    name: g.into(),
                    bound: Mode::Copy,
                    row: rng.chance(5),
                });
            }
        }
        let params: Vec<Param> = (0..rng.below(4))
            .map(|i| gen_param(rng, PARAMS[i as usize]))
            .collect();
        let mut actions = vec![];
        for _ in 0..rng.below(10) {
            let k = rng.below(100);
            actions.push(if k < 30 {
                // Mostly an instance the function declares.
                if !effects.instances.is_empty() && rng.chance(70) {
                    let mut e = rng.pick(&effects.instances).clone();
                    if rng.chance(30) {
                        e.label = None;
                    }
                    seed::Action::Perform(e)
                } else {
                    seed::Action::Perform(gen_effect(rng, 1))
                }
            } else if k < 55 {
                let callee = if rng.chance(90) {
                    rng.pick(&names).clone()
                } else {
                    "missing".into()
                };
                let mut substitutions = BTreeMap::new();
                for g in ["T", "U"] {
                    if rng.chance(60) {
                        substitutions.insert(g.to_string(), gen_type(rng, 3));
                    }
                }
                seed::Action::Call {
                    callee,
                    substitutions,
                }
            } else if k < 96 {
                seed::Action::Access {
                    root: rng.pick(&PARAMS).to_string(),
                    path: gen_path(rng),
                    pass: *rng.pick(&[Pass::In, Pass::Inout, Pass::Sink]),
                    moves_noncopy: rng.chance(5),
                }
            } else {
                seed::Action::Opaque(format!("opaque {}", rng.below(5)))
            });
        }
        let parameters = params
            .iter()
            .map(|p| seed::ParameterFacts {
                parameter: p.clone(),
                struct_fields: FIELDS
                    .iter()
                    .filter(|_| rng.chance(50))
                    .map(|f| f.to_string())
                    .collect(),
            })
            .collect();
        let span = rng.below(100) as usize..100 + rng.below(100) as usize;
        functions.insert(
            name.clone(),
            seed::FunctionFacts {
                declaration: Function {
                    name: name.clone(),
                    fip: false,
                    generics,
                    params,
                    result: gen_type(rng, 2),
                    effects,
                    body: if rng.chance(90) {
                        Some(Expr {
                            kind: ExprKind::Tuple(vec![]),
                            span: span.clone(),
                        })
                    } else {
                        None
                    },
                    span,
                },
                actions,
                parameters,
                source_owned: rng.chance(90),
            },
        );
    }
    seed::CheckedProgram { functions }
}

fn gen_limits(rng: &mut Rng) -> seed::Limits {
    let mut l = seed::Limits::default();
    if rng.chance(30) {
        l.work = 1 + rng.below(300) as usize;
    }
    if rng.chance(20) {
        l.call_depth = 1 + rng.below(4) as usize;
    }
    if rng.chance(5) {
        l.functions = rng.below(6) as usize;
    }
    if rng.chance(5) {
        l.actions = rng.below(20) as usize;
    }
    if rng.chance(2) {
        l.work = 1_000_001;
    }
    l
}

#[test]
fn fuzzed_facts_lint_identically() {
    let cases: u64 = std::env::var("VESPER_LINT_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut findings = 0;
    for case in 0..cases {
        let program = gen_program(&mut rng);
        let limits = gen_limits(&mut rng);
        let text = same_report(&format!("case {case}: {program:?}"), &program, limits);
        findings += text.matches("\"code\"").count();
    }
    assert!(findings > cases as usize / 4, "{findings} findings");
}

#[test]
fn fact_limits_fail_identically() {
    let base = || {
        let mut rng = Rng(7);
        gen_program(&mut rng)
    };
    let mut deep = Type::named("u32");
    for _ in 0..40 {
        deep = Type::Named("Vec".into(), vec![deep]);
    }
    // Past the port's encoding depth: the facts go to the Rust code, whose
    // checks are iterative, rather than through recursive encoding.
    let mut deeper = Type::named("u32");
    for _ in 0..300 {
        deeper = Type::Named("Vec".into(), vec![deeper]);
    }
    let wide = Type::Tuple(vec![Type::named("u8"); 600]);
    let bushy = Type::Tuple(vec![Type::Tuple(vec![Type::named("u8"); 30]); 30]);
    let arrow = Type::Function {
        params: vec![(Pass::In, Type::named("u8")); 129],
        result: Box::new(Type::unit()),
        effects: Effects::default(),
        mode: Mode::Copy,
    };
    let row = Type::Row(Effects {
        instances: vec![
            Effect {
                label: None,
                name: "E".into(),
                args: vec![Type::named("u8"); 65],
            };
            2
        ],
        tail: None,
    });
    let big_effect = Effect {
        label: None,
        name: "E".into(),
        args: vec![Type::named("u8"); 65],
    };
    for (i, ty) in [deep, deeper, wide, bushy, arrow, row]
        .into_iter()
        .enumerate()
    {
        for slot in 0..4 {
            let mut p = base();
            let f = p.functions.values_mut().next().unwrap();
            match slot {
                0 => f.declaration.result = ty.clone(),
                1 => f.declaration.effects.instances.push(Effect {
                    label: None,
                    name: "Capability".into(),
                    args: vec![ty.clone()],
                }),
                2 => f.actions.push(seed::Action::Call {
                    callee: "f0".into(),
                    substitutions: BTreeMap::from([("T".into(), ty.clone())]),
                }),
                _ => f.actions.push(seed::Action::Perform(Effect {
                    label: None,
                    name: "Fail".into(),
                    args: vec![ty.clone()],
                })),
            }
            same_report(
                &format!("type {i} slot {slot}"),
                &p,
                seed::Limits::default(),
            );
        }
    }
    let mut p = base();
    let f = p.functions.values_mut().next().unwrap();
    f.actions.push(seed::Action::Perform(big_effect.clone()));
    same_report("effect arity", &p, seed::Limits::default());
    let mut p = base();
    let f = p.functions.values_mut().next().unwrap();
    f.actions.push(seed::Action::Access {
        root: "p".into(),
        path: vec!["x".into(); 33],
        pass: Pass::In,
        moves_noncopy: false,
    });
    same_report("projection depth", &p, seed::Limits::default());
    let mut p = base();
    let f = p.functions.values_mut().next().unwrap();
    f.actions.push(seed::Action::Call {
        callee: "f0".into(),
        substitutions: (0..65)
            .map(|i| (format!("G{i}"), Type::named("u8")))
            .collect(),
    });
    same_report("substitutions", &p, seed::Limits::default());
    let mut p = base();
    let f = p.functions.values_mut().next().unwrap();
    f.declaration.generics = vec![
        Generic {
            name: "G".into(),
            bound: Mode::Copy,
            row: false
        };
        65
    ];
    same_report("generics", &p, seed::Limits::default());
    let mut p = base();
    let f = p.functions.values_mut().next().unwrap();
    f.parameters.push(seed::ParameterFacts {
        parameter: gen_param(&mut Rng(3), "p"),
        struct_fields: vec!["x".into(); 129],
    });
    same_report("struct fields", &p, seed::Limits::default());
    for limits in [
        seed::Limits {
            functions: 0,
            ..seed::Limits::default()
        },
        seed::Limits {
            work: 1_000_001,
            ..seed::Limits::default()
        },
        seed::Limits {
            call_depth: 65,
            ..seed::Limits::default()
        },
        seed::Limits {
            actions: 1,
            ..seed::Limits::default()
        },
    ] {
        same_report("limits", &base(), limits);
    }
}

// ---------------------------------------------------------------- policy

fn policy_text(r: Result<vesper_lint::policy::Policy, vesper_lint::policy::PolicyError>) -> String {
    match r {
        Ok(p) => format!(
            "rules {} unavailable {} usage {}",
            serde_json::to_string(&p.rules()).unwrap(),
            serde_json::to_string(&p.unavailable()).unwrap(),
            serde_json::to_string(&p.usage()).unwrap()
        ),
        Err(e) => format!("error {e}"),
    }
}

fn seed_policy_text(r: Result<seed::policy::Policy, seed::policy::PolicyError>) -> String {
    match r {
        Ok(p) => format!(
            "rules {} unavailable {} usage {}",
            serde_json::to_string(&p.rules()).unwrap(),
            serde_json::to_string(&p.unavailable()).unwrap(),
            serde_json::to_string(&p.usage()).unwrap()
        ),
        Err(e) => format!("error {e}"),
    }
}

fn same_policy(source: &str, limits: seed::policy::PolicyLimits) -> String {
    let expected = seed_policy_text(seed::policy::Policy::parse(source, limits));
    let port = vesper_lint::policy::PolicyLimits {
        bytes: limits.bytes,
        depth: limits.depth,
        tokens: limits.tokens,
        nodes: limits.nodes,
        work: limits.work,
    };
    let actual = policy_text(vesper_lint::policy::Policy::parse(source, port));
    assert_eq!(actual, expected, "{source:?} with {limits:?}");
    expected
}

fn same_default_policy(source: &str) -> String {
    same_policy(source, seed::policy::PolicyLimits::default())
}

/// Documents from crates/vesper-lint/tests/policy.rs and hand cases for
/// each part of the TOML grammar and the policy.
const CASES: &[&str] = &[
    "",
    "lints.vesper.unused_effect_capability = 'allow'\n[package]\nname='unrelated'\n[dependencies]\na={version='1',features=['x','y']}\n[[targets]]\nname='one'\n[[targets]]\nname='two'",
    "[lints.vesper]\nunknown='warn'",
    "[lints.unknown]\na=1",
    "lints=1",
    "[lints]\nvesper=[]",
    "[lints.vesper]\nunused_effect_capability=true",
    "[lints.vesper]\nunused_effect_capability='WARN'",
    "[lints.vesper]\nunused_effect_capability='warn'\nunused_effect_capability='deny'",
    "[lints.vesper]\n'overbroad_inout_projection'='allow'\n\"overbroad_inout_projectio\\u006e\"='warn'",
    "[lints.plugins]\n'@vesper-game/ignis-lints'={version='1',fuel_limit=5000000}",
    "[lints]\nplugins=1",
    "[lints.vesper]\ndemote_alloc_to_region='deny'",
    "[lints.plugins]",
    "[lints.vesper]\nmissed_fip_opportunity='allow'\nearly_cancel_checkpoint='warn'\nunredacted_pii_boundary='allow'",
    "# [{ . ignored comment\nnotes = \"\"\"fake [ [[{{ a.b.c.d ] } \\\" quote\n# still string\n\"\"\"\nliteral = '''[ { \" . # literal\n ] . }'''\nescaped = \"quotes \\\" [ { . # stay in string\"\n[\"lints\".\"vesper\"]\n\"unused_effect_capability\" = \"warn\"\n\"overbroad_inout_projection\" = 'allow'\n",
    "text=\"\"\"words\"\"\"\n[lints.vesper]\nunused_effect_capability='warn'\n",
    "text=\"\"\"words\"\"\"\"\n[lints.vesper]\nunused_effect_capability='warn'\n",
    "text=\"\"\"words\"\"\"\"\"\n[lints.vesper]\nunused_effect_capability='warn'\n",
    "text='''words'''\n[lints.vesper]\nunused_effect_capability='warn'\n",
    "text='''words''''\n",
    "text='''words'''''\n",
    "text='''words''''''\n",
    "text=\"\"\"words\"\"\"\"\"\"\n",
    "['a.b.c.d']\n'key.with.dots'={value=1}\n[lints.vesper]\nunused_effect_capability='warn'",
    "x=[\n[\n[1]\n]\n]\n",
    "[package]\nname='coexist'\n[lints.vesper]\nunused_effect_capability='allow'",
    "[lints.vesper]\nunused_effect_capability='warn'\n[unrelated]\na.b.c={d.e.f={g.h.i={j.k.l={m.n.o={value=0}}}}}",
    // Tables, dotted keys and arrays of tables.
    "[a]\n[a]",
    "[a.b]\n[a]\nx=1",
    "[a]\n[a.b]\n[a]",
    "a.b=1\n[a]",
    "a.b=1\n[a.c]",
    "[x]\na.b=1\n[x.a]",
    "[x]\na.b=1\n[x.a.c]",
    "[a.b.c]\nx=1\n[a]\nb.d=2",
    "[[a]]\n[a]",
    "[a]\n[[a]]",
    "[[a]]\nx=1\n[[a]]\nx=2\n[[a.b]]\ny=1\n[a.c]\nz=1",
    "[[x.y]]\n[x]\ny.z.w=1",
    "a=[]\n[[a]]",
    "a={}\n[a]",
    "a={}\n[a.b]",
    "a={b=1}\na.c=2",
    "a={b.c=1, b.d=2}",
    "a={b.c=1, b={}}",
    "a={b=1,}",
    "a={ }",
    "a={b=1, c={d=[1,{e=2}]}}",
    "a={\nb=1}",
    "a=[1,]\nb=[ ]\nc=[\n# c\n1, # d\n2\n]\nd=[,]",
    "a=[1,,2]",
    "a=[1 2]",
    "[ a . b ]\n[[ c ]]\n[ [d]]",
    "[[c] ]",
    "a . b = 1\n\"\" = 2\n'' = 3",
    "a.\"b\".'c' = 1\na.b.d = 2",
    "a = 1 b = 2",
    "a = 1 # comment\n# x\r\nb = 2\r\n",
    "a = 1\rb=2",
    "\u{feff}a = 1",
    "\u{feff}[lints.vesper]\nunused_effect_capability='deny'",
    "a = \"\\e\"",
    "a = \"\\u00e9\\U0001F600\"",
    "a = \"\\ud800\"",
    "a = \"\\u12\"",
    "a = \"tab\there\"",
    "a = \"del\u{7f}\"",
    "a = 'del\u{7f}'",
    "# del \u{7f}\n",
    "a = \"\"\"\nline\\\n   continued \\  \n\n  x\"\"\"",
    "a = \"\"\"\\ x\"\"\"",
    "a = \"\"\"a\"\"b\"\"\"",
    "a = '''\r\nline\r\n'''",
    "a = \"\"\"\"\"\"",
    "a = \"\"\"\"\"\"\"",
    // Numbers.
    "a = 0\nb = +0\nc = -0\nd = 1_000\ne = 0x_1\n",
    "a = 01",
    "a = 1_",
    "a = 1__0",
    "a = 0xdead_BEEF\nb = 0o17\nc = 0b101\n",
    "a = 0x7fffffffffffffff\n",
    "a = 0x8000000000000000\n",
    "a = 9223372036854775807\nb = -9223372036854775808\n",
    "a = 9223372036854775808\n",
    "a = -9223372036854775809\n",
    "a = +0x1",
    "a = 0b2",
    "a = 0o8",
    "a = 1.5\nb = -1e5\nc = 1E+5\nd = 1.5e-3\ne = 0.0\nf = 6.626e-34\n",
    "a = 1.",
    "a = .5",
    "a = 1.e5",
    "a = 1e",
    "a = 1e_5",
    "a = 1_0.0_1e1_0",
    "a = 1e308\nb = 1.7976931348623157e308\n",
    "a = 1.7976931348623158e308\n",
    "a = 1.79769313486231580793728971405303415079934132710037826936173778980444968292764750946649017977587207096330286416692887910946555547851940402630657488671505820681908902000708383676273854845817711531764475730270069855571366959622842914819860834936475292719074168444365510704342711559699508093042880177904174497791e308\n",
    "a = 1.79769313486231580793728971405303415079934132710037826936173778980444968292764750946649017977587207096330286416692887910946555547851940402630657488671505820681908902000708383676273854845817711531764475730270069855571366959622842914819860834936475292719074168444365510704342711559699508093042880177904174497792e308\n",
    "a = 1e309",
    "a = -1e400",
    "a = 0.00000000000000000000001e330",
    "a = inf\nb = +inf\nc = -inf\nd = nan\ne = +nan\nf = -nan\n",
    "a = infinity",
    "a = true\nb = false\n",
    "a = tru",
    "a = truex",
    // Datetimes.
    "a = 1979-05-27T07:32:00Z\nb = 1979-05-27T00:32:00.999999-07:00\nc = 1979-05-27 07:32:00\nd = 1979-05-27\ne = 07:32:00\nf = 00:32:00.999999\ng = 1979-05-27t07:32:00z\n",
    "a = 2000-02-29\nb = 1900-02-29\n",
    "a = 2001-02-29",
    "a = 1979-13-01",
    "a = 1979-05-32",
    "a = 1979-05-27T25:00:00",
    "a = 1979-05-27T07:60:00",
    "a = 1979-05-27T07:32:61",
    "a = 1979-05-27T07:32",
    "a = 1979-05-27T07:32:00+25:00",
    "a = 1979-05-27T07:32:00.",
    "a = [1979-05-27 , 2]",
    "a = 1979-05-27 # date",
    "a = 12:30",
    "a = 25:00:00",
    "a = 19790-05-27",
    // serde's private datetime marker.
    "[a]\n'$__toml_private_datetime' = '1979-05-27'",
    "a = {'$__toml_private_datetime' = 1}",
    // Limits.
    "[lints.vesper]\nunused_effect_capability='warn'\noverbroad_inout_projection='warn'\ndemote_alloc_to_region='allow'\noverly_restrictive_mode='allow'\nmissed_fip_opportunity='allow'\nearly_cancel_checkpoint='allow'\nunredacted_pii_boundary='allow'\nextra='allow'",
    "[lints.vesper]\nxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx='warn'",
    "[[lints]]",
    "lints = {vesper = {unused_effect_capability = 'deny'}, plugins = {}}",
    "[lints]\nvesper.unused_effect_capability = 'deny'\nplugins.x = 1",
];

#[test]
fn hand_written_documents_parse_identically() {
    for source in CASES {
        same_default_policy(source);
    }
    let mut source = String::new();
    for n in 1..20 {
        source.push_str(&format!("[[{}]]\n", vec!["x"; n].join(".")));
        same_default_policy(&source);
    }
    for source in [
        format!("x={}1{}", "[".repeat(80), "]".repeat(80)),
        format!("{}x=1", "a.".repeat(80)),
        format!("[{}x]\ny=1", "a.".repeat(80)),
        format!("[[{}x]]\ny=1", "a.".repeat(80)),
        format!("x={{{}y=1}}", "a.".repeat(80)),
        " ".repeat(65537),
        format!("k = '{}'", "x".repeat(70000)),
        format!("k = [{}]", "1,".repeat(3000)),
        format!("{}='v'", "k".repeat(129)),
    ] {
        same_default_policy(&source);
    }
}

#[test]
fn repository_manifests_parse_identically() {
    let files = support::files("toml");
    assert!(files.len() > 50);
    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        same_default_policy(&source);
    }
}

/// A random TOML-ish document: valid fragments of every grammar form,
/// optionally mutated byte by byte.
fn gen_document(rng: &mut Rng) -> String {
    const KEYS: &[&str] = &[
        "a",
        "b",
        "lints",
        "vesper",
        "plugins",
        "x-y",
        "_1",
        "\"q\"",
        "'l'",
        "\"\"",
        "\"é\\u00e9\"",
        "unused_effect_capability",
        "overbroad_inout_projection",
        "demote_alloc_to_region",
        "1",
        "true",
        "inf",
    ];
    const VALUES: &[&str] = &[
        "'allow'",
        "'warn'",
        "'deny'",
        "\"warn\"",
        "'other'",
        "1",
        "-1",
        "+1_0",
        "0x1f",
        "0o7",
        "0b1",
        "1.5",
        "1e10",
        "-1e400",
        "1e400",
        "inf",
        "-nan",
        "true",
        "false",
        "1979-05-27T07:32:00Z",
        "1979-05-27",
        "07:32:00",
        "1979-05-27 07:32:00.5+01:00",
        "\"esc \\t \\\" \\\\ \\u00e9\"",
        "'''lit\n''x'''",
        "\"\"\"ml \\\n  x \"\" y\"\"\"",
        "[]",
        "[1, 2,]",
        "[[1], ['a']]",
        "[\n 1, # c\n 2\n]",
        "{}",
        "{a = 1}",
        "{a.b = 1, a.c = 'x'}",
        "{x = {y = [1, {z = 2}]}}",
        "\"\"",
    ];
    let mut out = String::new();
    for _ in 0..rng.below(12) {
        let k = rng.below(100);
        let key = |rng: &mut Rng| {
            let n = 1 + rng.below(3);
            (0..n)
                .map(|_| rng.pick(KEYS).to_string())
                .collect::<Vec<_>>()
                .join(if rng.chance(20) { " . " } else { "." })
        };
        if k < 15 {
            out.push_str(&format!("[{}]", key(rng)));
        } else if k < 25 {
            out.push_str(&format!("[[{}]]", key(rng)));
        } else if k < 30 {
            out.push_str("# comment [ { ' \"");
        } else if k < 33 {
            out.push_str(" \t");
        } else {
            out.push_str(&format!("{} = {}", key(rng), rng.pick(VALUES)));
        }
        if rng.chance(20) {
            out.push_str(" # trailing");
        }
        out.push_str(if rng.chance(10) { "\r\n" } else { "\n" });
    }
    if rng.chance(60) {
        let bytes = unsafe_free_mutate(rng, out.into_bytes());
        out = String::from_utf8_lossy(&bytes).into_owned();
    }
    out
}

fn unsafe_free_mutate(rng: &mut Rng, mut bytes: Vec<u8>) -> Vec<u8> {
    const ALPHABET: &[u8] = b" \t\n\r#.=[]{},'\"\\_+-0123456789eExabtfuU:TZz";
    for _ in 0..1 + rng.below(3) {
        let at = rng.below(bytes.len() as u64 + 1) as usize;
        match rng.below(3) {
            0 => bytes.insert(at, *rng.pick(ALPHABET)),
            1 if at < bytes.len() => {
                bytes.remove(at);
            }
            _ if at < bytes.len() => bytes[at] = *rng.pick(ALPHABET),
            _ => (),
        }
    }
    bytes
}

#[test]
fn fuzzed_documents_parse_identically() {
    let cases: u64 = std::env::var("VESPER_LINT_TOML_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20000);
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut accepted = 0;
    for _ in 0..cases {
        let source = gen_document(&mut rng);
        if same_default_policy(&source).starts_with("rules") {
            accepted += 1;
        }
    }
    // Both outcomes are well represented.
    assert!(
        accepted > cases / 10 && accepted < cases * 9 / 10,
        "{accepted} of {cases}"
    );
}

#[test]
fn policy_limit_sweeps_fail_identically() {
    let sources = [
        include_str!("../examples/Loom.lint.toml"),
        "[package]\nname='x'\n[dependencies]\na={version='1',features=['x','y']}\n[lints.vesper]\nunused_effect_capability='allow'\n",
        "x=[[1,2],[3,[4,5]]]\ny={a={b={c=1}}}\n",
    ];
    for source in sources {
        let full = seed::policy::Policy::parse(source, seed::policy::PolicyLimits::default())
            .unwrap()
            .usage();
        let d = seed::policy::PolicyLimits::default();
        for work in 1..=full.work + 1 {
            same_policy(source, seed::policy::PolicyLimits { work, ..d });
        }
        for bytes in (1..=source.len() + 1).chain([full.decoded_bytes, full.decoded_bytes + 1]) {
            same_policy(source, seed::policy::PolicyLimits { bytes, ..d });
        }
        for nodes in 1..=full.nodes + full.tokens + 2 {
            same_policy(source, seed::policy::PolicyLimits { nodes, ..d });
        }
        for tokens in 1..=full.tokens + 1 {
            same_policy(source, seed::policy::PolicyLimits { tokens, ..d });
        }
        for depth in 1..=8 {
            same_policy(source, seed::policy::PolicyLimits { depth, ..d });
        }
        for bad in [
            seed::policy::PolicyLimits { bytes: 0, ..d },
            seed::policy::PolicyLimits {
                work: usize::MAX,
                ..d
            },
            seed::policy::PolicyLimits { depth: 33, ..d },
        ] {
            same_policy(source, bad);
        }
    }
}

// ---------------------------------------------------------------- edits

#[test]
fn fuzzed_edits_apply_identically() {
    const PIECES: &[&str] = &["a", "b", "é", "€", "😀", "\n", " "];
    let mut rng = Rng(42);
    let mut applied = 0;
    for _ in 0..20000 {
        let source: String = (0..rng.below(20)).map(|_| *rng.pick(PIECES)).collect();
        let mut edits = Vec::new();
        for _ in 0..rng.below(5) {
            let a = rng.below(source.len() as u64 + 3) as usize;
            let b = a + rng.below(6) as usize;
            let expected = if rng.chance(80) {
                source.get(a..b).unwrap_or("").to_string()
            } else {
                "x".into()
            };
            let replacement: String = (0..rng.below(4)).map(|_| *rng.pick(PIECES)).collect();
            edits.push((a..b, expected, replacement));
        }
        let max = if rng.chance(10) {
            rng.below(40) as usize
        } else if rng.chance(2) {
            262145
        } else {
            1000
        };
        let ours: Vec<_> = edits
            .iter()
            .map(|(span, expected, replacement)| vesper_lint::SourceEdit {
                span: span.clone(),
                expected: expected.clone(),
                replacement: replacement.clone(),
            })
            .collect();
        let theirs: Vec<_> = edits
            .iter()
            .map(|(span, expected, replacement)| seed::SourceEdit {
                span: span.clone(),
                expected: expected.clone(),
                replacement: replacement.clone(),
            })
            .collect();
        let expected = seed::apply_edits(&source, &theirs, max).map_err(|e| e.to_string());
        let actual = vesper_lint::apply_edits(&source, &ours, max).map_err(|e| e.to_string());
        assert_eq!(actual, expected, "{source:?} {edits:?} {max}");
        applied += expected.is_ok() as usize;
    }
    assert!(applied > 2000, "{applied}");
}

#[test]
fn port_frees_its_allocations() {
    let mut rng = Rng(5);
    let programs: Vec<_> = (0..50).map(|_| gen_program(&mut rng)).collect();
    let run = || {
        for p in &programs {
            let _ = vesper_lint::analyze(&to_port(p), vesper_lint::Limits::default());
        }
        for source in CASES {
            let _ = vesper_lint::policy::Policy::parse(
                source,
                vesper_lint::policy::PolicyLimits::default(),
            );
        }
    };
    run();
    let before = live();
    run();
    assert_eq!(live(), before);
}

fn live() -> i64 {
    #[cfg(feature = "vesper-port")]
    {
        vesper_lint::port_live_allocations()
    }
    #[cfg(not(feature = "vesper-port"))]
    {
        0
    }
}
