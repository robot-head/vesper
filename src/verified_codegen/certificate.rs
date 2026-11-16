//! Range certificates for the slot IR, and the production validator that
//! checks them before any machine code is emitted.
//!
//! `lean/CodegenFlowValidation.lean` proves that a per-row certificate
//! accepted by its executable checker (`validateProgram`) establishes the
//! control-flow invariant of `CodegenControlFlow`, and with it prefix
//! simulation, hardware-fault exclusion and divergence equivalence for the
//! selected code. This module closes the loop for compiler output:
//!
//! * [`generate`] (untrusted) computes a certificate for every lowered
//!   function by a forward range analysis over the slot IR. It is
//!   interprocedural: each function gets an entry summary (`In` facts every
//!   caller establishes) and an exit summary (`In` facts at every `Return`).
//! * [`validate`] (the trusted part) re-checks the certificate. For every
//!   row the Lean model covers, the check is [`lean_check_node`], a
//!   line-by-line mirror of `checkNode` (and through it `checkRow`,
//!   `transfer`, `supports`, `entails` and `constantKind` from
//!   `CodegenValidation`, and `checkConst`; an abort row constrains no
//!   successor). [`validate_lean`] is the mirror of
//!   `validateProgram` alone, which the tests compare against Lean `#eval`.
//! * [`check`] widens every comparison to the widest kind of its
//!   signedness ([`widen`]; Lean `widened_validation_sound`: the machine code
//!   and the source steps do not change), then generates and validates. The outcome is
//!   `Artifact::certificate`; with `Options::require_certificate`,
//!   compilation emits no code for a program whose certificate is rejected.
//!   The gate is off by default because the facts are not yet
//!   variant-sensitive: an enum payload word that holds a `u8` in one variant
//!   and a `bool` in another gets their join even after the tag test, and a
//!   call that never returns leaves its continuation without facts.
//!
//! Integer constants are 64-bit words in the slot IR and mathematical
//! integers in the Lean model. A word at or above 2^63 is the Lean constant
//! `word` (a `u64`) unless the certificate lists its row in
//! `signed_constants`, in which case it is `word - 2^64` (an `i64`). Both
//! encode to the same machine word, so the choice only picks which Lean
//! program the certificate is about; the generator picks the one the uses
//! need and the validator checks whatever it is given. A `Const` row may
//! give its destination any kind containing the value (`checkConst`), so a
//! value in `[2^32, 2^63)` can serve `u64` and `i64` uses.
//!
//! The rows outside the Lean model are checked by conservative rules that
//! are **not** covered by the Lean proofs; they rely on the calling
//! convention (`Call` preserves the caller's slots, the callee's incoming
//! area is the caller's outgoing area, `Enter` gives fresh slot and
//! outgoing words):
//!
//! * `Enter` must be row 1, right after the entry label in row 0. Only `In`
//!   facts survive it.
//! * Row 0's annotation is the function's entry summary and may name only
//!   `In` words; the program entry (function 0) must need nothing.
//! * `Call` must target a function entry label. The caller's facts must
//!   support the callee's entry summary with `In i` read as `Out i`; after
//!   the call every `Out` fact is dropped and replaced by the callee's exit
//!   summary, again with `In i` read as `Out i`, and the result must support
//!   the next row's annotation.
//! * `Return` must support the function's exit summary (`post`), which may
//!   name only `In` words.
//! * Labels are unique across the program; `Jump` and `BranchZero` targets
//!   must be labels of the same function other than its entry label (the
//!   Lean model would treat an unknown label as an abort).
//! * Annotations never mention `f64` (the Lean model has no binary64 kind).
//!
//! What the certificate establishes is the operand-range precondition the
//! Lean simulation needs (no hardware division fault, every checked row on
//! in-range operands). It does not prove the lowering correct, termination,
//! absence of language aborts, or the byte encodings.
use super::machine::{Kind, Label, Lir, Loc};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The range facts that hold before a row (Lean `Annotation`).
pub type Annotation = Vec<(Loc, Kind)>;

/// The certificate of one function.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionCertificate {
    /// One annotation per row; row 0's is the entry summary.
    pub annotations: Vec<Annotation>,
    /// The exit summary: facts on `In` words at every `Return`.
    pub post: Annotation,
    /// Rows whose `Const` word is read as `word - 2^64` in the Lean model.
    pub signed_constants: BTreeSet<usize>,
}

/// The certificate of a program, in the order of `Artifact::lir`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Certificate {
    pub functions: Vec<FunctionCertificate>,
}

/// Why the validator refused a certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub function: String,
    pub row: usize,
    pub reason: String,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "range certificate rejected in '{}' at row {}: {}",
            self.function, self.row, self.reason
        )
    }
}

/// Widen, generate and validate the certificate of `program` (the rows the
/// code is selected from).
pub fn check(program: &[(String, Vec<Lir>)]) -> Result<Certificate, Rejection> {
    let widened: Vec<(String, Vec<Lir>)> = program
        .iter()
        .map(|(name, rows)| (name.clone(), rows.iter().map(widen).collect()))
        .collect();
    let certificate = generate(&widened);
    validate(&widened, &certificate)?;
    Ok(certificate)
}

// ---------------------------------------------------------------------------
// The Lean mirror (`CodegenValidation`, `CodegenFlowValidation`).

/// Facts as a finite map; a missing location is Lean's `none`.
pub type Facts = BTreeMap<Loc, Kind>;

const TWO_64: i128 = 1 << 64;

/// Whether `kind` exists in the Lean model (`Kind`).
fn lean_kind(kind: Kind) -> bool {
    kind != Kind::F64
}

/// `Kind.lo` (only meaningful for Lean kinds).
fn lo(kind: Kind) -> i128 {
    match kind {
        Kind::I64 => -(1 << 63),
        _ => 0,
    }
}

/// `Kind.hi` (only meaningful for Lean kinds).
fn hi(kind: Kind) -> i128 {
    match kind {
        Kind::U8 => 255,
        Kind::U32 => 4_294_967_295,
        Kind::U64 => TWO_64 - 1,
        Kind::I64 => (1 << 63) - 1,
        Kind::Bool => 1,
        Kind::F64 => -1,
    }
}

fn fits(kind: Kind, value: i128) -> bool {
    lo(kind) <= value && value <= hi(kind)
}

/// The Lean integer of a `Const` row.
pub fn constant_value(word: u64, signed: bool) -> i128 {
    if signed {
        word as i128 - TWO_64
    } else {
        word as i128
    }
}

/// `constantKind`.
pub fn constant_kind(value: i128) -> Option<Kind> {
    [Kind::Bool, Kind::U8, Kind::U32, Kind::U64, Kind::I64]
        .into_iter()
        .find(|k| fits(*k, value))
}

/// `factsOf`: the first fact for a location wins.
pub fn facts_of(annotation: &[(Loc, Kind)]) -> Facts {
    let mut facts = Facts::new();
    for (location, kind) in annotation {
        facts.entry(*location).or_insert(*kind);
    }
    facts
}

/// `supports`.
pub fn supports(facts: &Facts, location: Loc, kind: Kind) -> bool {
    match facts.get(&location) {
        None => false,
        Some(known) => {
            lean_kind(kind) && lean_kind(*known) && lo(kind) <= lo(*known) && hi(*known) <= hi(kind)
        }
    }
}

/// `entails`.
pub fn entails(facts: &Facts, required: &[(Loc, Kind)]) -> bool {
    required.iter().all(|(l, k)| supports(facts, *l, *k))
}

fn put(facts: &mut Facts, location: Loc, kind: Option<Kind>) {
    match kind {
        Some(k) => {
            facts.insert(location, k);
        }
        None => {
            facts.remove(&location);
        }
    }
}

/// `checkRow`. `Call` and `Enter` are rejected, as in Lean.
pub fn check_row(facts: &Facts, row: &Lir) -> bool {
    use Loc::Slot;
    match *row {
        Lir::Arith { kind, a, b, .. } | Lir::Wrapping { kind, a, b, .. } => {
            kind != Kind::Bool && supports(facts, Slot(a), kind) && supports(facts, Slot(b), kind)
        }
        Lir::Neg { kind, a, .. } => kind != Kind::Bool && supports(facts, Slot(a), kind),
        Lir::Not { a, .. } => supports(facts, Slot(a), Kind::Bool),
        Lir::Compare { kind, a, b, .. } => {
            supports(facts, Slot(a), kind) && supports(facts, Slot(b), kind)
        }
        Lir::BranchZero { .. }
        | Lir::Jump(_)
        | Lir::Label(_)
        | Lir::Call { .. }
        | Lir::Enter { .. }
        | Lir::Return => false,
        // The slot IR has no `trap .fault` row.
        Lir::Const { .. }
        | Lir::Move { .. }
        | Lir::FArith { .. }
        | Lir::FNeg { .. }
        | Lir::FCompare { .. }
        | Lir::StoreOut { .. }
        | Lir::LoadOut { .. }
        | Lir::LoadIn { .. }
        | Lir::StoreIn { .. }
        | Lir::Trap(_) => true,
    }
}

/// `transfer`; `signed` says how a `Const` word is read.
pub fn transfer(facts: &Facts, row: &Lir, signed: bool) -> Facts {
    use Loc::{In, Out, Slot};
    let mut next = facts.clone();
    match *row {
        Lir::Const { dst, word } => put(
            &mut next,
            Slot(dst),
            constant_kind(constant_value(word, signed)),
        ),
        Lir::Move { dst, src } => put(&mut next, Slot(dst), facts.get(&Slot(src)).copied()),
        Lir::Arith { kind, dst, .. }
        | Lir::Wrapping { kind, dst, .. }
        | Lir::Neg { kind, dst, .. } => put(&mut next, Slot(dst), Some(kind)),
        Lir::Not { dst, .. } | Lir::Compare { dst, .. } | Lir::FCompare { dst, .. } => {
            put(&mut next, Slot(dst), Some(Kind::Bool))
        }
        Lir::FArith { dst, .. } | Lir::FNeg { dst, .. } => put(&mut next, Slot(dst), None),
        Lir::StoreOut { index, src } => put(&mut next, Out(index), facts.get(&Slot(src)).copied()),
        Lir::LoadOut { dst, index } => put(&mut next, Slot(dst), facts.get(&Out(index)).copied()),
        Lir::LoadIn { dst, index } => put(&mut next, Slot(dst), facts.get(&In(index)).copied()),
        Lir::StoreIn { index, src } => put(&mut next, In(index), facts.get(&Slot(src)).copied()),
        _ => {}
    }
    next
}

fn annotation(annotations: &[Annotation], pc: usize) -> &[(Loc, Kind)] {
    annotations.get(pc).map(Vec::as_slice).unwrap_or(&[])
}

/// `checkJump`: an unknown label is the modelled unreachable abort.
fn check_jump(
    labels: &HashMap<Label, usize>,
    annotations: &[Annotation],
    facts: &Facts,
    label: Label,
) -> bool {
    match labels.get(&label) {
        None => true,
        Some(target) => entails(facts, annotation(annotations, *target)),
    }
}

/// `widen`: a comparison at the widest kind of the same signedness. It
/// selects the same instructions (Lean `widen_select`) and takes the same
/// source step, so the certificate is checked on widened rows.
pub fn widen(row: &Lir) -> Lir {
    match *row {
        Lir::Compare {
            cond,
            kind,
            dst,
            a,
            b,
        } if lean_kind(kind) => Lir::Compare {
            cond,
            kind: if kind == Kind::I64 {
                Kind::I64
            } else {
                Kind::U64
            },
            dst,
            a,
            b,
        },
        _ => row.clone(),
    }
}

/// `checkConst`: the destination may get any kind containing the value.
pub fn check_const(facts: &Facts, dst: u32, value: i128, next: &[(Loc, Kind)]) -> bool {
    next.iter().all(|(l, k)| {
        if *l == Loc::Slot(dst) {
            lean_kind(*k) && fits(*k, value)
        } else {
            supports(facts, *l, *k)
        }
    })
}

/// `checkNode`.
pub fn lean_check_node(
    labels: &HashMap<Label, usize>,
    annotations: &[Annotation],
    signed: &BTreeSet<usize>,
    pc: usize,
    row: &Lir,
) -> bool {
    let facts = facts_of(annotation(annotations, pc));
    let next = annotation(annotations, pc + 1);
    match *row {
        Lir::BranchZero { cond, target } => {
            supports(&facts, Loc::Slot(cond), Kind::Bool)
                && entails(&facts, next)
                && check_jump(labels, annotations, &facts, target)
        }
        Lir::Jump(label) => check_jump(labels, annotations, &facts, label),
        Lir::Label(_) => entails(&facts, next),
        Lir::Const { dst, word } => check_const(
            &facts,
            dst,
            constant_value(word, signed.contains(&pc)),
            next,
        ),
        // An abort has no successor.
        Lir::Trap(_) => check_row(&facts, row),
        Lir::Return => true,
        _ => check_row(&facts, row) && entails(&transfer(&facts, row, signed.contains(&pc)), next),
    }
}

/// `validateProgram` (with `validateFrom` from row 0).
pub fn validate_lean(
    rows: &[Lir],
    labels: &HashMap<Label, usize>,
    annotations: &[Annotation],
    signed: &BTreeSet<usize>,
) -> bool {
    rows.iter()
        .enumerate()
        .all(|(pc, row)| lean_check_node(labels, annotations, signed, pc, row))
}

/// The label resolver of one function: each `Label` row's label to its row.
pub fn labels_of(rows: &[Lir]) -> HashMap<Label, usize> {
    let mut labels = HashMap::new();
    for (pc, row) in rows.iter().enumerate() {
        if let Lir::Label(l) = row {
            labels.entry(*l).or_insert(pc);
        }
    }
    labels
}

// ---------------------------------------------------------------------------
// The production validator.

fn in_to_out(annotation: &[(Loc, Kind)]) -> Annotation {
    annotation
        .iter()
        .filter_map(|(l, k)| match l {
            Loc::In(i) => Some((Loc::Out(*i), *k)),
            _ => None,
        })
        .collect()
}

fn only_in(annotation: &[(Loc, Kind)]) -> bool {
    annotation.iter().all(|(l, _)| matches!(l, Loc::In(_)))
}

/// Validate `certificate` for `program` (the functions in `Artifact::lir`
/// order, the program entry first).
pub fn validate(
    program: &[(String, Vec<Lir>)],
    certificate: &Certificate,
) -> Result<(), Rejection> {
    let reject = |f: usize, row: usize, reason: String| Rejection {
        function: program.get(f).map(|(n, _)| n.clone()).unwrap_or_default(),
        row,
        reason,
    };
    if certificate.functions.len() != program.len() {
        return Err(reject(0, 0, "one function certificate per function".into()));
    }
    let mut entries = HashMap::new();
    let mut all_labels = HashSet::new();
    for (f, (_, rows)) in program.iter().enumerate() {
        match rows.first() {
            Some(Lir::Label(l)) => {
                entries.insert(*l, f);
            }
            _ => return Err(reject(f, 0, "row 0 is not the entry label".into())),
        }
        for (pc, row) in rows.iter().enumerate() {
            if let Lir::Label(l) = row
                && !all_labels.insert(*l)
            {
                return Err(reject(f, pc, format!("label {l} is defined twice")));
            }
        }
    }
    // Every shape first: a call reads its callee's entry annotation.
    for (f, ((_, rows), cert)) in program.iter().zip(&certificate.functions).enumerate() {
        if cert.annotations.len() != rows.len() {
            return Err(reject(f, 0, "one annotation per row".into()));
        }
    }
    for (f, ((_, rows), cert)) in program.iter().zip(&certificate.functions).enumerate() {
        let annotations = &cert.annotations;
        if let Some(pc) = cert
            .signed_constants
            .iter()
            .find(|pc| !matches!(rows.get(**pc), Some(Lir::Const { .. })))
        {
            return Err(reject(
                f,
                *pc,
                "signed constant on a non-constant row".into(),
            ));
        }
        if annotations
            .iter()
            .chain(std::iter::once(&cert.post))
            .flatten()
            .any(|(_, k)| !lean_kind(*k))
        {
            return Err(reject(f, 0, "an annotation names f64".into()));
        }
        if !only_in(&annotations[0]) || !only_in(&cert.post) {
            return Err(reject(f, 0, "a summary names a non-incoming word".into()));
        }
        if f == 0 && !annotations[0].is_empty() {
            return Err(reject(f, 0, "the program entry has a precondition".into()));
        }
        let labels = labels_of(rows);
        let Some(Lir::Label(entry)) = rows.first() else {
            unreachable!()
        };
        for (pc, row) in rows.iter().enumerate() {
            match *row {
                Lir::Enter { .. } if pc != 1 => {
                    return Err(reject(f, pc, "Enter outside the prologue".into()));
                }
                _ if pc == 1 && !matches!(row, Lir::Enter { .. }) => {
                    return Err(reject(f, pc, "row 1 is not Enter".into()));
                }
                Lir::Jump(l) | Lir::BranchZero { target: l, .. }
                    if l == *entry || !labels.contains_key(&l) =>
                {
                    return Err(reject(f, pc, format!("jump to label {l} outside the body")));
                }
                _ => {}
            }
            let facts = || facts_of(annotation(annotations, pc));
            let next = annotation(annotations, pc + 1);
            let accepted = match *row {
                Lir::Enter { .. } => {
                    let mut facts = facts();
                    facts.retain(|l, _| matches!(l, Loc::In(_)));
                    entails(&facts, next)
                }
                Lir::Call { callee } => {
                    let Some(&g) = entries.get(&callee) else {
                        return Err(reject(f, pc, format!("call to non-entry label {callee}")));
                    };
                    let callee = &certificate.functions[g];
                    let mut facts = facts();
                    if !entails(&facts, &in_to_out(&callee.annotations[0])) {
                        return Err(reject(
                            f,
                            pc,
                            format!("arguments do not establish the entry of '{}'", program[g].0),
                        ));
                    }
                    facts.retain(|l, _| !matches!(l, Loc::Out(_)));
                    for (l, k) in facts_of(&in_to_out(&callee.post)) {
                        facts.insert(l, k);
                    }
                    entails(&facts, next)
                }
                Lir::Return => entails(&facts(), &cert.post),
                _ => lean_check_node(&labels, annotations, &cert.signed_constants, pc, row),
            };
            if !accepted {
                return Err(reject(f, pc, format!("{row:?} is not established")));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The generator (untrusted: everything it produces is validated).

/// A `Const` row: (function, row).
type ConstId = (usize, usize);

/// Abstract state: Lean facts plus, per location, the unsigned large
/// constants it may hold (so a use at `i64` can flip their reading).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    facts: Facts,
    origins: BTreeMap<Loc, BTreeSet<ConstId>>,
}

/// The narrowest Lean kind containing both ranges.
fn join_kind(a: Kind, b: Kind) -> Option<Kind> {
    let (l, h) = (lo(a).min(lo(b)), hi(a).max(hi(b)));
    [Kind::Bool, Kind::U8, Kind::U32, Kind::U64, Kind::I64]
        .into_iter()
        .find(|k| lo(*k) <= l && h <= hi(*k))
}

impl State {
    fn join(&self, other: &State) -> State {
        let mut facts = Facts::new();
        for (l, a) in &self.facts {
            if let Some(b) = other.facts.get(l)
                && let Some(k) = join_kind(*a, *b)
            {
                facts.insert(*l, k);
            }
        }
        let mut origins = self.origins.clone();
        for (l, o) in &other.origins {
            origins.entry(*l).or_default().extend(o.iter().copied());
        }
        State { facts, origins }
    }

    fn copy(&mut self, dst: Loc, src: Loc) {
        let kind = self.facts.get(&src).copied();
        put(&mut self.facts, dst, kind);
        match self.origins.get(&src).cloned() {
            Some(o) => {
                self.origins.insert(dst, o);
            }
            None => {
                self.origins.remove(&dst);
            }
        }
    }

    fn set(&mut self, dst: Loc, kind: Option<Kind>) {
        put(&mut self.facts, dst, kind);
        self.origins.remove(&dst);
    }

    fn retain(&mut self, keep: impl Fn(&Loc) -> bool) {
        self.facts.retain(|l, _| keep(l));
        self.origins.retain(|l, _| keep(l));
    }

    /// The `In` part, renamed to `Out` (or back).
    fn rename(&self, from: fn(&Loc) -> Option<Loc>) -> State {
        State {
            facts: self
                .facts
                .iter()
                .filter_map(|(l, k)| from(l).map(|l| (l, *k)))
                .collect(),
            origins: self
                .origins
                .iter()
                .filter_map(|(l, o)| from(l).map(|l| (l, o.clone())))
                .collect(),
        }
    }
}

fn out_as_in(l: &Loc) -> Option<Loc> {
    match l {
        Loc::Out(i) => Some(Loc::In(*i)),
        _ => None,
    }
}

fn in_as_out(l: &Loc) -> Option<Loc> {
    match l {
        Loc::In(i) => Some(Loc::Out(*i)),
        _ => None,
    }
}

fn join_into(slot: &mut Option<State>, state: &State) -> bool {
    let joined = match slot {
        None => state.clone(),
        Some(old) => old.join(state),
    };
    if slot.as_ref() == Some(&joined) {
        false
    } else {
        *slot = Some(joined);
        true
    }
}

/// The operands a row requires, as Lean `checkRow` checks them.
fn required(row: &Lir) -> Vec<(Loc, Kind)> {
    use Loc::Slot;
    match *row {
        Lir::Arith { kind, a, b, .. }
        | Lir::Wrapping { kind, a, b, .. }
        | Lir::Compare { kind, a, b, .. } => vec![(Slot(a), kind), (Slot(b), kind)],
        Lir::Neg { kind, a, .. } => vec![(Slot(a), kind)],
        Lir::Not { a, .. } => vec![(Slot(a), Kind::Bool)],
        Lir::BranchZero { cond, .. } => vec![(Slot(cond), Kind::Bool)],
        _ => vec![],
    }
}

struct Summaries {
    pre: Vec<Option<State>>,
    post: Vec<Option<State>>,
}

/// The locations whose facts row `pc` or a later row can still need
/// (backward liveness over the certificate's edges). Annotations keep only
/// these, so they stay proportional to the live values rather than to every
/// temporary the function ever wrote. `In` and `Out` words are few and stay
/// live across `Return` and `Call` (the summaries read them).
fn live_locations(rows: &[Lir]) -> Vec<BTreeSet<Loc>> {
    let labels = labels_of(rows);
    let mut ins = BTreeSet::new();
    let mut outs = BTreeSet::new();
    for row in rows {
        match *row {
            Lir::LoadIn { index, .. } | Lir::StoreIn { index, .. } => {
                ins.insert(Loc::In(index));
            }
            Lir::LoadOut { index, .. } | Lir::StoreOut { index, .. } => {
                outs.insert(Loc::Out(index));
            }
            _ => {}
        }
    }
    let successors = |pc: usize| -> Vec<usize> {
        let next = (pc + 1 < rows.len()).then_some(pc + 1);
        match rows[pc] {
            Lir::Jump(l) => labels.get(&l).copied().into_iter().collect(),
            Lir::BranchZero { target, .. } => labels
                .get(&target)
                .copied()
                .into_iter()
                .chain(next)
                .collect(),
            Lir::Return | Lir::Trap(_) => vec![],
            _ => next.into_iter().collect(),
        }
    };
    let mut live: Vec<BTreeSet<Loc>> = vec![BTreeSet::new(); rows.len()];
    loop {
        let mut changed = false;
        for pc in (0..rows.len()).rev() {
            let mut after = BTreeSet::new();
            for s in successors(pc) {
                after.extend(live[s].iter().copied());
            }
            let copy = |dst: Loc, src: Loc, after: &mut BTreeSet<Loc>| {
                if after.remove(&dst) {
                    after.insert(src);
                }
            };
            match rows[pc] {
                Lir::Move { dst, src } => copy(Loc::Slot(dst), Loc::Slot(src), &mut after),
                Lir::StoreOut { index, src } => copy(Loc::Out(index), Loc::Slot(src), &mut after),
                Lir::LoadOut { dst, index } => copy(Loc::Slot(dst), Loc::Out(index), &mut after),
                Lir::LoadIn { dst, index } => copy(Loc::Slot(dst), Loc::In(index), &mut after),
                Lir::StoreIn { index, src } => copy(Loc::In(index), Loc::Slot(src), &mut after),
                Lir::Return => after.extend(ins.iter().copied()),
                Lir::Call { .. } => {
                    after.retain(|l| !matches!(l, Loc::Out(_)));
                    after.extend(outs.iter().copied());
                }
                Lir::Enter { .. } => after.retain(|l| matches!(l, Loc::In(_))),
                ref row => {
                    if let Lir::Const { dst, .. } = row {
                        after.remove(&Loc::Slot(*dst));
                    }
                    if let Some(dst) = destination(row) {
                        after.remove(&Loc::Slot(dst));
                    }
                    for (l, _) in required(row) {
                        after.insert(l);
                    }
                }
            }
            if after != live[pc] {
                live[pc] = after;
                changed = true;
            }
        }
        if !changed {
            return live;
        }
    }
}

/// Analyse function `f` under the current summaries; returns the state
/// before each row, and updates the callees' entry and this exit summary.
fn analyse(
    program: &[(String, Vec<Lir>)],
    entries: &HashMap<Label, usize>,
    signed: &BTreeSet<ConstId>,
    f: usize,
    summaries: &mut Summaries,
    flips: &mut BTreeSet<ConstId>,
    lives: &[Vec<BTreeSet<Loc>>],
) -> (Vec<Option<State>>, bool) {
    let rows = &program[f].1;
    let labels = labels_of(rows);
    let live = &lives[f];
    let mut states: Vec<Option<State>> = vec![None; rows.len()];
    let mut changed = false;
    let Some(pre) = summaries.pre[f].clone() else {
        return (states, false);
    };
    if rows.is_empty() {
        return (states, false);
    }
    let mut pre = pre;
    pre.retain(|l| live[0].contains(l));
    states[0] = Some(pre);
    let mut work = vec![0usize];
    let mut queued = vec![false; rows.len()];
    queued[0] = true;
    while let Some(pc) = work.pop() {
        queued[pc] = false;
        let mut state = states[pc].clone().expect("queued rows have a state");
        let row = &rows[pc];
        for (l, k) in required(row) {
            if k == Kind::I64 && !supports(&state.facts, l, k) {
                flips.extend(state.origins.get(&l).into_iter().flatten().copied());
            }
        }
        let mut successors: Vec<(usize, State)> = vec![];
        match *row {
            Lir::Jump(l) => {
                if let Some(t) = labels.get(&l) {
                    successors.push((*t, state));
                }
            }
            Lir::BranchZero { target, .. } => {
                if let Some(t) = labels.get(&target) {
                    successors.push((*t, state.clone()));
                }
                successors.push((pc + 1, state));
            }
            Lir::Trap(_) => {}
            Lir::Return => {
                let exit = state.rename(|l| matches!(l, Loc::In(_)).then_some(*l));
                changed |= join_into(&mut summaries.post[f], &exit);
            }
            Lir::Enter { .. } => {
                state.retain(|l| matches!(l, Loc::In(_)));
                successors.push((pc + 1, state));
            }
            Lir::Call { callee } => {
                if let Some(&g) = entries.get(&callee) {
                    changed |= join_into(&mut summaries.pre[g], &state.rename(out_as_in));
                    if let Some(post) = &summaries.post[g] {
                        state.retain(|l| !matches!(l, Loc::Out(_)));
                        let results = post.rename(in_as_out);
                        state.facts.extend(results.facts);
                        state.origins.extend(results.origins);
                        successors.push((pc + 1, state));
                    }
                }
            }
            Lir::Const { dst, word } => {
                // A flagged constant is read as an `i64` (wrapping words at
                // or above 2^63); the others get `constantKind` and remember
                // their row if an `i64` use could need the flag.
                let flagged = signed.contains(&(f, pc));
                let value = constant_value(word, flagged && word >= 1 << 63);
                let kind = if flagged && fits(Kind::I64, value) {
                    Some(Kind::I64)
                } else {
                    constant_kind(value)
                };
                state.set(Loc::Slot(dst), kind);
                if !flagged && word > u32::MAX as u64 {
                    state
                        .origins
                        .insert(Loc::Slot(dst), BTreeSet::from([(f, pc)]));
                }
                successors.push((pc + 1, state));
            }
            Lir::Move { dst, src } => {
                state.copy(Loc::Slot(dst), Loc::Slot(src));
                successors.push((pc + 1, state));
            }
            Lir::StoreOut { index, src } => {
                state.copy(Loc::Out(index), Loc::Slot(src));
                successors.push((pc + 1, state));
            }
            Lir::LoadOut { dst, index } => {
                state.copy(Loc::Slot(dst), Loc::Out(index));
                successors.push((pc + 1, state));
            }
            Lir::LoadIn { dst, index } => {
                state.copy(Loc::Slot(dst), Loc::In(index));
                successors.push((pc + 1, state));
            }
            Lir::StoreIn { index, src } => {
                state.copy(Loc::In(index), Loc::Slot(src));
                successors.push((pc + 1, state));
            }
            _ => {
                let facts = transfer(&state.facts, row, false);
                let mut next = State {
                    facts,
                    origins: state.origins,
                };
                if let Some(dst) = destination(row) {
                    next.origins.remove(&Loc::Slot(dst));
                }
                successors.push((pc + 1, next));
            }
        }
        for (target, mut s) in successors {
            if target >= rows.len() {
                continue;
            }
            s.retain(|l| live[target].contains(l));
            if join_into(&mut states[target], &s) && !queued[target] {
                queued[target] = true;
                work.push(target);
            }
        }
    }
    (states, changed)
}

fn destination(row: &Lir) -> Option<u32> {
    match *row {
        Lir::Arith { dst, .. }
        | Lir::Wrapping { dst, .. }
        | Lir::Neg { dst, .. }
        | Lir::Not { dst, .. }
        | Lir::Compare { dst, .. }
        | Lir::FArith { dst, .. }
        | Lir::FNeg { dst, .. }
        | Lir::FCompare { dst, .. } => Some(dst),
        _ => None,
    }
}

fn to_annotation(state: Option<&State>) -> Annotation {
    state
        .map(|s| s.facts.iter().map(|(l, k)| (*l, *k)).collect())
        .unwrap_or_default()
}

/// Compute a certificate for `program` (function 0 is the program entry).
/// Rows the analysis never reaches get empty annotations.
pub fn generate(program: &[(String, Vec<Lir>)]) -> Certificate {
    let mut entries = HashMap::new();
    for (f, (_, rows)) in program.iter().enumerate() {
        if let Some(Lir::Label(l)) = rows.first() {
            entries.entry(*l).or_insert(f);
        }
    }
    let lives: Vec<_> = program
        .iter()
        .map(|(_, rows)| live_locations(rows))
        .collect();
    let mut signed = BTreeSet::new();
    loop {
        let mut summaries = Summaries {
            pre: vec![None; program.len()],
            post: vec![None; program.len()],
        };
        if !program.is_empty() {
            summaries.pre[0] = Some(State::default());
        }
        let mut flips = BTreeSet::new();
        let states = loop {
            let mut changed = false;
            let mut states = vec![];
            flips.clear();
            for f in 0..program.len() {
                let (s, c) = analyse(
                    program,
                    &entries,
                    &signed,
                    f,
                    &mut summaries,
                    &mut flips,
                    &lives,
                );
                changed |= c;
                states.push(s);
            }
            if !changed {
                break states;
            }
        };
        let new: Vec<_> = flips.difference(&signed).copied().collect();
        if new.is_empty() {
            return Certificate {
                functions: states
                    .iter()
                    .enumerate()
                    .map(|(f, s)| {
                        let mut annotations: Vec<Annotation> =
                            s.iter().map(|s| to_annotation(s.as_ref())).collect();
                        // The program entry needs nothing; summaries are `In` only.
                        if let Some(first) = annotations.first_mut() {
                            first.retain(|(l, _)| matches!(l, Loc::In(_)));
                        }
                        FunctionCertificate {
                            annotations,
                            post: to_annotation(summaries.post[f].as_ref()),
                            signed_constants: signed
                                .iter()
                                .filter(|(g, pc)| {
                                    *g == f
                                        && matches!(
                                            program[f].1[*pc],
                                            Lir::Const { word, .. } if word >= 1 << 63
                                        )
                                })
                                .map(|(_, pc)| *pc)
                                .collect(),
                        }
                    })
                    .collect(),
            };
        }
        signed.extend(new);
    }
}
