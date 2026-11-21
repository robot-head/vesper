//! Bounded equality saturation and extraction.
//!
//! A rule set is plain egglog text: sort and constructor declarations whose
//! `:cost` annotations are the cost model, followed by named rewrites. The
//! input graph is emitted as `let` bindings (so shared subgraphs stay shared),
//! saturated for a bounded number of iterations, and the cheapest
//! representative of the root e-class is extracted with egglog's tree-cost
//! extractor. The extracted term is returned as a hash-consed arena so a
//! shared subterm is converted once.
//!
//! The engine is the Vesper package in `vesper/` (Rust-exit wave SH6, lane
//! 18, #1413), which replaced egglog 3: it reads the same program text, runs
//! egglog's `(run n)` and tree-cost extraction, and explains the extraction
//! as a rewrite chain (egg-style explanations over a proof forest) instead of
//! asking egglog for a proof. `tests/egglog_differential.rs` compares its
//! extractions with egglog's.
use crate::witness::{self, Direction, RuleSet, Step, Summary, Witness};
use crate::{Error, MAX_EXTRACTED};

// Generated and committed, outside src/ (see the porting recipe).
#[rustfmt::skip]
#[allow(unsafe_code)]
#[path = "../port/binding.rs"]
mod binding;

/// Saturation iterations; every shipped rule set reaches a fixpoint sooner on
/// the graphs the tests use, and this caps work on adversarial graphs.
pub const ITERATIONS: usize = 24;

/// One node of an extracted term.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    /// Integer literal.
    Int(i64),
    /// String literal.
    Str(String),
    /// Constructor application; children index the arena.
    App(String, Vec<usize>),
}

/// An extracted term: `nodes[root]` is the root, children precede parents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extracted {
    /// Hash-consed nodes.
    pub nodes: Vec<Node>,
    /// Index of the root node.
    pub root: usize,
    /// egglog's tree cost of the extracted term.
    pub cost: u64,
    /// The extracted term printed as an s-expression (bounded).
    pub text: String,
}

impl Extracted {
    /// Constructor name and children of `id`, or an extraction error.
    pub fn app(&self, id: usize) -> Result<(&str, &[usize]), Error> {
        match &self.nodes[id] {
            Node::App(name, kids) => Ok((name.as_str(), kids.as_slice())),
            other => Err(Error::Extraction(format!(
                "expected a constructor, got {other:?}"
            ))),
        }
    }
    /// Integer literal at `id`.
    pub fn int(&self, id: usize) -> Result<i64, Error> {
        match &self.nodes[id] {
            Node::Int(v) => Ok(*v),
            other => Err(Error::Extraction(format!(
                "expected an integer, got {other:?}"
            ))),
        }
    }
    /// String literal at `id`.
    pub fn str(&self, id: usize) -> Result<&str, Error> {
        match &self.nodes[id] {
            Node::Str(v) => Ok(v),
            other => Err(Error::Extraction(format!(
                "expected a string, got {other:?}"
            ))),
        }
    }
    /// Number of constructor applications named `name`.
    pub fn count(&self, name: &str) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n, Node::App(head, _) if head == name))
            .count()
    }
}

/// Runs `rules` (declarations + rewrites) over `bindings` (egglog `let`
/// commands) and extracts the binding named `root`. With `saturate` false
/// the rewrites are declared but never run, which extracts the input graph
/// itself (used for the unfused cost).
pub fn run(rules: &str, bindings: &str, root: &str, saturate: bool) -> Result<Extracted, Error> {
    let program = format!("{rules}{bindings}");
    let mode = if saturate { MODE_SATURATE } else { 0 };
    let out = Words::run(&program, root, mode)?;
    let mut r = out.reader(&program);
    r.extracted()
}

/// Mode bits of the Vesper entry point.
const MODE_SATURATE: u64 = 1;
const MODE_EXPLAIN: u64 = 2;

/// Words returned by the Vesper engine (see `vesper/src/lib.ves`).
struct Words(Vec<u64>);

impl Words {
    fn run(program: &str, root: &str, mode: u64) -> Result<Self, Error> {
        let words = binding::fusion_run(program, root, mode | ((ITERATIONS as u64) << 8));
        let status = words.first().copied().unwrap_or(u64::MAX);
        let at = words.get(1).copied().unwrap_or(0);
        let what = match status {
            0 => return Ok(Self(words)),
            10 => return Err(Error::Limit("e-graph nodes")),
            11 => return Err(Error::Limit("witness steps")),
            1 => "malformed s-expression",
            2 => "unknown or malformed command",
            3 => "unknown or duplicate sort",
            4 => "unknown constructor",
            5 => "wrong number of arguments",
            6 => "type mismatch",
            7 => "unbound name",
            8 => "duplicate name",
            9 => "no binding named the root",
            13 => "the root has no finite-cost term",
            14 => "integer out of range",
            15 => "right-side variable not bound by the left side",
            16 => "a rewrite's left side is not a constructor application",
            _ => "unexpected engine status",
        };
        Err(Error::Egglog(format!("{what} at byte {at}")))
    }

    fn reader<'a>(&'a self, program: &'a str) -> Reader<'a> {
        Reader {
            words: &self.0,
            at: 4,
            program,
        }
    }
}

struct Reader<'a> {
    words: &'a [u64],
    at: usize,
    program: &'a str,
}

impl Reader<'_> {
    fn word(&mut self) -> Result<u64, Error> {
        let w = *self
            .words
            .get(self.at)
            .ok_or_else(|| Error::Extraction("engine output truncated".into()))?;
        self.at += 1;
        Ok(w)
    }

    fn index(&mut self) -> Result<usize, Error> {
        usize::try_from(self.word()?).map_err(|_| Error::Extraction("engine index".into()))
    }

    fn text(&mut self) -> Result<String, Error> {
        let start = self.index()?;
        let len = self.index()?;
        self.program
            .get(start..start.saturating_add(len))
            .map(str::to_owned)
            .ok_or_else(|| Error::Extraction("engine text range".into()))
    }

    /// An arena of `count` nodes whose children precede their parents.
    fn arena(&mut self, limit: usize, what: &'static str) -> Result<Vec<Node>, Error> {
        let count = self.index()?;
        if count > limit {
            return Err(Error::Limit(what));
        }
        let mut nodes = Vec::with_capacity(count);
        for i in 0..count {
            let kind = self.word()?;
            let node = match kind {
                0 => {
                    let v = self.word()? as i64;
                    self.word()?;
                    Node::Int(v)
                }
                1 => Node::Str(self.text()?),
                2 => Node::App(self.text()?, Vec::new()),
                _ => return Err(Error::Extraction("engine node kind".into())),
            };
            let kids = self.index()?;
            let node = match node {
                Node::App(head, _) => {
                    let mut ks = Vec::with_capacity(kids);
                    for _ in 0..kids {
                        let k = self.index()?;
                        if k >= i {
                            return Err(Error::Extraction("engine arena order".into()));
                        }
                        ks.push(k);
                    }
                    Node::App(head, ks)
                }
                other if kids == 0 => other,
                _ => return Err(Error::Extraction("literal with children".into())),
            };
            nodes.push(node);
        }
        Ok(nodes)
    }

    fn extracted(&mut self) -> Result<Extracted, Error> {
        let cost = self.word()?;
        let nodes = self.arena(MAX_EXTRACTED, "extracted term nodes")?;
        let root = self.index()?;
        if root >= nodes.len() {
            return Err(Error::Extraction("engine root slot".into()));
        }
        let text = render(&nodes, root);
        Ok(Extracted {
            nodes,
            root,
            cost,
            text,
        })
    }

    /// The witness: terms, the source slot (unused: the checker rebuilds
    /// the source from the bindings), and the steps, with each rule mapped
    /// from the program's rewrite order to `set`.
    fn witness(
        &mut self,
        rewrites: &[(witness::Pat, witness::Pat)],
        set: &RuleSet,
    ) -> Result<Witness, Error> {
        let terms = self.arena(witness::MAX_TERMS, "witness terms")?;
        self.index()?;
        let count = self.index()?;
        if count > witness::MAX_STEPS {
            return Err(Error::Limit("witness steps"));
        }
        let mut steps = Vec::with_capacity(count);
        for _ in 0..count {
            let k = self.index()?;
            let (lhs, rhs) = rewrites
                .get(k)
                .ok_or_else(|| Error::Witness("engine rule index".into()))?;
            let rule = set.find(lhs, rhs).ok_or_else(|| {
                Error::Witness(format!(
                    "equality justified by unknown rule {k} of the program"
                ))
            })?;
            let direction = match self.word()? {
                0 => Direction::Forward,
                1 => Direction::Backward,
                _ => return Err(Error::Witness("engine step direction".into())),
            };
            let depth = self.index()?;
            if depth > witness::MAX_DEPTH {
                return Err(Error::Limit("witness step depth"));
            }
            let mut path = Vec::with_capacity(depth);
            for _ in 0..depth {
                path.push(
                    u32::try_from(self.word()?)
                        .map_err(|_| Error::Witness("engine path".into()))?,
                );
            }
            let nv = self.index()?;
            let mut substitution = Vec::with_capacity(nv);
            for _ in 0..nv {
                let var = self.text()?;
                let term = self.index()?;
                substitution.push((var, term));
            }
            substitution.sort();
            steps.push(Step {
                rule,
                direction,
                path,
                substitution,
            });
        }
        Ok(Witness { terms, steps })
    }
}

/// A checked fusion: the unfused and fused extractions, the rewrite witness
/// that connects them, and what checking it established.
#[derive(Clone, Debug)]
pub struct Fusion {
    /// The input graph extracted without saturation (its cost is the unfused
    /// cost).
    pub before: Extracted,
    /// The cheapest term after saturation.
    pub term: Extracted,
    /// The source term rebuilt from the bindings (not by egglog).
    pub source: (Vec<Node>, usize),
    /// The rewrite chain from `source` to `term`.
    pub witness: Witness,
    /// The result of [`witness::check`].
    pub summary: Summary,
}

/// Stack for the proof thread (virtual; pages are touched only as used).
const PROOF_STACK: usize = 1 << 30;

/// Prefix reserved for binding identifiers (egglog's proof query named the
/// extracted term's shared subterms with it; the reservation is kept).
const TARGET_PREFIX: &str = "fusionwitness";

/// Saturates `bindings` with `rules`, extracts `root`, explains why `root`
/// equals the extracted term as a [`Witness`] and checks the witness against the source term rebuilt from `bindings` and the
/// extracted term. Any failure refuses the fusion.
pub fn fuse(rules: &str, set: &RuleSet, bindings: &str, root: &str) -> Result<Fusion, Error> {
    // Only binding identifiers can collide with the query's bindings; input
    // and matrix names appear as quoted strings and may contain anything.
    let collides = bindings.lines().any(|line| {
        line.trim_start()
            .strip_prefix("(let ")
            .is_some_and(|rest| rest.trim_start().starts_with(TARGET_PREFIX))
    });
    if collides {
        return Err(Error::Witness(
            "binding names collide with the query".into(),
        ));
    }
    let before = run(rules, bindings, root, false)?;
    let source = witness::source_term(bindings, root)?;
    // The explanation recurses over proof-forest paths, whose depth grows
    // with the graph, so it runs on a thread with a stack sized for
    // MAX_NODES-node chains.
    let (term, witness) = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("vesper-fusion-proof".into())
            .stack_size(PROOF_STACK)
            .spawn_scoped(scope, || explained(rules, set, bindings, root))
            .map_err(|e| Error::Witness(format!("cannot start the proof thread: {e}")))?
            .join()
            .map_err(|_| Error::Witness("the fusion engine panicked".into()))?
    })?;
    let summary = witness::check(
        set,
        (&source.0, source.1),
        (&term.nodes, term.root),
        &witness,
    )?;
    Ok(Fusion {
        before,
        term,
        source,
        witness,
        summary,
    })
}

/// Saturates `bindings` with `rules`, extracts `root` and explains the
/// extraction as a rewrite chain from the bindings' term (unchecked; [`fuse`]
/// checks it).
pub fn explained(
    rules: &str,
    set: &RuleSet,
    bindings: &str,
    root: &str,
) -> Result<(Extracted, Witness), Error> {
    let program = format!("{rules}{bindings}");
    let rewrites = witness::program_rewrites(rules)?;
    let out = Words::run(&program, root, MODE_SATURATE | MODE_EXPLAIN)?;
    let mut r = out.reader(&program);
    let term = r.extracted()?;
    let witness = r.witness(&rewrites, set)?;
    Ok((term, witness))
}

/// Prints `root` as an s-expression, eliding past a fixed budget (a shared
/// DAG printed as a tree can be exponentially larger than the arena).
fn render(nodes: &[Node], root: usize) -> String {
    const BUDGET: usize = 4096;
    fn go(nodes: &[Node], id: usize, out: &mut String) {
        if out.len() > BUDGET {
            return;
        }
        match &nodes[id] {
            Node::Int(v) => out.push_str(&v.to_string()),
            Node::Str(s) => {
                out.push('"');
                out.push_str(s);
                out.push('"');
            }
            Node::App(head, kids) => {
                out.push('(');
                out.push_str(head);
                for k in kids {
                    out.push(' ');
                    go(nodes, *k, out);
                }
                out.push(')');
            }
        }
    }
    let mut out = String::new();
    go(nodes, root, &mut out);
    if out.len() > BUDGET {
        out.push_str(" ...");
    }
    out
}
