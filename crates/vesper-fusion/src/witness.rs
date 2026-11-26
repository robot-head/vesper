//! Rewrite witnesses (REVIEW-STATUS COMP-03).
//!
//! Equality saturation decides that the extracted term is equal to the input
//! graph, but the extraction alone does not say *why*. Before a fused term is
//! used, the engine explains `root = extracted` as a [`Witness`] with one
//! [`Step`] per applied rewrite (see [`crate::engine`]), and this module
//! checks the witness with [`check`], which does not use the saturation engine:
//!
//! * the source term is rebuilt from the graph's own `let` bindings
//!   ([`source_term`]), not taken from the saturation engine;
//! * every step names a rule of the [`RuleSet`], and every rule of the set has
//!   a refinement lemma in `lean/FusionRules.lean` ([`crate::RULE_LEMMAS`]);
//! * every step's substitution binds exactly the rule's variables, the rule's
//!   left side (right side for a backward step) instantiated with it is the
//!   subterm at the step's position, and the subterm is replaced by the other
//!   side instantiated with the same substitution;
//! * the replay ends at the extracted term the fused evaluator runs.
//!
//! S-expression parsing, pattern extraction, hash-consed source-term building,
//! and witness replay run in `@vesper/fusion`'s independent `witness.ves`
//! module (Rust-exit wave SH6, lane 18, #1413 / #2239).
use crate::binding;
use crate::engine::Node;
use crate::{Error, RULE_LEMMAS};
use std::collections::{BTreeSet, HashMap};

/// Maximum steps in one witness; a longer witness is refused.
pub const MAX_STEPS: usize = 1 << 18;
/// Maximum nodes in one witness term arena.
pub const MAX_TERMS: usize = 1 << 18;
/// Maximum depth of a step position.
pub const MAX_DEPTH: usize = 4096;

// Wire protocol constants shared with `vesper/src/witness.ves`.
const PARSE_MODE_PROGRAM: u64 = 0;
const PARSE_MODE_RULE_SET: u64 = 1;

const PARSE_STATUS_OK: u64 = 0;
const PARSE_STATUS_TOO_DEEP: u64 = 1;
const PARSE_STATUS_UNBALANCED: u64 = 2;
const PARSE_STATUS_ESCAPE: u64 = 3;
const PARSE_STATUS_UNTERMINATED: u64 = 4;
const PARSE_STATUS_TOP_LEVEL_ATOM: u64 = 5;
const PARSE_STATUS_REWRITE_ARITY: u64 = 6;
const PARSE_STATUS_NO_CONSTRUCTOR: u64 = 7;
const PARSE_STATUS_NUMERIC_HEAD: u64 = 8;
const PARSE_STATUS_NOT_CTOR_REWRITE: u64 = 9;

const SOURCE_STATUS_OK: u64 = 0;
const SOURCE_STATUS_TOO_DEEP: u64 = 1;
const SOURCE_STATUS_UNBALANCED: u64 = 2;
const SOURCE_STATUS_ESCAPE: u64 = 3;
const SOURCE_STATUS_UNTERMINATED: u64 = 4;
const SOURCE_STATUS_BINDING_ATOM: u64 = 5;
const SOURCE_STATUS_NOT_LET: u64 = 6;
const SOURCE_STATUS_BAD_BINDING: u64 = 7;
const SOURCE_STATUS_UNBOUND: u64 = 8;
const SOURCE_STATUS_NO_CTOR: u64 = 9;
const SOURCE_STATUS_NO_ROOT: u64 = 10;
const SOURCE_STATUS_LIMIT_TERMS: u64 = 11;

const CHECK_STATUS_OK: u64 = 0;
const CHECK_STATUS_LIMIT_STEPS: u64 = 1;
const CHECK_STATUS_LIMIT_TERMS: u64 = 2;
const CHECK_STATUS_LIMIT_DEPTH: u64 = 3;
const CHECK_STATUS_ROOT_OOB: u64 = 4;
const CHECK_STATUS_ARENA_ORDER: u64 = 5;
const CHECK_STATUS_UNKNOWN_RULE: u64 = 6;
const CHECK_STATUS_TERM_OOB: u64 = 7;
const CHECK_STATUS_VAR_TWICE: u64 = 8;
const CHECK_STATUS_SUBST_MISMATCH: u64 = 9;
const CHECK_STATUS_UNBOUND_VAR: u64 = 10;
const CHECK_STATUS_BELOW_LITERAL: u64 = 11;
const CHECK_STATUS_POS_OOB: u64 = 12;
const CHECK_STATUS_SUBTERM_MISMATCH: u64 = 13;
const CHECK_STATUS_GOAL_MISMATCH: u64 = 14;

const PAT_TAG_VAR: u64 = 0;
const PAT_TAG_INT: u64 = 1;
const PAT_TAG_STR: u64 = 2;
const PAT_TAG_APP: u64 = 3;

const NODE_TAG_INT: u64 = 0;
const NODE_TAG_STR: u64 = 1;
const NODE_TAG_APP: u64 = 2;

const STEP_DIR_FORWARD: u64 = 0;
const STEP_DIR_BACKWARD: u64 = 1;

const PARSE_HEADER_WORDS: usize = 3;
const PARSE_WORD_DETAIL: usize = 1;
const PARSE_WORD_COUNT: usize = 2;

const SOURCE_HEADER_WORDS: usize = 5;
const SOURCE_WORD_SPAN_START: usize = 1;
const SOURCE_WORD_SPAN_LEN: usize = 2;
const SOURCE_WORD_ROOT_ID: usize = 3;
const SOURCE_WORD_NODE_COUNT: usize = 4;

const CHECK_SUMMARY_HEADER_WORDS: usize = 3;
const CHECK_WORD_DETAIL: usize = 1;
const CHECK_WORD_FORWARD: usize = 1;
const CHECK_WORD_BACKWARD: usize = 2;

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
        let words = binding::fusion_parse_rewrites(text, PARSE_MODE_RULE_SET);
        let status = words.first().copied().unwrap_or(u64::MAX);
        let detail = words.get(PARSE_WORD_DETAIL).copied().unwrap_or(0) as usize;
        if status != PARSE_STATUS_OK {
            return Err(match status {
                PARSE_STATUS_TOO_DEEP => Error::Witness("s-expression: too deep".into()),
                PARSE_STATUS_UNBALANCED => Error::Witness("s-expression: unbalanced".into()),
                PARSE_STATUS_ESCAPE => {
                    Error::Witness("s-expression: escapes are not used by the rule sets".into())
                }
                PARSE_STATUS_UNTERMINATED => {
                    Error::Witness("s-expression: unterminated string".into())
                }
                PARSE_STATUS_TOP_LEVEL_ATOM => bad("top-level atom".into()),
                PARSE_STATUS_REWRITE_ARITY => {
                    bad("a rewrite with options or the wrong arity".into())
                }
                PARSE_STATUS_NO_CONSTRUCTOR => {
                    Error::Witness("pattern without a constructor".into())
                }
                PARSE_STATUS_NUMERIC_HEAD => Error::Witness("pattern head is a number".into()),
                PARSE_STATUS_NOT_CTOR_REWRITE => {
                    let name = *names
                        .get(detail)
                        .ok_or_else(|| bad(format!("rewrite {detail} has no name")))?;
                    let _lemma = lookup_lemma(name)?;
                    bad(format!("rule {name} is not a constructor rewrite"))
                }
                _ => bad("unexpected parser status".into()),
            });
        }

        let pairs = decode_rewrite_pairs(text, &words)?;
        let mut rules = Vec::with_capacity(pairs.len());
        for (index, (lhs, rhs)) in pairs.into_iter().enumerate() {
            let name = *names
                .get(index)
                .ok_or_else(|| bad(format!("rewrite {index} has no name")))?;
            let lemma = lookup_lemma(name)?;
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

fn lookup_lemma(name: &str) -> Result<&'static str, Error> {
    RULE_LEMMAS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, l)| *l)
        .ok_or_else(|| Error::Witness(format!("rule set: rule {name} has no Lean lemma")))
}

/// The `(lhs, rhs)` patterns of every rewrite of a program text, in order
/// (how the engine numbers the rules of its steps).
pub(crate) fn program_rewrites(text: &str) -> Result<Vec<(Pat, Pat)>, Error> {
    let words = binding::fusion_parse_rewrites(text, PARSE_MODE_PROGRAM);
    let status = words.first().copied().unwrap_or(u64::MAX);
    if status != PARSE_STATUS_OK {
        return Err(match status {
            PARSE_STATUS_TOO_DEEP => Error::Witness("s-expression: too deep".into()),
            PARSE_STATUS_UNBALANCED => Error::Witness("s-expression: unbalanced".into()),
            PARSE_STATUS_ESCAPE => {
                Error::Witness("s-expression: escapes are not used by the rule sets".into())
            }
            PARSE_STATUS_UNTERMINATED => Error::Witness("s-expression: unterminated string".into()),
            PARSE_STATUS_REWRITE_ARITY => {
                Error::Witness("a rewrite with options or the wrong arity".into())
            }
            PARSE_STATUS_NO_CONSTRUCTOR => Error::Witness("pattern without a constructor".into()),
            PARSE_STATUS_NUMERIC_HEAD => Error::Witness("pattern head is a number".into()),
            _ => Error::Witness("unexpected rewrite parser status".into()),
        });
    }
    decode_rewrite_pairs(text, &words)
}

fn decode_rewrite_pairs(text: &str, words: &[u64]) -> Result<Vec<(Pat, Pat)>, Error> {
    let count = *words
        .get(PARSE_WORD_COUNT)
        .ok_or_else(|| Error::Witness("truncated pattern words".into()))? as usize;
    let mut at = PARSE_HEADER_WORDS;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let lhs = decode_preorder_pat(text, words, &mut at)?;
        let rhs = decode_preorder_pat(text, words, &mut at)?;
        out.push((lhs, rhs));
    }
    Ok(out)
}

fn decode_preorder_pat(text: &str, words: &[u64], at: &mut usize) -> Result<Pat, Error> {
    let next_u64 = |pos: &mut usize| -> Result<u64, Error> {
        let w = *words
            .get(*pos)
            .ok_or_else(|| Error::Witness("truncated pattern stream".into()))?;
        *pos += 1;
        Ok(w)
    };
    let slice_str = |pos: &mut usize| -> Result<String, Error> {
        let start = next_u64(pos)? as usize;
        let len = next_u64(pos)? as usize;
        text.get(start..start.saturating_add(len))
            .map(str::to_owned)
            .ok_or_else(|| Error::Witness("invalid pattern text span".into()))
    };

    match next_u64(at)? {
        PAT_TAG_VAR => Ok(Pat::Var(slice_str(at)?)),
        PAT_TAG_INT => Ok(Pat::Int(next_u64(at)? as i64)),
        PAT_TAG_STR => Ok(Pat::Str(slice_str(at)?)),
        PAT_TAG_APP => {
            let head = slice_str(at)?;
            let count = next_u64(at)? as usize;
            let mut kids = Vec::with_capacity(count);
            for _ in 0..count {
                kids.push(decode_preorder_pat(text, words, at)?);
            }
            Ok(Pat::App(head, kids))
        }
        _ => Err(Error::Witness("unknown pattern tag".into())),
    }
}

// ---------------------------------------------------------------------------
// The source term, from the graph's own bindings.

/// Builds the term named `root` from egglog `(let name expr)` bindings, as a
/// hash-consed arena (children precede parents). This is the checker's view
/// of the input graph; it does not run the saturation engine.
pub fn source_term(bindings: &str, root: &str) -> Result<(Vec<Node>, usize), Error> {
    let words = binding::fusion_source_term(bindings, root);
    let status = words.first().copied().unwrap_or(u64::MAX);
    if status != SOURCE_STATUS_OK {
        let span = || {
            let s = words.get(SOURCE_WORD_SPAN_START).copied().unwrap_or(0) as usize;
            let l = words.get(SOURCE_WORD_SPAN_LEN).copied().unwrap_or(0) as usize;
            bindings.get(s..s.saturating_add(l)).unwrap_or("")
        };
        return Err(match status {
            SOURCE_STATUS_TOO_DEEP => Error::Witness("s-expression: too deep".into()),
            SOURCE_STATUS_UNBALANCED => Error::Witness("s-expression: unbalanced".into()),
            SOURCE_STATUS_ESCAPE => {
                Error::Witness("s-expression: escapes are not used by the rule sets".into())
            }
            SOURCE_STATUS_UNTERMINATED => {
                Error::Witness("s-expression: unterminated string".into())
            }
            SOURCE_STATUS_BINDING_ATOM => Error::Witness("binding is an atom".into()),
            SOURCE_STATUS_NOT_LET => Error::Witness("not a let binding".into()),
            SOURCE_STATUS_BAD_BINDING => Error::Witness(format!("bad binding {}", span())),
            SOURCE_STATUS_UNBOUND => Error::Witness(format!("unbound binding {}", span())),
            SOURCE_STATUS_NO_CTOR => Error::Witness("binding without a constructor".into()),
            SOURCE_STATUS_NO_ROOT => Error::Witness(format!("no binding {root}")),
            SOURCE_STATUS_LIMIT_TERMS => Error::Limit("witness terms"),
            _ => Error::Witness("unexpected source_term status".into()),
        });
    }

    if words.len() < SOURCE_HEADER_WORDS {
        return Err(Error::Witness("truncated source_term output".into()));
    }
    let root_id = words[SOURCE_WORD_ROOT_ID] as usize;
    let node_count = words[SOURCE_WORD_NODE_COUNT] as usize;
    let mut at = SOURCE_HEADER_WORDS;
    let next_u64 = |pos: &mut usize| -> Result<u64, Error> {
        let w = *words
            .get(*pos)
            .ok_or_else(|| Error::Witness("truncated source_term arena".into()))?;
        *pos += 1;
        Ok(w)
    };
    let slice_str = |pos: &mut usize| -> Result<String, Error> {
        let start = next_u64(pos)? as usize;
        let len = next_u64(pos)? as usize;
        bindings
            .get(start..start.saturating_add(len))
            .map(str::to_owned)
            .ok_or_else(|| Error::Witness("invalid source_term span".into()))
    };

    let mut nodes = Vec::with_capacity(node_count);
    for _ in 0..node_count {
        let node = match next_u64(&mut at)? {
            NODE_TAG_INT => Node::Int(next_u64(&mut at)? as i64),
            NODE_TAG_STR => Node::Str(slice_str(&mut at)?),
            NODE_TAG_APP => {
                let head = slice_str(&mut at)?;
                let k_len = next_u64(&mut at)? as usize;
                let mut kids = Vec::with_capacity(k_len);
                for _ in 0..k_len {
                    kids.push(next_u64(&mut at)? as usize);
                }
                Node::App(head, kids)
            }
            _ => return Err(Error::Witness("unknown source_term node tag".into())),
        };
        nodes.push(node);
    }
    Ok((nodes, root_id))
}

// ---------------------------------------------------------------------------
// The checker.

struct SymTable<'a> {
    ids: HashMap<&'a str, u64>,
}

impl<'a> SymTable<'a> {
    fn new() -> Self {
        Self {
            ids: HashMap::new(),
        }
    }

    fn intern(&mut self, s: &'a str) -> u64 {
        let next = self.ids.len() as u64;
        *self.ids.entry(s).or_insert(next)
    }
}

fn encode_postorder_pat<'a>(pat: &'a Pat, syms: &mut SymTable<'a>, out: &mut Vec<u64>) {
    fn emit<'a>(p: &'a Pat, syms: &mut SymTable<'a>, words: &mut Vec<u64>, count: &mut u64) -> u64 {
        match p {
            Pat::Var(v) => {
                let id = *count;
                *count += 1;
                words.push(PAT_TAG_VAR);
                words.push(syms.intern(v));
                id
            }
            Pat::Int(v) => {
                let id = *count;
                *count += 1;
                words.push(PAT_TAG_INT);
                words.push(*v as u64);
                id
            }
            Pat::Str(s) => {
                let id = *count;
                *count += 1;
                words.push(PAT_TAG_STR);
                words.push(syms.intern(s));
                id
            }
            Pat::App(head, kids) => {
                let child_ids: Vec<u64> =
                    kids.iter().map(|k| emit(k, syms, words, count)).collect();
                let id = *count;
                *count += 1;
                words.push(PAT_TAG_APP);
                words.push(syms.intern(head));
                words.push(child_ids.len() as u64);
                words.extend(child_ids);
                id
            }
        }
    }

    let len_slot = out.len();
    out.push(0);
    let mut node_count = 0u64;
    emit(pat, syms, out, &mut node_count);
    out[len_slot] = node_count;
}

fn encode_nodes<'a>(nodes: &'a [Node], syms: &mut SymTable<'a>, out: &mut Vec<u64>) {
    out.push(nodes.len() as u64);
    for n in nodes {
        match n {
            Node::Int(v) => {
                out.push(NODE_TAG_INT);
                out.push(*v as u64);
            }
            Node::Str(s) => {
                out.push(NODE_TAG_STR);
                out.push(syms.intern(s));
            }
            Node::App(head, kids) => {
                out.push(NODE_TAG_APP);
                out.push(syms.intern(head));
                out.push(kids.len() as u64);
                out.extend(kids.iter().map(|&k| k as u64));
            }
        }
    }
}

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

    let mut syms = SymTable::new();
    let mut words = Vec::new();
    words.push(rules.rules.len() as u64);
    for r in &rules.rules {
        encode_postorder_pat(&r.lhs, &mut syms, &mut words);
        encode_postorder_pat(&r.rhs, &mut syms, &mut words);
    }
    encode_nodes(source.0, &mut syms, &mut words);
    words.push(source.1 as u64);
    encode_nodes(target.0, &mut syms, &mut words);
    words.push(target.1 as u64);
    encode_nodes(&witness.terms, &mut syms, &mut words);
    words.push(witness.steps.len() as u64);
    for step in &witness.steps {
        words.push(step.rule as u64);
        words.push(match step.direction {
            Direction::Forward => STEP_DIR_FORWARD,
            Direction::Backward => STEP_DIR_BACKWARD,
        });
        words.push(step.path.len() as u64);
        words.extend(step.path.iter().map(|&p| u64::from(p)));
        words.push(step.substitution.len() as u64);
        for (var, term) in &step.substitution {
            words.push(syms.intern(var));
            words.push(*term as u64);
        }
    }

    let out = binding::fusion_check_witness(&words);
    let status = out.first().copied().unwrap_or(u64::MAX);
    let detail = out.get(CHECK_WORD_DETAIL).copied().unwrap_or(0) as usize;
    match status {
        CHECK_STATUS_OK => {
            if out.len() < CHECK_SUMMARY_HEADER_WORDS + rules.rules.len() {
                return Err(Error::Witness("truncated witness summary".into()));
            }
            let forward = out[CHECK_WORD_FORWARD] as usize;
            let backward = out[CHECK_WORD_BACKWARD] as usize;
            let used_rules = rules
                .rules
                .iter()
                .zip(&out[CHECK_SUMMARY_HEADER_WORDS..])
                .filter(|(_, n)| **n > 0)
                .map(|(r, n)| (r.name, r.lemma, *n as usize))
                .collect();
            Ok(Summary {
                steps: witness.steps.len(),
                forward,
                backward,
                rules: used_rules,
            })
        }
        CHECK_STATUS_LIMIT_STEPS => Err(Error::Limit("witness steps")),
        CHECK_STATUS_LIMIT_TERMS => Err(Error::Limit("witness terms")),
        CHECK_STATUS_LIMIT_DEPTH => Err(Error::Limit("witness step depth")),
        CHECK_STATUS_ROOT_OOB => Err(Error::Witness("root out of range".into())),
        CHECK_STATUS_ARENA_ORDER => Err(Error::Witness("term arena out of order".into())),
        CHECK_STATUS_UNKNOWN_RULE => Err(Error::Witness(format!("step {detail}: unknown rule"))),
        CHECK_STATUS_TERM_OOB => Err(Error::Witness(format!("step {detail}: term out of range"))),
        CHECK_STATUS_VAR_TWICE => Err(Error::Witness(format!(
            "step {detail}: variable bound twice"
        ))),
        CHECK_STATUS_SUBST_MISMATCH => {
            let rule_name = witness
                .steps
                .get(detail)
                .and_then(|s| rules.rules.get(s.rule))
                .map_or("?", |r| r.name);
            Err(Error::Witness(format!(
                "step {detail}: substitution does not bind exactly the variables of {rule_name}"
            )))
        }
        CHECK_STATUS_UNBOUND_VAR => Err(Error::Witness("variable is unbound".into())),
        CHECK_STATUS_BELOW_LITERAL => Err(Error::Witness("step position below a literal".into())),
        CHECK_STATUS_POS_OOB => Err(Error::Witness("step position out of range".into())),
        CHECK_STATUS_SUBTERM_MISMATCH => {
            if let Some(step) = witness.steps.get(detail)
                && let Some(rule) = rules.rules.get(step.rule)
            {
                Err(Error::Witness(format!(
                    "step {detail}: {} ({:?}) does not match the subterm at {:?}",
                    rule.name, step.direction, step.path
                )))
            } else {
                Err(Error::Witness(format!("step {detail}: subterm mismatch")))
            }
        }
        CHECK_STATUS_GOAL_MISMATCH => Err(Error::Witness(
            "the replayed term is not the extracted term".into(),
        )),
        _ => Err(Error::Witness("unexpected witness checker status".into())),
    }
}
