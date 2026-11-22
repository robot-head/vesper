//! The Rust side of the Vesper port (port wave P1 lane 15, #912).
//!
//! `compare` and `Projection::is_subset_of` encode their inputs as one flat
//! word stream over a text pool, in the node layout that
//! `vesper/src/check.ves` documents, and rebuild the crate's `Report` from
//! the issue and adapter words the port returns. `check_release` hands the
//! two version texts and the comparison's classification to
//! `vesper/src/version.ves`, which parses the versions, applies release
//! policy 1 and writes every message.
//!
//! The encoder interns strings in sorted order, so comparing ids compares
//! strings, and hash-conses nodes, so structurally equal values of one
//! class share one offset: the port compares offsets wherever the Rust code
//! compares values with `==`.
use crate::port_binding;
use crate::{
    Capability, Classification, Declaration, Effects, Fields, Function, Generic, GradeRelation,
    Interface, Issue, IssueKind, Mode, Parameter, Pass, Primitive, Projection, ReborrowAdapter,
    Report, Resumption, Type,
};
use crate::{
    RELEASE_POLICY_VERSION, ReleaseBump, ReleaseIssue, ReleaseIssueCode, ReleaseReport,
    ReleaseVersionError, ReleaseVersionSide,
};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A word-at-a-time multiplicative hasher (FxHash's mixing) for the string
/// interner, where SipHash cost about a seventh of a comparison.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 29)
    }
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(*w)).wrapping_mul(K);
        }
        let mut tail = [0u8; 8];
        tail[..rest.len()].copy_from_slice(rest);
        self.0 =
            (self.0.rotate_left(5) ^ u64::from_le_bytes(tail) ^ bytes.len() as u64).wrapping_mul(K);
    }
    fn write_u8(&mut self, b: u8) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

/// Node classes (`vesper/src/check.ves`). Two nodes share an offset only
/// when their class and words are equal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Generics,
    Type,
    Path,
    Projection,
    Parameter,
    Capability,
    Effects,
    Function,
    Fields,
    Payload,
    Declaration,
    Interface,
}

/// The header: old root, new root, string count, string table offset.
const HEADER: usize = 4;

/// One slot of the hash-consing table: a node's hash, offset, length and
/// class. A free slot has `at == u32::MAX` ([`EMPTY`]).
#[derive(Clone, Copy)]
struct Slot {
    hash: u64,
    at: u32,
    len: u32,
    class: Class,
}

const EMPTY: Slot = Slot {
    hash: 0,
    at: u32::MAX,
    len: 0,
    class: Class::Generics,
};

/// A single-pass encoder. Nodes are built on a stack: a node begins, pushes
/// its words (encoding each child first, which pushes and pops above it),
/// and ends, which moves it into the stream unless an equal node exists.
struct Encoder<'a> {
    words: Vec<u64>,
    /// Stream positions holding provisional string ids.
    string_words: Vec<usize>,
    /// Provisional string ids, in first-use order.
    ids: HashMap<&'a str, u64, BuildHasherDefault<Fx>>,
    strings: Vec<&'a str>,
    stack: Vec<u64>,
    /// Stack positions holding string ids, ascending.
    marks: Vec<usize>,
    /// Open-addressing table of written nodes.
    slots: Vec<Slot>,
    used: usize,
    /// How many `ty` calls are open, so deep input cannot overflow the stack.
    depth: usize,
    /// The deepest `depth` reached since it was last reset.
    max_depth: usize,
    /// The first interface's declarations by export name, with their
    /// offsets, when they are shallow enough to compare with `==`.
    memo: HashMap<&'a str, (&'a Declaration, u64), BuildHasherDefault<Fx>>,
    /// Whether `interface` records declarations in `memo` (the first
    /// interface) or reuses them (the second).
    reuse: bool,
    /// The distinct subtrees cut at `DEEP`, by Rust equality.
    deep: Vec<&'a Type>,
}

/// Types nested deeper than this are cut. Every checker walk stops at depth
/// 128 (counting each type and function level, as `depth` does), so it never
/// reads below the cut; it only compares cut subtrees by offset, and a cut
/// node `[7, description, k]` carries the index `k` of its subtree among the
/// distinct cut subtrees, so offsets stay equal exactly when the subtrees are.
const DEEP: usize = 256;

/// Declarations whose types nest at most this deep are compared with the
/// derived (recursive) `==` to reuse an equal declaration's node.
const MEMO_DEPTH: usize = 64;

impl<'a> Encoder<'a> {
    /// An encoder sized for about `items` declarations or projections, so
    /// small inputs do not pay for large buffers.
    fn with_room(items: usize) -> Self {
        let slots = (4 * (items + 4) * 8).next_power_of_two().min(1024);
        Self {
            words: {
                let mut w = Vec::with_capacity(HEADER + 64 * (items + 1));
                w.resize(HEADER, 0);
                w
            },
            string_words: Vec::with_capacity(8 * (items + 1)),
            ids: HashMap::with_capacity_and_hasher(4 * (items + 1), Default::default()),
            strings: Vec::with_capacity(4 * (items + 1)),
            stack: Vec::with_capacity(256),
            marks: Vec::with_capacity(64),
            slots: vec![EMPTY; slots],
            used: 0,
            depth: 0,
            max_depth: 0,
            memo: HashMap::default(),
            reuse: false,
            deep: Vec::new(),
        }
    }

    fn begin(&self) -> usize {
        self.stack.len()
    }

    fn w(&mut self, word: u64) {
        self.stack.push(word);
    }

    fn s(&mut self, text: &'a str) {
        let next = self.strings.len() as u64;
        let id = *self.ids.entry(text).or_insert_with(|| next);
        if id == next {
            self.strings.push(text);
        }
        self.marks.push(self.stack.len());
        self.stack.push(id);
    }

    fn hash(class: Class, words: &[u64]) -> u64 {
        // FxHash-style mixing; equality is checked word by word anyway.
        let mut h = (class as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        for &w in words {
            h = (h.rotate_left(5) ^ w).wrapping_mul(0x517C_C1B7_2722_0A95);
        }
        h
    }

    /// Ends the node begun at `start`: its offset, shared with any equal
    /// node of the same class already written.
    fn end(&mut self, class: Class, start: usize) -> u64 {
        let hash = Self::hash(class, &self.stack[start..]);
        let len = self.stack.len() - start;
        let mask = self.slots.len() - 1;
        let mut i = hash as usize & mask;
        // The string ids among this node's words.
        let first_mark = self.marks.partition_point(|&p| p < start);
        loop {
            let slot = self.slots[i];
            if slot.at == u32::MAX {
                break;
            }
            if slot.hash == hash && slot.class == class && slot.len as usize == len {
                let at = slot.at as usize;
                if self.words[at..at + len]
                    .iter()
                    .zip(&self.stack[start..])
                    .all(|(a, b)| a == b)
                {
                    self.stack.truncate(start);
                    self.marks.truncate(first_mark);
                    return at as u64;
                }
            }
            i = (i + 1) & mask;
        }
        let at = self.words.len();
        for &p in &self.marks[first_mark..] {
            self.string_words.push(at + p - start);
        }
        self.marks.truncate(first_mark);
        self.words.extend_from_slice(&self.stack[start..]);
        self.stack.truncate(start);
        self.slots[i] = Slot {
            hash,
            at: at as u32,
            len: len as u32,
            class,
        };
        self.used += 1;
        if self.used * 2 > self.slots.len() {
            self.grow();
        }
        at as u64
    }

    fn grow(&mut self) {
        let size = self.slots.len() * 2;
        let old = std::mem::replace(&mut self.slots, vec![EMPTY; size]);
        let mask = self.slots.len() - 1;
        for slot in old.into_iter().filter(|s| s.at != u32::MAX) {
            let mut i = slot.hash as usize & mask;
            while self.slots[i].at != u32::MAX {
                i = (i + 1) & mask;
            }
            self.slots[i] = slot;
        }
    }

    /// The finished stream (header, nodes, then the string table in sorted
    /// order) and its text pool.
    fn finish(mut self, first: u64, second: u64) -> (Vec<u64>, Vec<u8>) {
        let mut order: Vec<usize> = (0..self.strings.len()).collect();
        order.sort_unstable_by_key(|&i| self.strings[i]);
        let mut sorted = vec![0u64; order.len()];
        for (id, &provisional) in order.iter().enumerate() {
            sorted[provisional] = id as u64;
        }
        for &i in &self.string_words {
            self.words[i] = sorted[self.words[i] as usize];
        }
        let table = self.words.len() as u64;
        self.words[..HEADER].copy_from_slice(&[first, second, order.len() as u64, table]);
        let mut text = Vec::new();
        for &provisional in &order {
            let s = self.strings[provisional];
            self.words.push(text.len() as u64);
            self.words.push(s.len() as u64);
            text.extend_from_slice(s.as_bytes());
        }
        (self.words, text)
    }
}

/// `a == b`, without recursion, so a cut subtree of any depth compares
/// without overflowing the stack.
fn same_type(a: &Type, b: &Type) -> bool {
    let mut pending = vec![(a, b)];
    let same_capability = |a: &'_ Capability, b: &'_ Capability| {
        a.identity == b.identity
            && a.effect == b.effect
            && a.resumption == b.resumption
            && a.ceiling == b.ceiling
            && a.grade_relation == b.grade_relation
            && a.arguments.len() == b.arguments.len()
    };
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (
                Type::Named {
                    name: n,
                    arguments: x,
                },
                Type::Named {
                    name: m,
                    arguments: y,
                },
            ) => {
                if n != m || x.len() != y.len() {
                    return false;
                }
                pending.extend(x.iter().zip(y));
            }
            (Type::Tuple { elements: x }, Type::Tuple { elements: y }) => {
                if x.len() != y.len() {
                    return false;
                }
                pending.extend(x.iter().zip(y));
            }
            (Type::Function { signature: f }, Type::Function { signature: g }) => {
                let (fe, ge) = (&f.effects, &g.effects);
                if f.generics != g.generics
                    || f.mode != g.mode
                    || f.result_mode != g.result_mode
                    || f.parameters.len() != g.parameters.len()
                    || fe.row_variables != ge.row_variables
                    || fe.capabilities.len() != ge.capabilities.len()
                {
                    return false;
                }
                for (p, q) in f.parameters.iter().zip(&g.parameters) {
                    if p.pass != q.pass || p.projection != q.projection {
                        return false;
                    }
                    pending.push((&p.ty, &q.ty));
                }
                for (c, d) in fe.capabilities.iter().zip(&ge.capabilities) {
                    if !same_capability(c, d) {
                        return false;
                    }
                    pending.extend(c.arguments.iter().zip(&d.arguments));
                }
                pending.push((&f.result, &g.result));
            }
            (Type::Named { .. } | Type::Tuple { .. } | Type::Function { .. }, _)
            | (_, Type::Named { .. } | Type::Tuple { .. } | Type::Function { .. }) => return false,
            // The rest hold no types.
            _ => {
                if a != b {
                    return false;
                }
            }
        }
    }
    true
}

fn mode(m: Mode) -> u64 {
    match m {
        Mode::Copy => 0,
        Mode::Affine => 1,
        Mode::Linear => 2,
    }
}

fn resumption(r: Resumption) -> u64 {
    match r {
        Resumption::Affine => 0,
        Resumption::Linear => 1,
    }
}

fn primitive(p: Primitive) -> u64 {
    match p {
        Primitive::Unit => 0,
        Primitive::Never => 1,
        Primitive::Bool => 2,
        Primitive::U8 => 3,
        Primitive::U32 => 4,
        Primitive::U64 => 5,
        Primitive::I64 => 6,
        Primitive::F64 => 7,
        Primitive::String => 8,
    }
}

impl<'a> Encoder<'a> {
    fn generics(&mut self, g: &[Generic]) -> u64 {
        let n = self.begin();
        self.w(g.len() as u64);
        for g in g {
            match g {
                Generic::Type { bound } => {
                    self.w(0);
                    self.w(mode(*bound));
                }
                Generic::Row { minimum_resumption } => {
                    self.w(1);
                    self.w(resumption(*minimum_resumption));
                }
            }
        }
        self.end(Class::Generics, n)
    }

    fn ty(&mut self, t: &'a Type) -> u64 {
        let n = self.begin();
        if self.depth >= DEEP {
            let k = match self.deep.iter().position(|d| same_type(d, t)) {
                Some(k) => k,
                None => {
                    self.deep.push(t);
                    self.deep.len() - 1
                }
            };
            self.w(7);
            self.s("type nesting exceeds 128 levels");
            self.w(k as u64);
            return self.end(Class::Type, n);
        }
        self.depth += 1;
        self.max_depth = self.max_depth.max(self.depth);
        let at = self.ty_node(t, n);
        self.depth -= 1;
        at
    }

    fn ty_node(&mut self, t: &'a Type, n: usize) -> u64 {
        match t {
            Type::Primitive { name } => {
                self.w(0);
                self.w(primitive(*name));
            }
            Type::I64Range { min, max } => {
                self.w(1);
                self.w(*min as u64);
                self.w(*max as u64);
            }
            Type::U64Range { min, max } => {
                self.w(2);
                self.w(*min);
                self.w(*max);
            }
            Type::Parameter { index } => {
                self.w(3);
                self.w(*index as u64);
            }
            Type::Named { name, arguments } => {
                self.w(4);
                self.s(name);
                self.w(arguments.len() as u64);
                for a in arguments {
                    let a = self.ty(a);
                    self.w(a);
                }
            }
            Type::Tuple { elements } => {
                self.w(5);
                self.w(elements.len() as u64);
                for e in elements {
                    let e = self.ty(e);
                    self.w(e);
                }
            }
            Type::Function { signature } => {
                self.w(6);
                let f = self.function(signature);
                self.w(f);
            }
            Type::Unsupported { description } => {
                self.w(7);
                self.s(description);
            }
        }
        self.end(Class::Type, n)
    }

    fn projection(&mut self, p: &'a Projection) -> u64 {
        let n = self.begin();
        match p {
            Projection::All => self.w(0),
            Projection::Paths { paths } => {
                self.w(1);
                self.w(paths.len() as u64);
                for path in paths {
                    let m = self.begin();
                    self.w(path.len() as u64);
                    for component in path {
                        self.s(component);
                    }
                    let path = self.end(Class::Path, m);
                    self.w(path);
                }
            }
        }
        self.end(Class::Projection, n)
    }

    fn parameter(&mut self, p: &'a Parameter) -> u64 {
        let n = self.begin();
        self.w(match p.pass {
            Pass::In => 0,
            Pass::Inout => 1,
            Pass::Sink => 2,
        });
        let ty = self.ty(&p.ty);
        self.w(ty);
        let projection = self.projection(&p.projection);
        self.w(projection);
        self.end(Class::Parameter, n)
    }

    fn capability(&mut self, c: &'a Capability) -> u64 {
        let n = self.begin();
        self.s(&c.identity);
        self.s(&c.effect);
        self.w(resumption(c.resumption));
        self.w(c.ceiling.is_some() as u64);
        self.w(c.ceiling.unwrap_or(0));
        self.w(match c.grade_relation {
            GradeRelation::UpperBound => 0,
            GradeRelation::Exact => 1,
        });
        self.w(c.arguments.len() as u64);
        for a in &c.arguments {
            let a = self.ty(a);
            self.w(a);
        }
        self.end(Class::Capability, n)
    }

    fn effects(&mut self, e: &'a Effects) -> u64 {
        let n = self.begin();
        self.w(e.capabilities.len() as u64);
        for c in &e.capabilities {
            let c = self.capability(c);
            self.w(c);
        }
        self.w(e.row_variables.len() as u64);
        for v in &e.row_variables {
            self.w(*v as u64);
        }
        self.end(Class::Effects, n)
    }

    fn function(&mut self, f: &'a Function) -> u64 {
        let n = self.begin();
        let generics = self.generics(&f.generics);
        self.w(generics);
        self.w(mode(f.mode));
        let result = self.ty(&f.result);
        self.w(result);
        self.w(mode(f.result_mode));
        let effects = self.effects(&f.effects);
        self.w(effects);
        self.w(f.parameters.len() as u64);
        for p in &f.parameters {
            let p = self.parameter(p);
            self.w(p);
        }
        self.end(Class::Function, n)
    }

    fn declaration(&mut self, d: &'a Declaration) -> u64 {
        let n = self.begin();
        match d {
            Declaration::Function { signature } => {
                self.w(0);
                let f = self.function(signature);
                self.w(f);
            }
            Declaration::Struct {
                generics,
                mode: m,
                fields,
            } => {
                self.w(1);
                let g = self.generics(generics);
                self.w(g);
                self.w(mode(*m));
                let k = self.begin();
                match fields {
                    Fields::Closed => self.w(0),
                    Fields::Public { fields } => {
                        self.w(1);
                        self.w(fields.len() as u64);
                        for (name, t) in fields {
                            self.s(name);
                            let t = self.ty(t);
                            self.w(t);
                        }
                    }
                }
                let fields = self.end(Class::Fields, k);
                self.w(fields);
            }
            Declaration::Enum {
                generics,
                mode: m,
                open,
                variants,
            } => {
                self.w(2);
                let g = self.generics(generics);
                self.w(g);
                self.w(mode(*m));
                self.w(*open as u64);
                self.w(variants.len() as u64);
                for (name, payload) in variants {
                    self.s(name);
                    let k = self.begin();
                    self.w(payload.len() as u64);
                    for t in payload {
                        let t = self.ty(t);
                        self.w(t);
                    }
                    let payload = self.end(Class::Payload, k);
                    self.w(payload);
                }
            }
            Declaration::Effect {
                generics,
                sealed,
                resumption: q,
                operations,
            } => {
                self.w(3);
                let g = self.generics(generics);
                self.w(g);
                self.w(*sealed as u64);
                self.w(resumption(*q));
                self.w(operations.len() as u64);
                for (name, f) in operations {
                    self.s(name);
                    let f = self.function(f);
                    self.w(f);
                }
            }
            Declaration::Unsupported { description } => {
                self.w(4);
                self.s(description);
            }
        }
        self.end(Class::Declaration, n)
    }

    fn interface(&mut self, i: &'a Interface) -> u64 {
        let n = self.begin();
        self.w(u64::from(i.schema_version));
        self.w(u64::from(i.semantics_version));
        self.w(i.exports.len() as u64);
        let reuse = self.reuse;
        for (name, d) in &i.exports {
            self.s(name);
            // An export equal to the first interface's has the same node,
            // so its offset is reused without encoding it again.
            let at = match self.memo.get(name.as_str()) {
                Some(&(old, at)) if reuse && old == d => at,
                _ => {
                    self.max_depth = self.depth;
                    let at = self.declaration(d);
                    // `==` recurses: only shallow declarations are kept.
                    if !reuse && self.max_depth <= self.depth + MEMO_DEPTH {
                        self.memo.insert(name, (d, at));
                    }
                    at
                }
            };
            self.w(at);
        }
        self.reuse = true;
        self.w(i.external_types.len() as u64);
        for (name, e) in &i.external_types {
            self.s(name);
            self.w(e.arity as u64);
            self.w(mode(e.mode));
        }
        self.end(Class::Interface, n)
    }
}

fn text(pool: &[u8], off: u64, len: u64) -> String {
    let range = off as usize..(off + len) as usize;
    String::from_utf8_lossy(&pool[range]).into_owned()
}

pub(crate) fn compare(old: &Interface, new: &Interface) -> Report {
    let items = old.exports.len() + new.exports.len();
    let mut enc = Encoder::with_room(items + old.external_types.len() + new.external_types.len());
    let (a, b) = (enc.interface(old), enc.interface(new));
    let (words, text_pool) = enc.finish(a, b);
    let q = port_binding::compare(&words, &text_pool);
    let q = q.as_ref();
    let pool = q.pool().as_slice();
    let classification = match q.class() {
        0 => Classification::Patch,
        1 => Classification::Minor,
        2 => Classification::Major,
        _ => Classification::Unknown,
    };
    let issues = q
        .issues()
        .to_vec()
        .as_chunks::<9>()
        .0
        .iter()
        .map(|w| Issue {
            path: text(pool, w[1], w[2]),
            rule: text(pool, w[3], w[4]),
            kind: if w[0] == 0 {
                IssueKind::Breaking
            } else {
                IssueKind::Unsupported
            },
            message: text(pool, w[5], w[6]),
            caller_witness: text(pool, w[7], w[8]),
        })
        .collect();
    let required_adapters = q
        .adapters()
        .to_vec()
        .as_chunks::<3>()
        .0
        .iter()
        .map(|w| ReborrowAdapter {
            function_path: text(pool, w[0], w[1]),
            parameter: w[2] as usize,
        })
        .collect();
    Report {
        classification,
        issues,
        required_adapters,
    }
}

pub(crate) fn is_subset_of(new: &Projection, old: &Projection) -> bool {
    let mut enc = Encoder::with_room(0);
    let (a, b) = (enc.projection(new), enc.projection(old));
    let (words, _) = enc.finish(a, b);
    port_binding::is_subset_of(&words, words[0], words[1])
}

fn bump(code: u64) -> Option<ReleaseBump> {
    match code {
        1 => Some(ReleaseBump::Patch),
        2 => Some(ReleaseBump::Minor),
        3 => Some(ReleaseBump::Major),
        _ => None,
    }
}

pub(crate) fn check_release(
    old: &Interface,
    new: &Interface,
    old_version: &str,
    new_version: &str,
) -> Result<ReleaseReport, ReleaseVersionError> {
    // The comparison has no side effects, so computing it before the
    // version checks is not observable.
    let comparison = compare(old, new);
    let class = match comparison.classification {
        Classification::Patch => 0,
        Classification::Minor => 1,
        Classification::Major => 2,
        Classification::Unknown => 3,
    };
    let r = port_binding::release(old_version.as_bytes(), new_version.as_bytes(), class);
    let r = r.as_ref();
    let pool = r.pool().as_slice();
    if r.err() != 0 {
        let (side, version) = if r.err() == 1 {
            (ReleaseVersionSide::Old, old_version)
        } else {
            (ReleaseVersionSide::New, new_version)
        };
        return Err(ReleaseVersionError {
            side,
            version: version.into(),
            message: String::from_utf8_lossy(pool).into_owned(),
        });
    }
    let w = r.words().to_vec();
    let issues = w[5..]
        .as_chunks::<3>()
        .0
        .iter()
        .map(|i| ReleaseIssue {
            code: match i[0] {
                0 => ReleaseIssueCode::UnknownInterface,
                1 => ReleaseIssueCode::PrereleaseUnsupported,
                2 => ReleaseIssueCode::NonIncreasingVersion,
                _ => ReleaseIssueCode::InsufficientBump,
            },
            message: text(pool, i[1], i[2]),
        })
        .collect::<Vec<_>>();
    Ok(ReleaseReport {
        policy_version: RELEASE_POLICY_VERSION,
        old_version: text(pool, w[1], w[2]),
        new_version: text(pool, w[3], w[4]),
        required_bump: comparison.classification,
        comparison,
        actual_bump: bump(w[0]),
        accepted: issues.is_empty(),
        issues,
    })
}
