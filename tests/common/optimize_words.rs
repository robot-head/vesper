//! The input words of `@vesper/optimize` (packages/vesper-optimize,
//! self-hosting SH3 lane 10, #1249) and readers of its answers, shared by
//! tests/self_host_optimize*.rs and the hybrid build's `optimize` module
//! (crates/vesper-hybrid).
//!
//! A batch is a case count, then each case: the limits and three spent
//! counters, the names, the type map entries, the expressions (children
//! first) and what the task reads after them (`read_*` in input.ves). The
//! package's `Str` entries take names inline in the words. The words are
//! not UTF-8, so the hybrid passes a *split* input instead: the names'
//! lengths in the words and their bytes, one after another, in a separate
//! text (`read_names_text`).
#![allow(dead_code)]

use std::collections::BTreeMap;
use vesper::ast::{Expr, ExprKind, Literal, Pass, Type, TypeMap};
use vesper::numeric::IntegerKind;
use vesper::optimize::witness::{
    Equality, IntOp, Reason, RewriteCertificate, Spent, Substitution, Term, TermNode, WitnessError,
    WitnessLimits,
};

/// The spans of every node of `e`.
pub fn spans(e: &Expr, out: &mut Vec<(usize, usize)>) {
    out.push((e.span.start, e.span.end));
    match &e.kind {
        ExprKind::Binary(_, a, b) => {
            spans(a, out);
            spans(b, out);
        }
        ExprKind::Call { callee, args, .. } => {
            spans(callee, out);
            for a in args {
                spans(&a.expr, out);
            }
        }
        _ => {}
    }
}

/// The entries of `types` the package can look up for `exprs`.
pub fn relevant_types(exprs: &[&Expr], types: &TypeMap) -> TypeMap {
    let mut s = vec![];
    for e in exprs {
        spans(e, &mut s);
    }
    s.into_iter()
        .filter_map(|k| types.get(&k).map(|t| (k, t.clone())))
        .collect()
}

#[derive(Default)]
struct Words {
    names: Vec<String>,
    index: BTreeMap<String, u64>,
    exprs: Vec<u64>,
    count: u64,
}

pub fn pass_word(p: Pass) -> u64 {
    match p {
        Pass::In => 0,
        Pass::Inout => 1,
        Pass::Sink => 2,
    }
}

impl Words {
    fn name(&mut self, s: &str) -> u64 {
        if let Some(i) = self.index.get(s) {
            return *i;
        }
        let i = self.names.len() as u64;
        self.names.push(s.to_owned());
        self.index.insert(s.to_owned(), i);
        i
    }

    /// Appends `e` children first; returns its input index (the input
    /// format of `read_exprs` in input.ves).
    fn expr(&mut self, e: &Expr) -> u64 {
        let mut w = vec![];
        let kind = match &e.kind {
            ExprKind::Literal(l) => {
                match l {
                    // Kind, sign, then the magnitude's low and high words.
                    Literal::Int(v) => {
                        let m = v.unsigned_abs();
                        w.extend([1, (*v < 0) as u64, m as u64, (m >> 64) as u64]);
                    }
                    Literal::Float(_) => w.extend([2, 0, 0, 0]),
                    Literal::Bool(_) => w.extend([3, 0, 0, 0]),
                    Literal::String(_) => w.extend([4, 0, 0, 0]),
                    Literal::WideInt(_) => w.extend([5, 0, 0, 0]),
                }
                1
            }
            ExprKind::Name(n) => {
                w.extend([self.name(n), n.capacity() as u64]);
                2
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let c = self.expr(callee);
                let mut a = vec![];
                for arg in args {
                    let x = self.expr(&arg.expr);
                    a.extend([pass_word(arg.pass), arg.explicit as u64, x]);
                }
                w.extend([
                    c,
                    types.len() as u64,
                    types.capacity() as u64,
                    args.len() as u64,
                    args.capacity() as u64,
                ]);
                w.extend(a);
                6
            }
            ExprKind::Binary(op, a, b) => {
                let x = self.expr(a);
                let y = self.expr(b);
                w.extend([self.name(op), op.capacity() as u64, x, y]);
                11
            }
            k => kind_number(k),
        };
        self.exprs
            .extend([kind, e.span.start as u64, e.span.end as u64]);
        self.exprs.extend(w);
        self.count += 1;
        self.count - 1
    }
}

pub fn kind_number(k: &ExprKind) -> u64 {
    match k {
        ExprKind::Literal(_) => 1,
        ExprKind::Name(_) => 2,
        ExprKind::Tuple(_) => 3,
        ExprKind::Struct { .. } => 4,
        ExprKind::Block { .. } => 5,
        ExprKind::Call { .. } => 6,
        ExprKind::Field(..) => 7,
        ExprKind::Index(..) => 8,
        ExprKind::Try(_) => 9,
        ExprKind::Unary(..) => 10,
        ExprKind::Binary(..) => 11,
        ExprKind::Assign(..) => 12,
        ExprKind::If { .. } => 13,
        ExprKind::Match { .. } => 14,
        ExprKind::Closure { .. } => 15,
        ExprKind::Handle { .. } => 16,
        ExprKind::Loop(_) => 17,
        ExprKind::For { .. } => 18,
        ExprKind::Range { .. } => 19,
        ExprKind::Break(_) => 20,
        ExprKind::Return(_) => 21,
        ExprKind::Continue => 22,
    }
}

pub fn type_code(t: &Type) -> u64 {
    match t {
        Type::Named(n, args) if args.is_empty() => match n.as_str() {
            "u8" => 1,
            "u32" => 2,
            "u64" => 3,
            "i64" => 4,
            _ => 6,
        },
        Type::IntLiteral(_) => 5,
        _ => 6,
    }
}

pub fn op_word(op: IntOp) -> u64 {
    match op {
        IntOp::Add => 0,
        IntOp::Sub => 1,
        IntOp::Mul => 2,
    }
}

pub fn term_words(t: &Term, out: &mut Vec<u64>) {
    out.push(t.kind as u64);
    match t.node {
        TermNode::Const(v) => out.extend([0, v as u64, ((v as u128) >> 64) as u64, 0]),
        TermNode::InputVar(v) => out.extend([1, v as u64, 0, 0]),
        TermNode::Checked { op, children } => {
            out.extend([2, op_word(op), children[0] as u64, children[1] as u64])
        }
        TermNode::Wrapping { op, children } => {
            out.extend([3, op_word(op), children[0] as u64, children[1] as u64])
        }
    }
}

pub fn proof_words(p: &Equality, out: &mut Vec<u64>) {
    out.extend([p.from as u64, p.to as u64]);
    match p.reason {
        Reason::Refl => out.extend([0, 0, 0, 0, 0, 0, 0]),
        Reason::Rule {
            rule,
            substitution,
            guard,
        } => out.extend([
            1,
            rule as u64,
            substitution.bindings[0] as u64,
            substitution.bindings[1] as u64,
            substitution.bindings[2] as u64,
            substitution.len as u64,
            guard.map_or(0, |g| g as u64 + 1),
        ]),
        Reason::Congruence { children } => {
            out.extend([2, children[0] as u64, children[1] as u64, 0, 0, 0, 0])
        }
        Reason::Symmetry { proof } => out.extend([3, proof as u64, 0, 0, 0, 0, 0]),
        Reason::Transitivity { first, second } => {
            out.extend([4, first as u64, second as u64, 0, 0, 0, 0])
        }
    }
}

pub fn put(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

/// A batch being written: the words, and the names' text of a split input.
pub struct Batch {
    pub bytes: Vec<u8>,
    pub names: String,
    split: bool,
}

impl Batch {
    /// A batch of `cases` cases; `split` keeps the names out of the words.
    pub fn new(cases: usize, split: bool) -> Batch {
        let mut bytes = vec![];
        put(&mut bytes, cases as u64);
        Batch {
            bytes,
            names: String::new(),
            split,
        }
    }

    fn head(&mut self, limits: WitnessLimits, spent: Spent) {
        let l = limits;
        for x in [
            l.max_terms,
            l.max_proofs,
            l.max_input_nodes,
            l.max_output_nodes,
            l.max_depth,
            l.max_variables,
            l.max_name_bytes,
            l.max_metadata_bytes,
            l.max_work,
            spent.work,
            spent.metadata,
            spent.names,
        ] {
            put(&mut self.bytes, x as u64);
        }
    }

    fn names_and_types(&mut self, w: &Words, types: &TypeMap) -> Vec<u64> {
        let out = &mut self.bytes;
        put(out, w.names.len() as u64);
        for n in &w.names {
            put(out, n.len() as u64);
            if self.split {
                self.names.push_str(n);
            } else {
                out.extend(n.as_bytes());
                while !out.len().is_multiple_of(8) {
                    out.push(0);
                }
            }
        }
        let mut words = vec![types.len() as u64];
        for ((s, e), t) in types {
            words.extend([*s as u64, *e as u64, type_code(t)]);
        }
        words.push(w.count);
        words.extend(&w.exprs);
        words
    }

    /// One `optimize_propose` case: `propose_rewrite` of `source` on a fresh
    /// budget of `limits`.
    pub fn propose(&mut self, source: &Expr, types: &TypeMap, limits: WitnessLimits) {
        self.head(limits, Spent::default());
        let mut w = Words::default();
        let source = w.expr(source);
        let mut words = self.names_and_types(&w, types);
        words.push(source);
        for x in words {
            put(&mut self.bytes, x);
        }
    }

    /// One `optimize_verify` case: `verify_spent` of the certificate (its
    /// vectors' capacities given, since a clone's may differ), and with
    /// `build` also `build_candidate` of its output.
    #[allow(clippy::too_many_arguments)]
    pub fn verify(
        &mut self,
        source: &Expr,
        candidate: &Expr,
        types: &TypeMap,
        cert: &RewriteCertificate,
        caps: (usize, usize),
        limits: WitnessLimits,
        spent: Spent,
        build: bool,
    ) {
        self.head(limits, spent);
        let mut w = Words::default();
        let source = w.expr(source);
        let candidate = w.expr(candidate);
        let mut words = self.names_and_types(&w, types);
        words.extend([source, candidate]);
        words.extend([
            cert.schema_version as u64,
            cert.arithmetic_version as u64,
            caps.0 as u64,
            caps.1 as u64,
            cert.input as u64,
            cert.output as u64,
            cert.conclusion as u64,
            cert.terms.len() as u64,
        ]);
        for t in &cert.terms {
            term_words(t, &mut words);
        }
        words.push(cert.proofs.len() as u64);
        for p in &cert.proofs {
            proof_words(p, &mut words);
        }
        words.push(build as u64);
        for x in words {
            put(&mut self.bytes, x);
        }
    }
}

/// The text `pack_text` (lib.ves) packed: a byte count, then the bytes
/// eight per word, little-endian.
pub fn unpack_text(words: &[u64]) -> String {
    let n = words.first().copied().unwrap_or(0) as usize;
    let mut bytes = Vec::with_capacity(n);
    for w in &words[1..] {
        bytes.extend(w.to_le_bytes());
    }
    bytes.truncate(n);
    String::from_utf8(bytes).expect("the package writes ASCII")
}

/// The `WitnessError` of a nonzero result code (its position plus one).
pub fn error_of(code: u64) -> WitnessError {
    use WitnessError::*;
    const ALL: [WitnessError; 21] = [
        InvalidLimits,
        Limit,
        Allocation,
        UnsupportedAst,
        MissingType,
        TypeMismatch,
        ConstantOutOfRange,
        UnknownVariable,
        InvalidIntrinsicOrigin,
        UnsupportedVersion,
        InvalidTermIndex,
        NonTopologicalTerm,
        InvalidProofIndex,
        NonTopologicalProof,
        UnknownRule,
        InvalidSubstitution,
        InvalidRuleInstance,
        UnprovenTotality,
        InvalidEquality,
        InputMismatch,
        OutputMismatch,
    ];
    ALL[code as usize - 1]
}

/// An `R code work metadata names` line: the result and the spent counters.
pub fn parse_verify_line(line: &str) -> (Result<(), WitnessError>, Spent) {
    let f: Vec<u64> = line[2..].split(' ').map(|x| x.parse().unwrap()).collect();
    let result = if f[0] == 0 {
        Ok(())
    } else {
        Err(error_of(f[0]))
    };
    let spent = Spent {
        work: f[1] as usize,
        metadata: f[2] as usize,
        names: f[3] as usize,
    };
    (result, spent)
}

/// A candidate proposal's certificate, spent counters and the certificate
/// vectors' capacities, from its `P C`, `C`, `T` and `Q` lines.
pub fn parse_certificate(text: &str) -> (RewriteCertificate, Spent, (usize, usize)) {
    let mut lines = text.lines();
    let p: Vec<u64> = lines.next().unwrap()[4..]
        .split(' ')
        .map(|x| x.parse().unwrap())
        .collect();
    let _candidate = lines.next().unwrap();
    let terms_line = lines.next().unwrap();
    let proofs_line = lines.next().unwrap();
    let kind_of = |k: u64| match k {
        0 => IntegerKind::U8,
        1 => IntegerKind::U32,
        2 => IntegerKind::U64,
        _ => IntegerKind::I64,
    };
    let op_of = |k: u64| match k {
        0 => IntOp::Add,
        1 => IntOp::Sub,
        _ => IntOp::Mul,
    };
    let mut terms = vec![];
    for t in terms_line[1..].split_whitespace() {
        let f: Vec<&str> = t.split(':').collect();
        let n = |i: usize| f[i].parse::<u64>().unwrap();
        let node = match n(1) {
            0 => TermNode::Const(f[3].parse::<i128>().unwrap()),
            1 => TermNode::InputVar(n(4) as u32),
            2 => TermNode::Checked {
                op: op_of(n(2)),
                children: [n(4) as u32, n(5) as u32],
            },
            _ => TermNode::Wrapping {
                op: op_of(n(2)),
                children: [n(4) as u32, n(5) as u32],
            },
        };
        terms.push(Term {
            kind: kind_of(n(0)),
            node,
        });
    }
    let mut proofs = vec![];
    for q in proofs_line[1..].split_whitespace() {
        let f: Vec<u64> = q.split(':').map(|x| x.parse().unwrap()).collect();
        let reason = match f[2] {
            0 => Reason::Refl,
            1 => Reason::Rule {
                rule: f[3] as u16,
                substitution: Substitution {
                    bindings: [f[4] as u32, f[5] as u32, f[6] as u32],
                    len: f[7] as u8,
                },
                guard: (f[8] != 0).then(|| (f[8] - 1) as u32),
            },
            2 => Reason::Congruence {
                children: [f[9] as u32, f[10] as u32],
            },
            3 => Reason::Symmetry { proof: f[9] as u32 },
            _ => Reason::Transitivity {
                first: f[9] as u32,
                second: f[10] as u32,
            },
        };
        proofs.push(Equality {
            from: f[0] as u32,
            to: f[1] as u32,
            reason,
        });
    }
    let cert = RewriteCertificate {
        schema_version: 1,
        arithmetic_version: 1,
        terms,
        proofs,
        input: p[5] as u32,
        output: p[6] as u32,
        conclusion: p[7] as u32,
    };
    let spent = Spent {
        work: p[0] as usize,
        metadata: p[1] as usize,
        names: p[2] as usize,
    };
    (cert, spent, (p[3] as usize, p[4] as usize))
}
