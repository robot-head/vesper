//! Rewrite witnesses (REVIEW-STATUS COMP-03).
//!
//! Equality saturation decides that the extracted term is equal to the input
//! graph, but the extraction alone does not say *why*. Before a fused term is
//! used, the engine explains `root = extracted` as a [`Witness`] with one
//! [`Step`] per applied rewrite (see [`crate::engine`]), and this module
//! checks the witness with [`check`], which does not use the engine at all:
//!
//! * the source term is rebuilt from the graph's own `let` bindings
//!   ([`source_term`]), not taken from egglog;
//! * every step names a rule of the [`RuleSet`], and every rule of the set has
//!   a refinement lemma in `lean/FusionRules.lean` ([`crate::RULE_LEMMAS`]);
//! * every step's substitution binds exactly the rule's variables, the rule's
//!   left side (right side for a backward step) instantiated with it is the
//!   subterm at the step's position, and the subterm is replaced by the other
//!   side instantiated with the same substitution;
//! * the replay ends at the extracted term the fused evaluator runs.
//!
//! A backward step uses a rule right to left. Each lemma states that both
//! sides denote the same value (`⟦rhs⟧ = ⟦lhs⟧`), and the operator models are
//! compositional, so a step in either direction at any position preserves the
//! term's denotation; a checked witness is therefore a chain of lemma
//! instances from the source to the target. What is checked is the chain's
//! syntax; that each lemma's model matches the Rust evaluators is the job of
//! the crate's differential tests, as before.
//!
//! Terms live in hash-consed arenas ([`Node`]): structurally equal subterms
//! share one index, so term equality is index equality, and a step rebuilds
//! only the path from the root to its position.
use crate::engine::Node;
use crate::{Error, RULE_LEMMAS};
use std::collections::{BTreeSet, HashMap};

/// Maximum steps in one witness; a longer witness is refused.
pub const MAX_STEPS: usize = 1 << 18;
/// Maximum nodes in one witness term arena.
pub const MAX_TERMS: usize = 1 << 18;
/// Maximum depth of a step position.
pub const MAX_DEPTH: usize = 4096;

/// Which way a step uses its rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Left side to right side, as the rule is written.
    Forward,
    /// Right side to left side.
    Backward,
}

/// One applied rewrite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// Index of the rule in the [`RuleSet`].
    pub rule: usize,
    /// Direction the rule is used in.
    pub direction: Direction,
    /// Child indices from the root of the current term to the rewritten
    /// subterm.
    pub path: Vec<u32>,
    /// Pattern variable to term (an index into [`Witness::terms`]), sorted by
    /// variable name.
    pub substitution: Vec<(String, usize)>,
}

/// A rewrite chain from the source term to the extracted term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Witness {
    /// Terms the substitutions refer to; children precede parents.
    pub terms: Vec<Node>,
    /// The steps, in order.
    pub steps: Vec<Step>,
}

/// What a checked witness establishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Number of steps.
    pub steps: usize,
    /// Steps that use their rule left to right.
    pub forward: usize,
    /// Steps that use their rule right to left.
    pub backward: usize,
    /// Rule name, its Lean lemma, and how many steps used it, in rule order;
    /// rules no step used are left out.
    pub rules: Vec<(&'static str, &'static str, usize)>,
}

/// A rewrite pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pat {
    /// Pattern variable.
    Var(String),
    /// Integer literal.
    Int(i64),
    /// String literal.
    Str(String),
    /// Constructor application.
    App(String, Vec<Pat>),
}

impl Pat {
    fn vars<'a>(&'a self, out: &mut BTreeSet<&'a str>) {
        match self {
            Pat::Var(v) => {
                out.insert(v);
            }
            Pat::App(_, kids) => kids.iter().for_each(|k| k.vars(out)),
            Pat::Int(_) | Pat::Str(_) => {}
        }
    }
}

/// A named rewrite `lhs => rhs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The rule's name in [`crate::RULE_LEMMAS`].
    pub name: &'static str,
    /// The Lean lemma that refines it.
    pub lemma: &'static str,
    /// Left side.
    pub lhs: Pat,
    /// Right side.
    pub rhs: Pat,
}

/// The rewrites of a rule set, parsed from its egglog text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleSet {
    rules: Vec<Rule>,
}

impl RuleSet {
    /// Parses every `(rewrite lhs rhs)` command of `text`, naming them with
    /// `names` in order. Fails unless there is exactly one name per rewrite,
    /// every name has a lemma in [`crate::RULE_LEMMAS`], no rewrite carries
    /// options, and each rule's right side uses only its left side's variables.
    pub fn parse(text: &str, names: &[&'static str]) -> Result<Self, Error> {
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
            lhs.vars(&mut lv);
            rhs.vars(&mut rv);
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
        Ok(Self { rules })
    }

    /// The rules, in rule-set order.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub(crate) fn find(&self, lhs: &Pat, rhs: &Pat) -> Option<usize> {
        self.rules
            .iter()
            .position(|r| &r.lhs == lhs && &r.rhs == rhs)
    }
}

/// The `(lhs, rhs)` patterns of every rewrite of a program text, in order
/// (how the engine numbers the rules of its steps).
pub(crate) fn program_rewrites(text: &str) -> Result<Vec<(Pat, Pat)>, Error> {
    let mut out = Vec::new();
    for command in parse_sexps(text)? {
        if let Sx::List(items) = &command
            && matches!(items.first(), Some(Sx::Atom(head)) if head == "rewrite")
        {
            let [_, lhs, rhs] = items.as_slice() else {
                return Err(Error::Witness(
                    "a rewrite with options or the wrong arity".into(),
                ));
            };
            out.push((pattern(lhs)?, pattern(rhs)?));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// S-expressions.

#[derive(Clone, Debug, PartialEq, Eq)]
enum Sx {
    Atom(String),
    Str(String),
    List(Vec<Sx>),
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

// ---------------------------------------------------------------------------
// Hash-consed term arena.

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

    /// Imports a whole arena whose children precede their parents; returns
    /// the new index of every node.
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

    /// Ancestors of the subterm at `path` (with the child index taken from
    /// each) and the subterm itself.
    fn descend(&self, root: usize, path: &[u32]) -> Result<(Vec<(usize, usize)>, usize), Error> {
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
                unreachable!("descend only passes through applications");
            };
            let mut kids = kids.clone();
            kids[k] = cur;
            cur = self.intern(Node::App(head.clone(), kids))?;
        }
        Ok(cur)
    }
}

// ---------------------------------------------------------------------------
// The source term, from the graph's own bindings.

/// Builds the term named `root` from egglog `(let name expr)` bindings, as a
/// hash-consed arena (children precede parents). This is the checker's view
/// of the input graph; it does not run egglog.
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

// ---------------------------------------------------------------------------
// The checker.

/// Replays `witness` from `source` and checks that it ends at `target`.
/// Both terms are `(arena, root)` pairs whose children precede parents.
pub fn check(
    rules: &RuleSet,
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
    let mut uses = vec![0usize; rules.rules.len()];
    let (mut forward, mut backward) = (0, 0);
    for (n, step) in witness.steps.iter().enumerate() {
        let fail = |m: &str| Error::Witness(format!("step {n}: {m}"));
        let rule = rules
            .rules
            .get(step.rule)
            .ok_or_else(|| fail("unknown rule"))?;
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
        rule.lhs.vars(&mut vars);
        rule.rhs.vars(&mut vars);
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
            .rules
            .iter()
            .zip(uses)
            .filter(|(_, n)| *n > 0)
            .map(|(r, n)| (r.name, r.lemma, n))
            .collect(),
    })
}
