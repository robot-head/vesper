//! Differential tests comparing `@vesper/fusion`'s independent `witness.ves`
//! checker, S-expression reader, rule-set parser, and source-term builder
//! against the original pure-Rust reference implementation.
use std::collections::{BTreeSet, HashMap};
use std::time::Instant;
use vesper_fusion::dsp::{self, Biquad, DspGraph};
use vesper_fusion::engine::Node;
use vesper_fusion::tensor::{self, Activation, DType, TensorGraph};
use vesper_fusion::witness::{
    self, Direction, MAX_DEPTH, MAX_STEPS, MAX_TERMS, Pat, Rule, RuleSet, Step, Summary, Witness,
};
use vesper_fusion::{Error, RULE_LEMMAS};
use vesper_tensor::quant::WeightFormat;

const BENCH_ROUNDS: usize = 1000;
const ABBA_WARMUP_EPOCHS: usize = 2;
const ABBA_EPOCHS: usize = 30;
const ABBA_CHECK_OPS: usize = 64;
const ABBA_PARSE_OPS: usize = 16;

mod reference {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Sx {
        Atom(String),
        Str(String),
        List(Vec<Sx>),
    }

    fn pat_vars<'a>(p: &'a Pat, out: &mut BTreeSet<&'a str>) {
        match p {
            Pat::Var(v) => {
                out.insert(v);
            }
            Pat::App(_, kids) => kids.iter().for_each(|k| pat_vars(k, out)),
            Pat::Int(_) | Pat::Str(_) => {}
        }
    }

    fn parse_sexps(text: &str) -> Result<Vec<Sx>, Error> {
        let bad = |m: &str| Error::Witness(format!("s-expression: {m}"));
        let bytes = text.as_bytes();
        let mut stack: Vec<Vec<Sx>> = vec![Vec::new()];
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b';' => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'(' => {
                    if stack.len() > MAX_DEPTH {
                        return Err(bad("too deep"));
                    }
                    stack.push(Vec::new());
                    i += 1;
                }
                b')' => {
                    let done = stack.pop().ok_or_else(|| bad("unbalanced"))?;
                    stack
                        .last_mut()
                        .ok_or_else(|| bad("unbalanced"))?
                        .push(Sx::List(done));
                    i += 1;
                }
                b'"' => {
                    let start = i + 1;
                    i = start;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            return Err(bad("escapes are not used by the rule sets"));
                        }
                        i += 1;
                    }
                    if i >= bytes.len() {
                        return Err(bad("unterminated string"));
                    }
                    let s = text[start..i].to_owned();
                    stack.last_mut().expect("root list").push(Sx::Str(s));
                    i += 1;
                }
                b if b.is_ascii_whitespace() => i += 1,
                _ => {
                    let start = i;
                    while i < bytes.len()
                        && !bytes[i].is_ascii_whitespace()
                        && !matches!(bytes[i], b'(' | b')' | b'"' | b';')
                    {
                        i += 1;
                    }
                    stack
                        .last_mut()
                        .expect("root list")
                        .push(Sx::Atom(text[start..i].to_owned()));
                }
            }
            if stack.is_empty() {
                return Err(bad("unbalanced"));
            }
        }
        if stack.len() != 1 {
            return Err(bad("unbalanced"));
        }
        Ok(stack.pop().expect("root list"))
    }

    fn int_atom(a: &str) -> Option<i64> {
        let digits = a.strip_prefix('-').unwrap_or(a);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            a.parse().ok()
        } else {
            None
        }
    }

    fn pattern(sx: &Sx) -> Result<Pat, Error> {
        Ok(match sx {
            Sx::Str(s) => Pat::Str(s.clone()),
            Sx::Atom(a) => match int_atom(a) {
                Some(v) => Pat::Int(v),
                None => Pat::Var(a.clone()),
            },
            Sx::List(items) => {
                let Some(Sx::Atom(head)) = items.first() else {
                    return Err(Error::Witness("pattern without a constructor".into()));
                };
                if int_atom(head).is_some() {
                    return Err(Error::Witness("pattern head is a number".into()));
                }
                Pat::App(
                    head.clone(),
                    items[1..].iter().map(pattern).collect::<Result<_, _>>()?,
                )
            }
        })
    }

    pub fn parse_rules(text: &str, names: &[&'static str]) -> Result<Vec<Rule>, Error> {
        let bad = |m: String| Error::Witness(format!("rule set: {m}"));
        let mut rules = Vec::new();
        for command in parse_sexps(text)? {
            let Sx::List(items) = &command else {
                return Err(bad("top-level atom".into()));
            };
            if !matches!(items.first(), Some(Sx::Atom(head)) if head == "rewrite") {
                continue;
            }
            let [_, lhs, rhs] = items.as_slice() else {
                return Err(bad("a rewrite with options or the wrong arity".into()));
            };
            let index = rules.len();
            let name = *names
                .get(index)
                .ok_or_else(|| bad(format!("rewrite {index} has no name")))?;
            let lemma = RULE_LEMMAS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, l)| *l)
                .ok_or_else(|| bad(format!("rule {name} has no Lean lemma")))?;
            let (lhs, rhs) = (pattern(lhs)?, pattern(rhs)?);
            let (mut lv, mut rv) = (BTreeSet::new(), BTreeSet::new());
            pat_vars(&lhs, &mut lv);
            pat_vars(&rhs, &mut rv);
            if !rv.is_subset(&lv) || matches!(lhs, Pat::Var(_)) {
                return Err(bad(format!("rule {name} is not a constructor rewrite")));
            }
            rules.push(Rule {
                name,
                lemma,
                lhs,
                rhs,
            });
        }
        if rules.len() != names.len() {
            return Err(bad(format!(
                "{} rewrites for {} names",
                rules.len(),
                names.len()
            )));
        }
        let mut seen = BTreeSet::new();
        if !rules.iter().all(|r| seen.insert(r.name)) {
            return Err(bad("duplicate rule name".into()));
        }
        Ok(rules)
    }

    #[derive(Default)]
    struct Arena {
        nodes: Vec<Node>,
        index: HashMap<Node, usize>,
    }

    impl Arena {
        fn intern(&mut self, node: Node) -> Result<usize, Error> {
            if let Some(&id) = self.index.get(&node) {
                return Ok(id);
            }
            if self.nodes.len() >= MAX_TERMS {
                return Err(Error::Limit("witness terms"));
            }
            self.nodes.push(node.clone());
            self.index.insert(node, self.nodes.len() - 1);
            Ok(self.nodes.len() - 1)
        }

        fn import(&mut self, nodes: &[Node]) -> Result<Vec<usize>, Error> {
            let mut map = Vec::with_capacity(nodes.len());
            for (i, n) in nodes.iter().enumerate() {
                let node = match n {
                    Node::App(head, kids) => {
                        let kids = kids
                            .iter()
                            .map(|&k| {
                                if k < i {
                                    Ok(map[k])
                                } else {
                                    Err(Error::Witness("term arena out of order".into()))
                                }
                            })
                            .collect::<Result<_, _>>()?;
                        Node::App(head.clone(), kids)
                    }
                    other => other.clone(),
                };
                map.push(self.intern(node)?);
            }
            Ok(map)
        }

        fn instantiate(&mut self, pat: &Pat, subst: &HashMap<&str, usize>) -> Result<usize, Error> {
            match pat {
                Pat::Var(v) => subst
                    .get(v.as_str())
                    .copied()
                    .ok_or_else(|| Error::Witness(format!("variable {v} is unbound"))),
                Pat::Int(v) => self.intern(Node::Int(*v)),
                Pat::Str(s) => self.intern(Node::Str(s.clone())),
                Pat::App(head, kids) => {
                    let kids = kids
                        .iter()
                        .map(|k| self.instantiate(k, subst))
                        .collect::<Result<_, _>>()?;
                    self.intern(Node::App(head.clone(), kids))
                }
            }
        }

        fn descend(
            &self,
            root: usize,
            path: &[u32],
        ) -> Result<(Vec<(usize, usize)>, usize), Error> {
            if path.len() > MAX_DEPTH {
                return Err(Error::Limit("witness step depth"));
            }
            let mut trail = Vec::with_capacity(path.len());
            let mut at = root;
            for &k in path {
                let Node::App(_, kids) = &self.nodes[at] else {
                    return Err(Error::Witness("step position below a literal".into()));
                };
                let child = *kids
                    .get(k as usize)
                    .ok_or_else(|| Error::Witness("step position out of range".into()))?;
                trail.push((at, k as usize));
                at = child;
            }
            Ok((trail, at))
        }

        fn replace(&mut self, root: usize, path: &[u32], new: usize) -> Result<usize, Error> {
            let (trail, _) = self.descend(root, path)?;
            let mut cur = new;
            for (parent, k) in trail.into_iter().rev() {
                let Node::App(head, kids) = &self.nodes[parent] else {
                    unreachable!();
                };
                let mut kids = kids.clone();
                kids[k] = cur;
                cur = self.intern(Node::App(head.clone(), kids))?;
            }
            Ok(cur)
        }
    }

    pub fn source_term(bindings: &str, root: &str) -> Result<(Vec<Node>, usize), Error> {
        fn build(sx: &Sx, env: &HashMap<String, usize>, arena: &mut Arena) -> Result<usize, Error> {
            match sx {
                Sx::Str(s) => arena.intern(Node::Str(s.clone())),
                Sx::Atom(a) => match int_atom(a) {
                    Some(v) => arena.intern(Node::Int(v)),
                    None => env
                        .get(a)
                        .copied()
                        .ok_or_else(|| Error::Witness(format!("unbound binding {a}"))),
                },
                Sx::List(items) => {
                    let Some(Sx::Atom(head)) = items.first() else {
                        return Err(Error::Witness("binding without a constructor".into()));
                    };
                    let kids = items[1..]
                        .iter()
                        .map(|k| build(k, env, arena))
                        .collect::<Result<_, _>>()?;
                    arena.intern(Node::App(head.clone(), kids))
                }
            }
        }
        let mut arena = Arena::default();
        let mut env = HashMap::new();
        for command in parse_sexps(bindings)? {
            let Sx::List(items) = &command else {
                return Err(Error::Witness("binding is an atom".into()));
            };
            let [Sx::Atom(kw), Sx::Atom(name), body] = items.as_slice() else {
                return Err(Error::Witness("not a let binding".into()));
            };
            if kw != "let" || env.contains_key(name) {
                return Err(Error::Witness(format!("bad binding {name}")));
            }
            let id = build(body, &env, &mut arena)?;
            env.insert(name.clone(), id);
        }
        let root = *env
            .get(root)
            .ok_or_else(|| Error::Witness(format!("no binding {root}")))?;
        Ok((arena.nodes, root))
    }

    pub fn check(
        rules: &[Rule],
        source: (&[Node], usize),
        target: (&[Node], usize),
        witness: &Witness,
    ) -> Result<Summary, Error> {
        if witness.steps.len() > MAX_STEPS {
            return Err(Error::Limit("witness steps"));
        }
        let root_of = |(nodes, root): (&[Node], usize), map: &[usize]| {
            if root < nodes.len() {
                Ok(map[root])
            } else {
                Err(Error::Witness("root out of range".into()))
            }
        };
        let mut arena = Arena::default();
        let smap = arena.import(source.0)?;
        let mut cur = root_of(source, &smap)?;
        let tmap = arena.import(target.0)?;
        let goal = root_of(target, &tmap)?;
        let wmap = arena.import(&witness.terms)?;
        let mut uses = vec![0usize; rules.len()];
        let (mut forward, mut backward) = (0, 0);
        for (n, step) in witness.steps.iter().enumerate() {
            let fail = |m: &str| Error::Witness(format!("step {n}: {m}"));
            let rule = rules.get(step.rule).ok_or_else(|| fail("unknown rule"))?;
            let (from, to) = match step.direction {
                Direction::Forward => (&rule.lhs, &rule.rhs),
                Direction::Backward => (&rule.rhs, &rule.lhs),
            };
            let mut subst = HashMap::new();
            for (var, term) in &step.substitution {
                let id = *wmap.get(*term).ok_or_else(|| fail("term out of range"))?;
                if subst.insert(var.as_str(), id).is_some() {
                    return Err(fail("variable bound twice"));
                }
            }
            let mut vars = BTreeSet::new();
            pat_vars(&rule.lhs, &mut vars);
            pat_vars(&rule.rhs, &mut vars);
            if subst.len() != vars.len() || !vars.iter().all(|v| subst.contains_key(v)) {
                return Err(fail(&format!(
                    "substitution does not bind exactly the variables of {}",
                    rule.name
                )));
            }
            let (_, at) = arena.descend(cur, &step.path)?;
            let matched = arena.instantiate(from, &subst)?;
            if matched != at {
                return Err(fail(&format!(
                    "{} ({:?}) does not match the subterm at {:?}",
                    rule.name, step.direction, step.path
                )));
            }
            let replacement = arena.instantiate(to, &subst)?;
            cur = arena.replace(cur, &step.path, replacement)?;
            uses[step.rule] += 1;
            match step.direction {
                Direction::Forward => forward += 1,
                Direction::Backward => backward += 1,
            }
        }
        if cur != goal {
            return Err(Error::Witness(
                "the replayed term is not the extracted term".into(),
            ));
        }
        Ok(Summary {
            steps: witness.steps.len(),
            forward,
            backward,
            rules: rules
                .iter()
                .zip(uses)
                .filter(|(_, n)| *n > 0)
                .map(|(r, n)| (r.name, r.lemma, n))
                .collect(),
        })
    }
}

#[test]
fn rule_set_and_source_term_match_rust_reference() {
    let t_port = RuleSet::parse(tensor::RULES, tensor::REWRITE_NAMES).unwrap();
    let t_ref = reference::parse_rules(tensor::RULES, tensor::REWRITE_NAMES).unwrap();
    assert_eq!(t_port.rules(), t_ref.as_slice());

    let d_port = RuleSet::parse(dsp::RULES, dsp::REWRITE_NAMES).unwrap();
    let d_ref = reference::parse_rules(dsp::RULES, dsp::REWRITE_NAMES).unwrap();
    assert_eq!(d_port.rules(), d_ref.as_slice());

    let bad_rules: &[(&str, &[&'static str])] = &[
        ("top_atom", &[]),
        ("(", &[]),
        (")", &[]),
        ("(rewrite \"unterminated", &["ew-act"]),
        ("(rewrite \"esc\\ape\" x)", &["ew-act"]),
        ("(rewrite () x)", &["ew-act"]),
        ("(rewrite (123 x) x)", &["ew-act"]),
        ("(rewrite x x)", &["ew-act"]),
        ("(rewrite x x)", &[]),
        ("(rewrite x x)", &["unknown-rule"]),
        ("(rewrite (Act a x) (Act a y))", &["ew-act"]),
        ("(rewrite (Act a x) x :when ((= a 0)))", &["ew-act"]),
    ];
    for &(txt, names) in bad_rules {
        assert_eq!(
            RuleSet::parse(txt, names).err(),
            reference::parse_rules(txt, names).err(),
            "rule text: {txt:?}"
        );
    }

    let bindings_cases = [
        (
            "; comment\n(let t0 (In \"x\"))\n(let t1 (In \"x\"))\n(let t2 (Add (Act -9223372036854775808 t0) (Act 9223372036854775807 t1)))\n",
            "t2",
        ),
        ("(let a 99999999999999999999999999)\n", "a"),
        ("atom\n", "t0"),
        ("(let only_two)\n", "t0"),
        ("(notlet t0 1)\n", "t0"),
        ("(let t0 1)\n(let t0 2)\n", "t0"),
        ("(let t0 () )\n", "t0"),
        ("(let t0 (\"strhead\" 1))\n", "t0"),
        ("(let t0 1)\n", "missing"),
    ];
    for (b, root) in bindings_cases {
        assert_eq!(
            witness::source_term(b, root),
            reference::source_term(b, root),
            "bindings: {b:?}"
        );
    }
    let cases = 2 + bad_rules.len() + bindings_cases.len();
    println!("flip-gate differential differences=0 cases={cases}");
}

#[test]
fn witness_check_matches_rust_reference_across_valid_and_tampered_cases() {
    let section = Biquad::new([0.2, 0.1, 0.05], [-0.3, 0.1]).unwrap();

    let mut g = TensorGraph::new();
    let x = g.input("x", DType::F32, 64).unwrap();
    let b = g.input("b", DType::F32, 24).unwrap();
    let w = g.matrix("w", 24, 64, WeightFormat::Q8_0).unwrap();
    let y = g.matvec(w, x).unwrap();
    let y = g.add(y, b).unwrap();
    let y = g.mul(y, b).unwrap();
    let y = g.scale_f32(0.5, y).unwrap();
    let y = g.activation(Activation::Silu, y).unwrap();
    let fused_t = g.fuse(y).unwrap();

    let mut dg = DspGraph::new();
    let s = dg.source("in").unwrap();
    let a = dg.gain(0.5, s).unwrap();
    let a = dg.pan(0.25, a).unwrap();
    let a = dg.gain(0.75, a).unwrap();
    let a = dg.mix(a, s).unwrap();
    let a = dg.gain(0.9, a).unwrap();
    let a = dg.biquad(section, a).unwrap();
    let a = dg.biquad(section, a).unwrap();
    let fused_d = dg.fuse(a).unwrap();

    // Direct `witness::check` differential on explicit arena + mutations.
    let set = tensor::rule_set().unwrap();
    let ref_rules = reference::parse_rules(tensor::RULES, tensor::REWRITE_NAMES).unwrap();
    let (src, sroot) =
        witness::source_term("(let t0 (In \"x\"))\n(let t1 (Act 0 t0))\n", "t1").unwrap();
    let (tgt, troot) =
        witness::source_term("(let t0 (Fuse (EAct 0 (Lane (In \"x\")))))\n", "t0").unwrap();
    let mut terms = src.clone();
    terms.push(Node::Int(0));
    let a_idx = terms.len() - 1;
    let x_idx = src
        .iter()
        .position(|n| matches!(n, Node::App(h, _) if h == "In"))
        .unwrap();
    let base = Witness {
        terms,
        steps: vec![Step {
            rule: 0,
            direction: Direction::Forward,
            path: Vec::new(),
            substitution: vec![("a".into(), a_idx), ("x".into(), x_idx)],
        }],
    };

    let mut candidates = vec![base.clone()];
    for edit in [
        |w: &mut Witness| w.steps.clear(),
        |w: &mut Witness| w.steps[0].rule = 99,
        |w: &mut Witness| w.steps[0].direction = Direction::Backward,
        |w: &mut Witness| w.steps[0].path.push(0),
        |w: &mut Witness| w.steps[0].path.push(9),
        |w: &mut Witness| w.steps[0].substitution.clear(),
        |w: &mut Witness| {
            let d = w.steps[0].substitution[0].clone();
            w.steps[0].substitution.push(d);
        },
        |w: &mut Witness| w.steps[0].substitution[0].1 = 99,
        |w: &mut Witness| {
            w.terms.insert(0, Node::App("BadOrder".into(), vec![1]));
        },
    ] {
        let mut c = base.clone();
        edit(&mut c);
        candidates.push(c);
    }

    for cand in &candidates {
        assert_eq!(
            witness::check(set, (&src, sroot), (&tgt, troot), cand),
            reference::check(&ref_rules, (&src, sroot), (&tgt, troot), cand),
        );
    }
    assert_eq!(
        witness::check(set, (&src, 99), (&tgt, troot), &base),
        reference::check(&ref_rules, (&src, 99), (&tgt, troot), &base),
    );

    // Multi-step end-to-end witness check timing comparison (logged with --nocapture).
    let d_set = dsp::rule_set().unwrap();
    let d_ref_rules = reference::parse_rules(dsp::RULES, dsp::REWRITE_NAMES).unwrap();

    // Build a multi-step realistic DSP chain and compare `witness::check` vs `reference::check`.
    let big_bindings = concat!(
        "(let s0 (Src \"in\"))\n",
        "(let a0 (Gain 1056964608 s0))\n",
        "(let a1 (Pan 1048576000 a0))\n",
        "(let a2 (Gain 1061158912 a1))\n",
        "(let a3 (Mix a2 s0))\n",
        "(let a4 (Gain 1063675494 a3))\n",
        "(let a5 (Biquad 0 a4))\n",
        "(let a6 (Biquad 0 a5))\n",
        "(let a7 (Biquad 0 a6))\n",
    );
    let (big_src, big_sroot) = witness::source_term(big_bindings, "a7").unwrap();
    let big_fused = vesper_fusion::engine::fuse(dsp::RULES, d_set, big_bindings, "a7").unwrap();
    let big_tgt = (&big_fused.term.nodes[..], big_fused.term.root);

    let t0 = Instant::now();
    for _ in 0..BENCH_ROUNDS {
        let s = witness::check(d_set, (&big_src, big_sroot), big_tgt, &big_fused.witness).unwrap();
        std::hint::black_box(s);
    }
    let port_elapsed = t0.elapsed();

    let t1 = Instant::now();
    for _ in 0..BENCH_ROUNDS {
        let s = reference::check(
            &d_ref_rules,
            (&big_src, big_sroot),
            big_tgt,
            &big_fused.witness,
        )
        .unwrap();
        std::hint::black_box(s);
    }
    let ref_elapsed = t1.elapsed();
    eprintln!(
        "vesper-fusion witness::check ({BENCH_ROUNDS} rounds, {} steps): port={:?}, rust={:?}, ratio={:.3}x",
        big_fused.witness.steps.len(),
        port_elapsed,
        ref_elapsed,
        port_elapsed.as_secs_f64() / ref_elapsed.as_secs_f64().max(1e-9)
    );
    assert_eq!(
        &fused_t.check_witness(fused_t.witness()).unwrap(),
        fused_t.witness_summary()
    );
    assert_eq!(
        &fused_d.check_witness(fused_d.witness()).unwrap(),
        fused_d.witness_summary()
    );
    let cases = candidates.len() + 4;
    println!("flip-gate differential differences=0 cases={cases}");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_witness() {
    use std::hint::black_box;

    let d_set = dsp::rule_set().unwrap();
    let d_ref_rules = reference::parse_rules(dsp::RULES, dsp::REWRITE_NAMES).unwrap();
    let big_bindings = concat!(
        "(let s0 (Src \"in\"))\n",
        "(let a0 (Gain 1056964608 s0))\n",
        "(let a1 (Pan 1048576000 a0))\n",
        "(let a2 (Gain 1061158912 a1))\n",
        "(let a3 (Mix a2 s0))\n",
        "(let a4 (Gain 1063675494 a3))\n",
        "(let a5 (Biquad 0 a4))\n",
        "(let a6 (Biquad 0 a5))\n",
        "(let a7 (Biquad 0 a6))\n",
    );
    let (big_src, big_sroot) = witness::source_term(big_bindings, "a7").unwrap();
    let big_fused = vesper_fusion::engine::fuse(dsp::RULES, d_set, big_bindings, "a7").unwrap();
    let big_tgt = (&big_fused.term.nodes[..], big_fused.term.root);

    let run_rust_check = || {
        for _ in 0..ABBA_CHECK_OPS {
            let s = reference::check(
                black_box(&d_ref_rules),
                black_box((&big_src, big_sroot)),
                black_box(big_tgt),
                black_box(&big_fused.witness),
            )
            .unwrap();
            black_box(s);
        }
    };
    let run_vesper_check = || {
        for _ in 0..ABBA_CHECK_OPS {
            let s = witness::check(
                black_box(d_set),
                black_box((&big_src, big_sroot)),
                black_box(big_tgt),
                black_box(&big_fused.witness),
            )
            .unwrap();
            black_box(s);
        }
    };

    for _ in 0..ABBA_WARMUP_EPOCHS {
        run_rust_check();
        run_vesper_check();
        run_vesper_check();
        run_rust_check();
    }

    let mut check_pairs = Vec::with_capacity(ABBA_EPOCHS);
    for _ in 0..ABBA_EPOCHS {
        let t0 = Instant::now();
        run_rust_check();
        let a1 = t0.elapsed().as_nanos().max(1);

        let t1 = Instant::now();
        run_vesper_check();
        let b1 = t1.elapsed().as_nanos().max(1);

        let t2 = Instant::now();
        run_vesper_check();
        let b2 = t2.elapsed().as_nanos().max(1);

        let t3 = Instant::now();
        run_rust_check();
        let a2 = t3.elapsed().as_nanos().max(1);

        check_pairs.push(format!("{}:{}", a1 + a2, b1 + b2));
    }
    println!("flip-gate pairs witness_check {}", check_pairs.join(" "));

    let run_rust_parse = || {
        for _ in 0..ABBA_PARSE_OPS {
            let r1 =
                reference::parse_rules(black_box(tensor::RULES), black_box(tensor::REWRITE_NAMES))
                    .unwrap();
            let r2 = reference::parse_rules(black_box(dsp::RULES), black_box(dsp::REWRITE_NAMES))
                .unwrap();
            let st = reference::source_term(black_box(big_bindings), black_box("a7")).unwrap();
            black_box((r1, r2, st));
        }
    };
    let run_vesper_parse = || {
        for _ in 0..ABBA_PARSE_OPS {
            let r1 =
                RuleSet::parse(black_box(tensor::RULES), black_box(tensor::REWRITE_NAMES)).unwrap();
            let r2 = RuleSet::parse(black_box(dsp::RULES), black_box(dsp::REWRITE_NAMES)).unwrap();
            let st = witness::source_term(black_box(big_bindings), black_box("a7")).unwrap();
            black_box((r1, r2, st));
        }
    };

    for _ in 0..ABBA_WARMUP_EPOCHS {
        run_rust_parse();
        run_vesper_parse();
        run_vesper_parse();
        run_rust_parse();
    }

    let mut parse_pairs = Vec::with_capacity(ABBA_EPOCHS);
    for _ in 0..ABBA_EPOCHS {
        let t0 = Instant::now();
        run_rust_parse();
        let a1 = t0.elapsed().as_nanos().max(1);

        let t1 = Instant::now();
        run_vesper_parse();
        let b1 = t1.elapsed().as_nanos().max(1);

        let t2 = Instant::now();
        run_vesper_parse();
        let b2 = t2.elapsed().as_nanos().max(1);

        let t3 = Instant::now();
        run_rust_parse();
        let a2 = t3.elapsed().as_nanos().max(1);

        parse_pairs.push(format!("{}:{}", a1 + a2, b1 + b2));
    }
    println!("flip-gate pairs witness_parse {}", parse_pairs.join(" "));
}
