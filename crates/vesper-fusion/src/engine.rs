//! Bounded egglog equality saturation and extraction.
//!
//! A rule set is plain egglog text: sort and constructor declarations whose
//! `:cost` annotations are the cost model, followed by named rewrites. The
//! input graph is emitted as `let` bindings (so shared subgraphs stay shared),
//! saturated for a bounded number of iterations, and the cheapest
//! representative of the root e-class is extracted with egglog's tree-cost
//! extractor. The extracted term is returned as a hash-consed arena so a
//! shared subterm is converted once.
use crate::witness::{self, RuleSet, Summary, Witness};
use crate::{Error, MAX_EXTRACTED};
use egglog::{CommandOutput, EGraph, Term, TermDag, ast::Literal};
use std::collections::HashMap;
use std::fmt::Write as _;

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
    let mut program = String::with_capacity(rules.len() + bindings.len() + 64);
    program.push_str(rules);
    program.push_str(bindings);
    if saturate {
        program.push_str(&format!("(run {ITERATIONS})\n"));
    }
    program.push_str(&format!("(extract {root})\n"));
    let mut egraph = EGraph::default();
    let outputs = egraph
        .parse_and_run_program(None, &program)
        .map_err(|e| Error::Egglog(e.to_string()))?;
    let Some((dag, cost, term)) = outputs.into_iter().rev().find_map(|o| match o {
        CommandOutput::ExtractBest(dag, cost, term) => Some((dag, cost, term)),
        _ => None,
    }) else {
        return Err(Error::Egglog("no extraction output".into()));
    };
    convert(&dag, cost, term)
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

/// Prefix of the bindings that name the extracted term in the proof query.
const TARGET_PREFIX: &str = "fusionwitness";

/// Saturates `bindings` with `rules`, extracts `root`, asks egglog for a proof
/// that `root` equals the extracted term, converts it into a [`Witness`] and
/// checks the witness against the source term rebuilt from `bindings` and the
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
    // egglog's proof extraction recurses over proof depth, which grows with
    // the graph, so it runs on a thread with a stack sized for MAX_NODES-node
    // chains. egglog reports malformed states by panicking; a panic there is
    // a refusal, not a crash of the caller.
    let (term, witness) = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("vesper-fusion-proof".into())
            .stack_size(PROOF_STACK)
            .spawn_scoped(scope, || {
                run_proved(rules, set, bindings, root, (&source.0, source.1))
            })
            .map_err(|e| Error::Witness(format!("cannot start the proof thread: {e}")))?
            .join()
            .map_err(|_| Error::Witness("egglog panicked while producing the proof".into()))?
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

fn run_proved(
    rules: &str,
    set: &RuleSet,
    bindings: &str,
    root: &str,
    source: (&[Node], usize),
) -> Result<(Extracted, Witness), Error> {
    let mut program = String::with_capacity(rules.len() + bindings.len() + 64);
    program.push_str(rules);
    program.push_str(bindings);
    let _ = writeln!(program, "(run {ITERATIONS})\n(extract {root})");
    let mut egraph = EGraph::new_with_proofs();
    let outputs = egraph
        .parse_and_run_program(None, &program)
        .map_err(|e| Error::Egglog(e.to_string()))?;
    let Some((dag, cost, term)) = outputs.into_iter().rev().find_map(|o| match o {
        CommandOutput::ExtractBest(dag, cost, term) => Some((dag, cost, term)),
        _ => None,
    }) else {
        return Err(Error::Egglog("no extraction output".into()));
    };
    let term = convert(&dag, cost, term)?;
    // Write the extracted term out for the query: a subterm with several
    // parents gets its own binding (so a shared DAG stays linear in size),
    // the rest is inlined (each `let` is a command, and commands are slow in
    // proof mode).
    let mut parents = vec![0usize; term.nodes.len()];
    for node in &term.nodes {
        if let Node::App(_, kids) = node {
            kids.iter().for_each(|&k| parents[k] += 1);
        }
    }
    fn write_term(nodes: &[Node], shared: &[bool], id: usize, top: bool, out: &mut String) {
        match &nodes[id] {
            Node::Int(v) => {
                let _ = write!(out, "{v}");
            }
            Node::Str(s) => {
                let _ = write!(out, "\"{s}\"");
            }
            Node::App(_, _) if shared[id] && !top => {
                let _ = write!(out, "{TARGET_PREFIX}{id}");
            }
            Node::App(head, kids) => {
                let _ = write!(out, "({head}");
                for &k in kids {
                    out.push(' ');
                    write_term(nodes, shared, k, false, out);
                }
                out.push(')');
            }
        }
    }
    let shared: Vec<bool> = parents.iter().map(|&p| p > 1).collect();
    let mut query = String::new();
    for id in (0..term.nodes.len()).filter(|&id| shared[id]) {
        if matches!(term.nodes[id], Node::App(..)) {
            let _ = write!(query, "(let {TARGET_PREFIX}{id} ");
            write_term(&term.nodes, &shared, id, true, &mut query);
            query.push_str(")\n");
        }
    }
    let _ = write!(query, "(prove (= {root} ");
    write_term(&term.nodes, &shared, term.root, false, &mut query);
    query.push_str("))\n");
    let outputs = egraph
        .parse_and_run_program(None, &query)
        .map_err(|e| Error::Witness(format!("egglog could not prove the extraction: {e}")))?;
    let Some((store, proof)) = outputs.into_iter().rev().find_map(|o| match o {
        CommandOutput::ProveExists {
            proof_store,
            proof_id,
        } => Some((proof_store, proof_id)),
        _ => None,
    }) else {
        return Err(Error::Witness("egglog returned no proof".into()));
    };
    let witness = witness::produce(set, &store, proof, source, &term)?;
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

fn convert(dag: &TermDag, cost: u64, term: egglog::TermId) -> Result<Extracted, Error> {
    let mut nodes = Vec::new();
    let mut index: HashMap<Node, usize> = HashMap::new();
    let mut memo: HashMap<egglog::TermId, usize> = HashMap::new();
    // Iterative post-order so deep chains cannot overflow the stack.
    let mut stack = vec![(term, false)];
    while let Some((id, expanded)) = stack.pop() {
        if memo.contains_key(&id) {
            continue;
        }
        let node = match dag.get(id) {
            Term::Lit(Literal::Int(v)) => Node::Int(*v),
            Term::Lit(Literal::String(s)) => Node::Str(s.clone()),
            Term::Lit(other) => {
                return Err(Error::Extraction(format!("unexpected literal {other:?}")));
            }
            Term::Var(v) => return Err(Error::Extraction(format!("unexpected variable {v}"))),
            Term::App(head, kids) => {
                if !expanded {
                    stack.push((id, true));
                    for k in kids.iter().rev() {
                        if !memo.contains_key(k) {
                            stack.push((*k, false));
                        }
                    }
                    continue;
                }
                Node::App(head.clone(), kids.iter().map(|k| memo[k]).collect())
            }
        };
        let slot = match index.get(&node) {
            Some(&slot) => slot,
            None => {
                if nodes.len() >= MAX_EXTRACTED {
                    return Err(Error::Limit("extracted term nodes"));
                }
                nodes.push(node.clone());
                index.insert(node, nodes.len() - 1);
                nodes.len() - 1
            }
        };
        memo.insert(id, slot);
    }
    let root = memo[&term];
    let text = render(&nodes, root);
    Ok(Extracted {
        nodes,
        root,
        cost,
        text,
    })
}
