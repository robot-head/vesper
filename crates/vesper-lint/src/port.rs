//! The port build's side of the boundary (`cfg(vesper_port)`): the encoding
//! of a [`CheckedProgram`] for the Vesper lint engine (`vesper/src/lib.ves`)
//! and the forwarding of [`crate::analyze`], [`crate::apply_edits`] and
//! [`crate::policy::Policy::parse`].
//!
//! Every decision is made in Vesper. This side writes the facts as a word
//! stream and one text blob, and turns the words that come back into the
//! Rust report: a finding names its effect by index into the function's
//! declared row and its paths by index into the function's actions, so the
//! values it carries are clones of the input's own.

use crate::policy::{ErrorKind, LintName, Policy, PolicyError, PolicyUsage, Severity};
pub use crate::port_binding::*;
use crate::{
    Action, CheckedProgram, Finding, FindingKind, Limits, LintError, LintReport, Skipped,
    SourceEdit,
};
use vesper_core::ast::{Effect, Effects, Mode, Param, Pass, Type};

/// `LintFail` of the port: the index of the error in the tables below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LintFail {
    pub code: u64,
}

/// [`LintError`] messages by `LintFail::code` (analysis and edits).
const LINT_ERRORS: [&str; 19] = [
    "invalid lint limits",
    "function limit",
    "action count overflow",
    "fact limit",
    "signature size limit",
    "struct field limit",
    "substitution limit",
    "projection depth limit",
    "type complexity limit",
    "type arity limit",
    "arrow shape limit",
    "nested effect arity limit",
    "row shape limit",
    "effect arity limit",
    "lint work limit",
    "source/edit byte limit",
    "overlapping or stale source edit",
    "edit size overflow",
    "candidate byte limit",
];

/// [`Skipped::reason`] texts by code (below 16).
const SKIP_REASONS: [&str; 8] = [
    "unresolved direct callee",
    "open or polymorphic effect row",
    "higher-order or unresolved signature",
    "unresolved effect instance",
    "recursive or deep call graph",
    "unresolved direct call",
    "unresolved generic call substitution",
    "effect instance could not be mapped to caller capability",
];

/// A skip reason at or above this is the text of action `reason - OPAQUE`
/// of the program, counting every function's actions in name order (an
/// [`Action::Opaque`]).
const OPAQUE: u64 = 16;

/// [`PolicyError`]s by `LintFail::code` (`Policy::parse`).
const POLICY_ERRORS: [(ErrorKind, &str); 38] = [
    (ErrorKind::Limit, "invalid policy limits"),
    (ErrorKind::Limit, "policy work overflow"),
    (ErrorKind::Limit, "policy work limit"),
    (ErrorKind::Limit, "policy byte overflow"),
    (ErrorKind::Limit, "policy decoded byte limit"),
    (ErrorKind::Limit, "policy value depth/node limit"),
    (ErrorKind::Limit, "policy tree overflow"),
    (ErrorKind::Limit, "policy node limit"),
    (ErrorKind::Limit, "TOML key byte limit"),
    (ErrorKind::Limit, "policy input byte limit"),
    (ErrorKind::Limit, "policy token limit"),
    (ErrorKind::Limit, "policy node estimate overflow"),
    (ErrorKind::Limit, "policy lexical tree estimate limit"),
    (ErrorKind::Syntax, "invalid string escape"),
    (ErrorKind::Syntax, "invalid multiline quote run"),
    (ErrorKind::Syntax, "unterminated TOML string"),
    (ErrorKind::Limit, "policy preparse tree depth limit"),
    (ErrorKind::Syntax, "malformed array-of-table header"),
    (ErrorKind::Syntax, "malformed TOML table header"),
    (ErrorKind::Syntax, "unterminated TOML table header"),
    (ErrorKind::Syntax, "malformed TOML key"),
    (ErrorKind::Syntax, "mismatched TOML container"),
    (ErrorKind::Syntax, "comma outside TOML container"),
    (ErrorKind::Syntax, "unclosed TOML container"),
    (ErrorKind::Syntax, "invalid TOML syntax or duplicate key"),
    (ErrorKind::Shape, "Loom document must be a table"),
    (ErrorKind::Shape, "lints must be a table"),
    (ErrorKind::Shape, "lints.vesper must be a table"),
    (ErrorKind::Limit, "policy rule count limit"),
    (ErrorKind::Severity, "lint severity must be a string"),
    (
        ErrorKind::Severity,
        "severity must be exactly allow, warn, or deny",
    ),
    (ErrorKind::Limit, "policy key byte limit"),
    (ErrorKind::UnknownRule, "unknown Vesper lint name"),
    (ErrorKind::DuplicateRule, "duplicate lint policy rule"),
    (
        ErrorKind::UnavailableDeny,
        "requested deny lint is not implemented",
    ),
    (ErrorKind::Shape, "lints.plugins must be a table"),
    (
        ErrorKind::Plugins,
        "lint plugin packs are unsupported; no pack was loaded",
    ),
    (ErrorKind::UnknownSection, "unknown table under lints"),
];

/// `Policy::parse`'s code for a document the Rust code reads itself (a
/// table whose first key is serde's private datetime marker).
const POLICY_UNSUPPORTED: u64 = 38;

const LINTS: [LintName; 7] = [
    LintName::OverbroadInoutProjection,
    LintName::UnusedEffectCapability,
    LintName::DemoteAllocToRegion,
    LintName::OverlyRestrictiveMode,
    LintName::MissedFipOpportunity,
    LintName::EarlyCancelCheckpoint,
    LintName::UnredactedPiiBoundary,
];

fn lint_error(e: LintFail) -> LintError {
    LintError(
        LINT_ERRORS
            .get(e.code as usize)
            .copied()
            .unwrap_or("lint port error"),
    )
}

/// The facts as the port reads them (`decode` in lib.ves).
#[derive(Default)]
pub(crate) struct Encoded {
    pub words: Vec<u64>,
    pub blob: String,
    /// The nesting of the type being written.
    depth: usize,
    /// Whether a type nests deeper than [`MAX_ENCODED_DEPTH`]; its subtree
    /// is not written.
    pub too_deep: bool,
}

/// Types nested deeper than this are not encoded. The Rust checks refuse
/// any type deeper than 32 (`validate_type`), and both the encoder and the
/// port's decoder recurse, so such programs go to the Rust code instead of
/// the port (the error is the same, and the stack stays bounded).
const MAX_ENCODED_DEPTH: usize = 40;

impl Encoded {
    fn name(&mut self, s: &str) {
        self.words.push(self.blob.len() as u64);
        self.words.push(s.len() as u64);
        self.blob.push_str(s);
    }
    fn opt_name(&mut self, s: Option<&String>) {
        match s {
            Some(s) => {
                self.words.push(1);
                self.name(s);
            }
            None => self.words.push(0),
        }
    }
    fn pass(&mut self, p: Pass) {
        self.words.push(match p {
            Pass::In => 0,
            Pass::Inout => 1,
            Pass::Sink => 2,
        });
    }
    fn ty(&mut self, t: &Type) {
        if self.depth > MAX_ENCODED_DEPTH {
            self.too_deep = true;
            self.words.push(5);
            return;
        }
        self.depth += 1;
        self.ty_node(t);
        self.depth -= 1;
    }
    fn ty_node(&mut self, t: &Type) {
        match t {
            Type::IntLiteral(n) => {
                self.words.push(0);
                self.words.push(*n as u128 as u64);
                self.words.push((*n as u128 >> 64) as u64);
            }
            Type::Named(name, args) => {
                self.words.push(1);
                self.name(name);
                self.types(args);
            }
            Type::Tuple(args) => {
                self.words.push(2);
                self.types(args);
            }
            Type::Function {
                params,
                result,
                effects,
                mode,
            } => {
                self.words.push(3);
                self.words.push(params.len() as u64);
                for (pass, t) in params {
                    self.pass(*pass);
                    self.ty(t);
                }
                self.ty(result);
                self.effects(effects);
                self.words.push(mode_word(*mode));
            }
            Type::Row(effects) => {
                self.words.push(4);
                self.effects(effects);
            }
            Type::Infer => self.words.push(5),
        }
    }
    fn types(&mut self, ts: &[Type]) {
        self.words.push(ts.len() as u64);
        for t in ts {
            self.ty(t);
        }
    }
    fn effect(&mut self, e: &Effect) {
        self.opt_name(e.label.as_ref());
        self.name(&e.name);
        self.types(&e.args);
    }
    fn effects(&mut self, e: &Effects) {
        self.words.push(e.instances.len() as u64);
        for i in &e.instances {
            self.effect(i);
        }
        self.opt_name(e.tail.as_ref());
    }
    fn param(&mut self, p: &Param) {
        self.name(&p.name);
        self.pass(p.pass);
        self.words.push(p.projections.len() as u64);
        for projection in &p.projections {
            self.words.push(projection.len() as u64);
            for n in projection {
                self.name(n);
            }
        }
        self.ty(&p.ty);
        self.words.push(p.span.start as u64);
        self.words.push(p.span.end as u64);
    }
}

fn mode_word(m: Mode) -> u64 {
    match m {
        Mode::Copy => 0,
        Mode::Affine => 1,
        Mode::Linear => 2,
    }
}

/// The word stream and blob of `program`, functions in name order.
pub(crate) fn encode(program: &CheckedProgram) -> Encoded {
    let mut e = Encoded::default();
    e.words.push(program.functions.len() as u64);
    for (name, f) in &program.functions {
        let d = &f.declaration;
        e.name(name);
        e.words.push(f.source_owned as u64);
        e.words.push(d.body.is_some() as u64);
        e.words.push(d.span.start as u64);
        e.words.push(d.span.end as u64);
        e.words.push(d.generics.len() as u64);
        for g in &d.generics {
            e.name(&g.name);
            e.words.push(mode_word(g.bound));
            e.words.push(g.row as u64);
        }
        e.words.push(d.params.len() as u64);
        for p in &d.params {
            e.param(p);
        }
        e.ty(&d.result);
        e.effects(&d.effects);
        e.words.push(f.actions.len() as u64);
        for a in &f.actions {
            match a {
                Action::Perform(effect) => {
                    e.words.push(0);
                    e.effect(effect);
                }
                Action::Call {
                    callee,
                    substitutions,
                } => {
                    e.words.push(1);
                    e.name(callee);
                    e.words.push(substitutions.len() as u64);
                    for (n, t) in substitutions {
                        e.name(n);
                        e.ty(t);
                    }
                }
                Action::Access {
                    root,
                    path,
                    pass,
                    moves_noncopy,
                } => {
                    e.words.push(2);
                    e.name(root);
                    e.words.push(path.len() as u64);
                    for n in path {
                        e.name(n);
                    }
                    e.pass(*pass);
                    e.words.push(*moves_noncopy as u64);
                }
                Action::Opaque(_) => e.words.push(3),
            }
        }
        e.words.push(f.parameters.len() as u64);
        for p in &f.parameters {
            e.param(&p.parameter);
            e.words.push(p.struct_fields.len() as u64);
            for n in &p.struct_fields {
                e.name(n);
            }
        }
    }
    e
}

/// Reads the result words of `analyze`.
struct Words<'a> {
    words: &'a [u64],
    at: usize,
}

impl Words<'_> {
    fn next(&mut self) -> u64 {
        let w = self.words[self.at];
        self.at += 1;
        w
    }
    fn usize(&mut self) -> usize {
        self.next() as usize
    }
    fn text(&mut self) -> String {
        let n = self.usize();
        let mut bytes = Vec::with_capacity(n);
        for i in 0..n.div_ceil(8) {
            let w = self.words[self.at + i];
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        self.at += n.div_ceil(8);
        bytes.truncate(n);
        String::from_utf8(bytes).expect("the port writes UTF-8 messages")
    }
}

/// [`crate::analyze`] through the port.
pub(crate) fn analyze(program: &CheckedProgram, limits: Limits) -> Result<LintReport, LintError> {
    let encoded = encode(program);
    if encoded.too_deep {
        return crate::analyze_rust(program, limits);
    }
    let words = crate::port_binding::analyze(
        &encoded.words,
        &encoded.blob,
        limits.functions as u64,
        limits.actions as u64,
        limits.work as u64,
        limits.call_depth as u64,
    )
    .map_err(lint_error)?;
    let functions: Vec<_> = program.functions.iter().collect();
    let mut r = Words {
        words: &words,
        at: 0,
    };
    let work = r.usize();
    let mut findings = Vec::new();
    for _ in 0..r.usize() {
        let rule = r.next();
        let (name, facts) = functions[r.usize()];
        let span = r.usize()..r.usize();
        let detail = if rule == 1 {
            let effect_index = r.usize();
            FindingKind::UnusedEffectCapability {
                effect: facts.declaration.effects.instances[effect_index].clone(),
                effect_index,
            }
        } else {
            let parameter = facts.parameters[r.usize()].parameter.name.clone();
            let paths = |r: &mut Words| -> Vec<Vec<String>> {
                (0..r.usize())
                    .map(|_| match &facts.actions[r.usize()] {
                        Action::Access { path, .. } => path.clone(),
                        _ => unreachable!("the port names Access actions only"),
                    })
                    .collect()
            };
            let mutable_paths = paths(&mut r);
            let read_paths = paths(&mut r);
            FindingKind::OverbroadInoutProjection {
                parameter,
                mutable_paths,
                read_paths,
            }
        };
        findings.push(Finding {
            code: if rule == 1 { "L0102" } else { "L0101" }.into(),
            function: name.clone(),
            span,
            message: r.text(),
            detail,
        });
    }
    let mut skipped = Vec::new();
    for _ in 0..r.usize() {
        let (name, _) = functions[r.usize()];
        let reason = r.next();
        let reason = if reason >= OPAQUE {
            // The action of whichever function the analysis reached.
            let action = program
                .functions
                .values()
                .flat_map(|f| &f.actions)
                .nth((reason - OPAQUE) as usize);
            match action.expect("the port names an action of the program") {
                Action::Opaque(text) => text.clone(),
                _ => unreachable!("the port names Opaque actions only"),
            }
        } else {
            SKIP_REASONS[reason as usize].into()
        };
        skipped.push(Skipped {
            function: name.clone(),
            reason,
        });
    }
    Ok(LintReport {
        findings,
        skipped,
        work,
    })
}

/// [`crate::apply_edits`] through the port.
pub(crate) fn apply_edits(
    source: &str,
    edits: &[SourceEdit],
    max_bytes: usize,
) -> Result<String, LintError> {
    let mut words = Vec::with_capacity(1 + edits.len() * 6);
    let mut texts = String::new();
    words.push(edits.len() as u64);
    for e in edits {
        words.push(e.span.start as u64);
        words.push(e.span.end as u64);
        words.push(texts.len() as u64);
        words.push(e.expected.len() as u64);
        texts.push_str(&e.expected);
        words.push(texts.len() as u64);
        words.push(e.replacement.len() as u64);
        texts.push_str(&e.replacement);
    }
    let bytes = crate::port_binding::apply_edits(source, &words, &texts, max_bytes as u64)
        .map_err(lint_error)?;
    // Every edit is a whole UTF-8 sequence between char boundaries.
    Ok(String::from_utf8(bytes).expect("edits keep UTF-8 boundaries"))
}

/// [`Policy::parse`] through the port, or `None` for a document the Rust
/// code reads (see [`POLICY_UNSUPPORTED`]).
pub(crate) fn policy_parse(
    source: &str,
    limits: crate::policy::PolicyLimits,
) -> Option<Result<Policy, PolicyError>> {
    let words = match crate::port_binding::policy_parse(
        source,
        limits.bytes as u64,
        limits.depth as u64,
        limits.tokens as u64,
        limits.nodes as u64,
        limits.work as u64,
    ) {
        Ok(words) => words,
        Err(e) if e.code == POLICY_UNSUPPORTED => return None,
        Err(e) => {
            let (kind, message) = POLICY_ERRORS[e.code as usize];
            return Some(Err(PolicyError { kind, message }));
        }
    };
    let mut levels = Vec::new();
    let mut configured = Vec::new();
    for (i, lint) in LINTS.into_iter().enumerate() {
        let severity = match words[i] {
            1 => Severity::Allow,
            2 => Severity::Warn,
            3 => Severity::Deny,
            _ => continue,
        };
        levels.push((lint, severity));
        if words[7] & (1 << i) != 0 {
            configured.push(lint);
        }
    }
    let usage = PolicyUsage {
        input_bytes: Some(source.len()),
        decoded_bytes: words[8] as usize,
        tokens: words[9] as usize,
        nodes: words[10] as usize,
        work: words[11] as usize,
    };
    Some(Ok(Policy::from_parts(levels, configured, usage)))
}
