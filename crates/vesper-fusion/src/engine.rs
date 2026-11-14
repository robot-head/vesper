//! Bounded egglog equality saturation and extraction.
//!
//! A rule set is plain egglog text: sort and constructor declarations whose
//! `:cost` annotations are the cost model, followed by named rewrites. The
//! input graph is emitted as `let` bindings (so shared subgraphs stay shared),
//! saturated for a bounded number of iterations, and the cheapest
//! representative of the root e-class is extracted with egglog's tree-cost
//! extractor. The extracted term is returned as a hash-consed arena so a
//! shared subterm is converted once.
use crate::{Error, MAX_EXTRACTED};
use egglog::{CommandOutput, EGraph, Term, TermDag, ast::Literal};
use std::collections::HashMap;

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
