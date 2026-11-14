//! Resonance stereo chain fusion (design §32.1 item 4).
//!
//! A [`DspGraph`] is a DAG of stereo stages over named sources: gain, pan
//! (constant-power law), a biquad section applied to both channels, and mix.
//! [`DspGraph::evaluate`] is the unfused reference: every stage is one pass
//! over a whole block that writes a new stereo buffer. [`DspGraph::fuse`]
//! saturates the graph with [`RULES`]; [`FusedDsp::evaluate`] runs the
//! extracted term, where a chain of unary stages above a source or mix is one
//! loop that carries each sample through every stage in registers.
//!
//! Exactness: `gain-stereo`, `pan-stereo`, `biquad-cascade`, `cascade-merge`
//! and the list rules keep each sample's FP32 operation sequence, so their
//! fused results are bit-identical. `stereo-compose` and `mix-sends`
//! reassociate products and sums of gains, and the two `gain-*-biquad` rules
//! scale a section's feed-forward coefficients instead of the signal; those
//! hold exactly over the reals (`lean/FusionRules.lean`) and within
//! [`gain_tolerance`] / [`filter_tolerance`] in FP32. Biquads start from zero
//! state on every evaluation, which the gain-folding rules require (a linear
//! time-invariant section commutes with a gain only from a scaled state).
use crate::engine::{self, Extracted};
use crate::{Error, MAX_ELEMENTS, MAX_NODES, next_owner, valid_name};
use std::collections::HashMap;
use std::fmt::Write as _;
use vesper_resonance::pdc::{self, Input, NodeId, NodeSpec};

/// The DSP rule set. A stage that writes a stereo buffer costs 16; a fused
/// per-sample stage 4; list cells and gain expressions 1-2.
pub const RULES: &str = r#"
(sort S)
(sort G)
(sort C)
(sort Q)
(constructor Src (String) S)
(constructor Gain (i64 S) S :cost 16)
(constructor Pan (i64 S) S :cost 16)
(constructor Biquad (i64 S) S :cost 16)
(constructor Mix (S S) S :cost 16)
(constructor Stereo (G G S) S :cost 4)
(constructor Cascade (Q S) S :cost 4)
(constructor GP (i64) G)
(constructor PanL (i64) G)
(constructor PanR (i64) G)
(constructor GMul (G G) G)
(constructor GAdd (G G) G)
(constructor Coef (i64) C)
(constructor CScale (G C) C)
(constructor SNil () Q)
(constructor SCons (C Q) Q :cost 2)
(constructor QApp (Q Q) Q :cost 8)
; gain-stereo / pan-stereo: per-channel gain stages.
(rewrite (Gain g s) (Stereo (GP g) (GP g) s))
(rewrite (Pan p s) (Stereo (PanL p) (PanR p) s))
; stereo-compose: two gain stages are one.
(rewrite (Stereo a b (Stereo c d s)) (Stereo (GMul a c) (GMul b d) s))
; biquad-cascade / cascade-merge: sections run sample by sample in one loop.
(rewrite (Biquad c s) (Cascade (SCons (Coef c) (SNil)) s))
(rewrite (Cascade q2 (Cascade q1 s)) (Cascade (QApp q1 q2) s))
; cascade-append-nil / cascade-append-cons.
(rewrite (QApp (SNil) q) q)
(rewrite (QApp (SCons c r) q) (SCons c (QApp r q)))
; gain-into-biquad / gain-before-biquad: a uniform gain scales b0, b1, b2
; of the first section (zero initial state).
(rewrite (Stereo g g (Cascade (SCons c r) s)) (Cascade (SCons (CScale g c) r) s))
(rewrite (Cascade (SCons c r) (Stereo g g s)) (Cascade (SCons (CScale g c) r) s))
; mix-sends: two gain sends of one source mix into one gain stage.
(rewrite (Mix (Stereo a b s) (Stereo c d s)) (Stereo (GAdd a c) (GAdd b d) s))
"#;

/// Normalized biquad coefficients (`a0 = 1`), transposed direct form II:
/// `y = b0 x + s1; s1 = b1 x - a1 y + s2; s2 = b2 x - a2 y`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biquad {
    /// Feed-forward coefficients.
    pub b: [f32; 3],
    /// Feedback coefficients `a1`, `a2`.
    pub a: [f32; 2],
}

impl Biquad {
    /// Validates finiteness and the stability triangle
    /// `|a2| < 1`, `|a1| < 1 + a2` (both poles strictly inside the unit circle).
    pub fn new(b: [f32; 3], a: [f32; 2]) -> Result<Self, Error> {
        if b.iter().chain(&a).any(|v| !v.is_finite()) {
            return Err(Error::NonFinite);
        }
        if !(a[1].abs() < 1.0 && a[0].abs() < 1.0 + a[1]) {
            return Err(Error::Parameter("biquad poles outside the unit circle"));
        }
        Ok(Self { b, a })
    }
    fn scaled(self, g: f32) -> Self {
        Self {
            b: [self.b[0] * g, self.b[1] * g, self.b[2] * g],
            a: self.a,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Section {
    s1: f32,
    s2: f32,
}

impl Section {
    fn tick(&mut self, c: &Biquad, x: f32) -> f32 {
        let y = c.b[0] * x + self.s1;
        self.s1 = c.b[1] * x - c.a[0] * y + self.s2;
        self.s2 = c.b[2] * x - c.a[1] * y;
        y
    }
}

/// A stereo block.
#[derive(Clone, Debug, PartialEq)]
pub struct Stereo {
    /// Left channel.
    pub left: Vec<f32>,
    /// Right channel.
    pub right: Vec<f32>,
}

/// Named stereo sources.
#[derive(Clone, Debug, Default)]
pub struct DspEnv {
    sources: HashMap<String, Stereo>,
}

impl DspEnv {
    /// An empty environment.
    pub fn new() -> Self {
        Self::default()
    }
    /// Binds a source block.
    pub fn source(&mut self, name: &str, block: Stereo) -> &mut Self {
        self.sources.insert(name.to_owned(), block);
        self
    }
}

/// A node handle of one [`DspGraph`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Signal {
    owner: u64,
    index: u32,
}

#[derive(Clone, Debug, PartialEq)]
enum Op {
    Source(String),
    Gain(usize, usize),
    Pan(usize, usize),
    Biquad(usize, usize),
    Mix(usize, usize),
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Params {
    gains: Vec<f32>,
    pans: Vec<f32>,
    sections: Vec<Biquad>,
}

/// Constant-power pan law: position `p` in `[-1, 1]` gives
/// `(cos t, sin t)` with `t = (p + 1) pi / 4`.
pub fn pan_gains(p: f32) -> (f32, f32) {
    let t = (p + 1.0) * std::f32::consts::FRAC_PI_4;
    (t.cos(), t.sin())
}

/// A stereo DSP graph.
#[derive(Debug)]
pub struct DspGraph {
    owner: u64,
    nodes: Vec<Op>,
    params: Params,
}

/// A clone is a new graph: it gets a fresh owner tag, so handles of the
/// original are `InvalidNode` on the copy once the two can diverge.
impl Clone for DspGraph {
    fn clone(&self) -> Self {
        Self {
            owner: next_owner(),
            nodes: self.nodes.clone(),
            params: self.params.clone(),
        }
    }
}

impl Default for DspGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl DspGraph {
    /// An empty graph.
    pub fn new() -> Self {
        Self {
            owner: next_owner(),
            nodes: Vec::new(),
            params: Params::default(),
        }
    }

    fn index(&self, s: Signal) -> Result<usize, Error> {
        if s.owner != self.owner || s.index as usize >= self.nodes.len() {
            return Err(Error::InvalidNode);
        }
        Ok(s.index as usize)
    }

    fn push(&mut self, op: Op) -> Result<Signal, Error> {
        if self.nodes.len() >= MAX_NODES {
            return Err(Error::Limit("graph nodes"));
        }
        self.nodes.push(op);
        Ok(Signal {
            owner: self.owner,
            index: (self.nodes.len() - 1) as u32,
        })
    }

    fn param<T>(list: &mut Vec<T>, v: T) -> Result<usize, Error> {
        if list.len() >= MAX_NODES {
            return Err(Error::Limit("graph parameters"));
        }
        list.push(v);
        Ok(list.len() - 1)
    }

    /// A named source.
    pub fn source(&mut self, name: &str) -> Result<Signal, Error> {
        valid_name(name)?;
        if self
            .nodes
            .iter()
            .any(|n| matches!(n, Op::Source(s) if s == name))
        {
            return Err(Error::InvalidName(name.to_owned()));
        }
        self.push(Op::Source(name.to_owned()))
    }

    /// Both channels times `g`.
    pub fn gain(&mut self, g: f32, s: Signal) -> Result<Signal, Error> {
        let s = self.index(s)?;
        if !g.is_finite() {
            return Err(Error::NonFinite);
        }
        let p = Self::param(&mut self.params.gains, g)?;
        self.push(Op::Gain(p, s))
    }

    /// Left times `pan_gains(p).0`, right times `pan_gains(p).1`.
    pub fn pan(&mut self, position: f32, s: Signal) -> Result<Signal, Error> {
        let s = self.index(s)?;
        if !(-1.0..=1.0).contains(&position) {
            return Err(Error::Parameter("pan position outside [-1, 1]"));
        }
        let p = Self::param(&mut self.params.pans, position)?;
        self.push(Op::Pan(p, s))
    }

    /// One biquad section on each channel (zero initial state).
    pub fn biquad(&mut self, c: Biquad, s: Signal) -> Result<Signal, Error> {
        let s = self.index(s)?;
        Biquad::new(c.b, c.a)?;
        let p = Self::param(&mut self.params.sections, c)?;
        self.push(Op::Biquad(p, s))
    }

    /// Channel-wise `a + b`.
    pub fn mix(&mut self, a: Signal, b: Signal) -> Result<Signal, Error> {
        let (a, b) = (self.index(a)?, self.index(b)?);
        self.push(Op::Mix(a, b))
    }

    fn reachable(&self, root: usize) -> Vec<bool> {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            match self.nodes[i] {
                Op::Source(_) => {}
                Op::Gain(_, s) | Op::Pan(_, s) | Op::Biquad(_, s) => stack.push(s),
                Op::Mix(a, b) => stack.extend([a, b]),
            }
        }
        seen
    }

    fn bindings(&self, root: usize) -> String {
        let live = self.reachable(root);
        let mut out = String::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if !live[i] {
                continue;
            }
            let body = match n {
                Op::Source(name) => format!("(Src \"{name}\")"),
                Op::Gain(p, s) => format!("(Gain {p} s{s})"),
                Op::Pan(p, s) => format!("(Pan {p} s{s})"),
                Op::Biquad(p, s) => format!("(Biquad {p} s{s})"),
                Op::Mix(a, b) => format!("(Mix s{a} s{b})"),
            };
            let _ = writeln!(out, "(let s{i} {body})");
        }
        out
    }

    /// Saturates the graph rooted at `root` with [`RULES`] and extracts the
    /// cheapest equivalent term.
    pub fn fuse(&self, root: Signal) -> Result<FusedDsp, Error> {
        let r = self.index(root)?;
        let bindings = self.bindings(r);
        let before = engine::run(RULES, &bindings, &format!("s{r}"), false)?;
        let term = engine::run(RULES, &bindings, &format!("s{r}"), true)?;
        let live = self.reachable(r);
        let fused = FusedDsp {
            params: self.params.clone(),
            unfused_cost: before.cost,
            unfused_passes: live
                .iter()
                .zip(&self.nodes)
                .filter(|(l, n)| **l && !matches!(n, Op::Source(_)))
                .count(),
            term,
        };
        fused.plan()?;
        Ok(fused)
    }

    /// Unfused reference evaluation of `root`.
    pub fn evaluate(&self, root: Signal, env: &DspEnv) -> Result<Stereo, Error> {
        let r = self.index(root)?;
        let live = self.reachable(r);
        let mut values: Vec<Option<Stereo>> = vec![None; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            if !live[i] {
                continue;
            }
            let get = |j: usize| values[j].as_ref().expect("operands precede nodes");
            let v = match n {
                Op::Source(name) => source(env, name)?,
                Op::Gain(p, s) => {
                    let g = self.params.gains[*p];
                    map(get(*s), |l, r| (g * l, g * r))?
                }
                Op::Pan(p, s) => {
                    let (gl, gr) = pan_gains(self.params.pans[*p]);
                    map(get(*s), |l, r| (gl * l, gr * r))?
                }
                Op::Biquad(p, s) => {
                    let c = self.params.sections[*p];
                    let (mut left, mut right) = (Section::default(), Section::default());
                    map(get(*s), |l, r| (left.tick(&c, l), right.tick(&c, r)))?
                }
                Op::Mix(a, b) => {
                    let (a, b) = (get(*a), get(*b));
                    if a.left.len() != b.left.len() {
                        return Err(Error::Shape("mix block lengths"));
                    }
                    let left = a.left.iter().zip(&b.left).map(|(x, y)| x + y).collect();
                    let right = a.right.iter().zip(&b.right).map(|(x, y)| x + y).collect();
                    finite(Stereo { left, right })?
                }
            };
            values[i] = Some(v);
        }
        Ok(values[r].take().expect("root evaluated"))
    }

    /// Delay-compensated Resonance schedule of the unfused graph: one PDC node
    /// (and buffer) per reachable stage.
    pub fn schedule(&self, root: Signal) -> Result<pdc::Schedule, Error> {
        let r = self.index(root)?;
        let live = self.reachable(r);
        let specs = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| live[*i])
            .map(|(i, n)| NodeSpec {
                id: NodeId(i as u32),
                latency: 0,
                inputs: match *n {
                    Op::Source(_) => vec![],
                    Op::Gain(_, s) | Op::Pan(_, s) | Op::Biquad(_, s) => {
                        vec![Input::Forward(NodeId(s as u32))]
                    }
                    Op::Mix(a, b) => vec![
                        Input::Forward(NodeId(a as u32)),
                        Input::Forward(NodeId(b as u32)),
                    ],
                },
            })
            .collect::<Vec<_>>();
        pdc::compile_schedule(&specs).map_err(Error::Schedule)
    }
}

fn source(env: &DspEnv, name: &str) -> Result<Stereo, Error> {
    let s = env
        .sources
        .get(name)
        .ok_or_else(|| Error::Missing(name.to_owned()))?;
    if s.left.len() != s.right.len() || s.left.is_empty() || s.left.len() > MAX_ELEMENTS {
        return Err(Error::Shape("source channel lengths"));
    }
    finite(s.clone())
}

fn finite(s: Stereo) -> Result<Stereo, Error> {
    if s.left.iter().chain(&s.right).any(|v| !v.is_finite()) {
        return Err(Error::NonFinite);
    }
    Ok(s)
}

fn map(s: &Stereo, mut f: impl FnMut(f32, f32) -> (f32, f32)) -> Result<Stereo, Error> {
    let (left, right) = s.left.iter().zip(&s.right).map(|(l, r)| f(*l, *r)).unzip();
    finite(Stereo { left, right })
}

/// One per-sample stage of a fused loop.
#[derive(Clone, Debug, PartialEq)]
enum Stage {
    /// Left and right multipliers, each list applied in order.
    Gains(Vec<f32>, Vec<f32>),
    Cascade(Vec<Biquad>),
}

/// How the fused term computes one materialized buffer.
#[derive(Clone, Debug, PartialEq)]
enum Pass {
    Source(String),
    Mix(usize, usize),
    /// Stages applied innermost first to the buffer of node `base`.
    Chain {
        base: usize,
        stages: Vec<Stage>,
    },
}

/// Fusion statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// egglog cost of the input graph.
    pub cost_before: u64,
    /// egglog cost of the extracted term.
    pub cost_after: u64,
    /// Buffer-writing passes of the unfused graph (sources excluded).
    pub passes_before: usize,
    /// Buffer-writing passes of the fused term (sources excluded).
    pub passes_after: usize,
}

/// The extracted fused term of a [`DspGraph`].
#[derive(Clone, Debug)]
pub struct FusedDsp {
    params: Params,
    unfused_cost: u64,
    unfused_passes: usize,
    term: Extracted,
}

impl FusedDsp {
    /// The fused term as an s-expression.
    pub fn term(&self) -> &str {
        &self.term.text
    }

    /// Number of `name` constructor nodes in the fused term.
    pub fn count(&self, name: &str) -> usize {
        self.term.count(name)
    }

    /// Fusion statistics.
    pub fn report(&self) -> Result<Report, Error> {
        let plan = self.plan()?;
        Ok(Report {
            cost_before: self.unfused_cost,
            cost_after: self.term.cost,
            passes_before: self.unfused_passes,
            passes_after: plan
                .values()
                .filter(|p| !matches!(p, Pass::Source(_)))
                .count(),
        })
    }

    /// The multipliers a gain expression applies, in order. A composed
    /// gain is one multiplier when its product stays in the normal FP32
    /// range; otherwise the stages stay separate, so composition never turns
    /// a finite unfused result into an overflow or a flush to zero.
    fn gain(&self, g: usize) -> Result<Vec<f32>, Error> {
        let t = &self.term;
        let (head, kids) = t.app(g)?;
        let idx = |k: usize| -> Result<usize, Error> {
            usize::try_from(t.int(k)?).map_err(|_| Error::Extraction("negative parameter".into()))
        };
        let missing = || Error::Extraction(format!("unknown {head} parameter"));
        Ok(match head {
            "GP" => vec![*self.params.gains.get(idx(kids[0])?).ok_or_else(missing)?],
            "PanL" => vec![pan_gains(*self.params.pans.get(idx(kids[0])?).ok_or_else(missing)?).0],
            "PanR" => vec![pan_gains(*self.params.pans.get(idx(kids[0])?).ok_or_else(missing)?).1],
            "GMul" => {
                // `(GMul outer inner)`: the inner gain is applied first.
                let (outer, inner) = (self.gain(kids[0])?, self.gain(kids[1])?);
                match (outer.as_slice(), inner.as_slice()) {
                    ([a], [c]) if in_range(a * c, &[*a, *c]) => vec![a * c],
                    _ => inner.into_iter().chain(outer).collect(),
                }
            }
            "GAdd" => match (
                self.gain(kids[0])?.as_slice(),
                self.gain(kids[1])?.as_slice(),
            ) {
                ([a], [c]) if (a + c).is_finite() => vec![a + c],
                _ => {
                    return Err(Error::Unsupported(
                        "merged send gains outside the FP32 range",
                    ));
                }
            },
            other => return Err(Error::Extraction(format!("{other} is not a gain"))),
        })
    }

    /// A section and any gain that could not be folded into it; that gain is
    /// applied before the cascade instead (equivalent from zero state).
    fn coef(&self, c: usize) -> Result<(Biquad, Vec<f32>), Error> {
        let t = &self.term;
        let (head, kids) = t.app(c)?;
        match head {
            "Coef" => {
                let i = usize::try_from(t.int(kids[0])?)
                    .map_err(|_| Error::Extraction("negative section".into()))?;
                let b = self
                    .params
                    .sections
                    .get(i)
                    .copied()
                    .ok_or_else(|| Error::Extraction("unknown section".into()))?;
                Ok((b, Vec::new()))
            }
            "CScale" => {
                let (b, mut pre) = self.coef(kids[1])?;
                let g = self.gain(kids[0])?;
                if let ([g], true) = (g.as_slice(), pre.is_empty()) {
                    let scaled = b.scaled(*g);
                    if scaled
                        .b
                        .iter()
                        .zip(&b.b)
                        .all(|(s, b)| in_range(*s, &[*b, *g]))
                    {
                        return Ok((scaled, pre));
                    }
                }
                pre.extend(g);
                Ok((b, pre))
            }
            other => Err(Error::Extraction(format!("{other} is not a section"))),
        }
    }

    fn sections(&self, mut q: usize) -> Result<(Vec<Biquad>, Vec<f32>), Error> {
        let (mut out, mut pre) = (Vec::new(), Vec::new());
        loop {
            let (head, kids) = self.term.app(q)?;
            match head {
                "SNil" => return Ok((out, pre)),
                "SCons" => {
                    let (b, p) = self.coef(kids[0])?;
                    out.push(b);
                    pre.extend(p);
                    q = kids[1];
                }
                // QApp never survives extraction (the appended list is
                // cheaper); reject rather than guess.
                other => return Err(Error::Extraction(format!("{other} is not a section list"))),
            }
        }
    }

    /// Splits the term into materialized passes keyed by arena node.
    fn plan(&self) -> Result<std::collections::BTreeMap<usize, Pass>, Error> {
        let mut plan = std::collections::BTreeMap::new();
        let mut todo = vec![self.term.root];
        while let Some(id) = todo.pop() {
            if plan.contains_key(&id) {
                continue;
            }
            let mut stages = Vec::new();
            let mut cur = id;
            let pass = loop {
                let (head, kids) = self.term.app(cur)?;
                match head {
                    "Src" => break Pass::Source(self.term.str(kids[0])?.to_owned()),
                    "Mix" => break Pass::Mix(kids[0], kids[1]),
                    "Stereo" => {
                        stages.push(Stage::Gains(self.gain(kids[0])?, self.gain(kids[1])?));
                        cur = kids[2];
                    }
                    "Cascade" => {
                        // Stages are collected outermost first, so the
                        // unfolded gain (pushed after) runs before the cascade.
                        let (sections, pre) = self.sections(kids[0])?;
                        stages.push(Stage::Cascade(sections));
                        if !pre.is_empty() {
                            stages.push(Stage::Gains(pre.clone(), pre));
                        }
                        cur = kids[1];
                    }
                    "Gain" | "Pan" | "Biquad" => {
                        let p = usize::try_from(self.term.int(kids[0])?)
                            .map_err(|_| Error::Extraction("negative parameter".into()))?;
                        let missing = || Error::Extraction(format!("unknown {head} parameter"));
                        stages.push(match head {
                            "Gain" => {
                                let g = *self.params.gains.get(p).ok_or_else(missing)?;
                                Stage::Gains(vec![g], vec![g])
                            }
                            "Pan" => {
                                let (l, r) =
                                    pan_gains(*self.params.pans.get(p).ok_or_else(missing)?);
                                Stage::Gains(vec![l], vec![r])
                            }
                            _ => Stage::Cascade(vec![
                                *self.params.sections.get(p).ok_or_else(missing)?,
                            ]),
                        });
                        cur = kids[1];
                    }
                    other => return Err(Error::Extraction(format!("{other} is not a signal"))),
                }
            };
            if stages.is_empty() {
                if let Pass::Mix(a, b) = pass {
                    todo.extend([a, b]);
                }
                plan.insert(id, pass);
            } else {
                stages.reverse();
                todo.push(cur);
                plan.insert(id, Pass::Chain { base: cur, stages });
            }
        }
        Ok(plan)
    }

    /// Evaluates the fused term.
    pub fn evaluate(&self, env: &DspEnv) -> Result<Stereo, Error> {
        let plan = self.plan()?;
        let mut done: HashMap<usize, Stereo> = HashMap::new();
        let mut stack = vec![(self.term.root, false)];
        while let Some((id, ready)) = stack.pop() {
            if done.contains_key(&id) {
                continue;
            }
            let pass = &plan[&id];
            let deps: Vec<usize> = match pass {
                Pass::Source(_) => vec![],
                Pass::Mix(a, b) => vec![*a, *b],
                Pass::Chain { base, .. } => vec![*base],
            };
            if !ready {
                stack.push((id, true));
                stack.extend(
                    deps.iter()
                        .filter(|d| !done.contains_key(d))
                        .map(|d| (*d, false)),
                );
                continue;
            }
            let out = match pass {
                Pass::Source(name) => source(env, name)?,
                Pass::Mix(a, b) => {
                    let (a, b) = (&done[a], &done[b]);
                    if a.left.len() != b.left.len() {
                        return Err(Error::Shape("mix block lengths"));
                    }
                    let left = a.left.iter().zip(&b.left).map(|(x, y)| x + y).collect();
                    let right = a.right.iter().zip(&b.right).map(|(x, y)| x + y).collect();
                    finite(Stereo { left, right })?
                }
                Pass::Chain { base, stages } => run_chain(&done[base], stages)?,
            };
            done.insert(id, out);
        }
        Ok(done.remove(&self.term.root).expect("root evaluated"))
    }

    /// Delay-compensated Resonance schedule of the fused term: one PDC node
    /// (and buffer) per materialized pass.
    pub fn schedule(&self) -> Result<pdc::Schedule, Error> {
        let plan = self.plan()?;
        let specs = plan
            .iter()
            .map(|(id, pass)| NodeSpec {
                id: NodeId(*id as u32),
                latency: 0,
                inputs: match pass {
                    Pass::Source(_) => vec![],
                    Pass::Mix(a, b) => vec![
                        Input::Forward(NodeId(*a as u32)),
                        Input::Forward(NodeId(*b as u32)),
                    ],
                    Pass::Chain { base, .. } => vec![Input::Forward(NodeId(*base as u32))],
                },
            })
            .collect::<Vec<_>>();
        pdc::compile_schedule(&specs).map_err(Error::Schedule)
    }
}

/// A product of nonzero factors is usable when it is finite and normal (a
/// zero factor gives an exact zero).
fn in_range(p: f32, factors: &[f32]) -> bool {
    p.is_finite() && (p.is_normal() || (p == 0.0 && factors.contains(&0.0)))
}

/// One loop over the block; each sample passes through every stage before
/// the next sample is read.
fn run_chain(input: &Stereo, stages: &[Stage]) -> Result<Stereo, Error> {
    let mut state: Vec<Vec<(Section, Section)>> = stages
        .iter()
        .map(|s| match s {
            Stage::Cascade(cs) => vec![(Section::default(), Section::default()); cs.len()],
            Stage::Gains(..) => Vec::new(),
        })
        .collect();
    let n = input.left.len();
    let (mut left, mut right) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for t in 0..n {
        let (mut l, mut r) = (input.left[t], input.right[t]);
        for (stage, state) in stages.iter().zip(&mut state) {
            match stage {
                Stage::Gains(gl, gr) => {
                    for g in gl {
                        l *= g;
                    }
                    for g in gr {
                        r *= g;
                    }
                }
                Stage::Cascade(cs) => {
                    for (c, (sl, sr)) in cs.iter().zip(state.iter_mut()) {
                        l = sl.tick(c, l);
                        r = sr.tick(c, r);
                    }
                }
            }
            if !(l.is_finite() && r.is_finite()) {
                return Err(Error::NonFinite);
            }
        }
        left.push(l);
        right.push(r);
    }
    Ok(Stereo { left, right })
}

/// Tolerance for chains of gain, pan and mix stages only: with `k` rounded
/// products/sums on the longest path, `|fused - unfused| <= 2 k u M` where
/// `M` bounds the magnitudes of the partial products (`u = 2^-24`). The
/// tests pass `M` as the sum of absolute contributions to the sample.
pub fn gain_tolerance(k: usize, magnitude: f32) -> f32 {
    2.0 * k as f32 * (f32::EPSILON / 2.0) * magnitude
}

/// Engineering tolerance for chains containing biquads whose gains were
/// folded into coefficients: `1e-4 * max(1, peak)`, where `peak` is the
/// largest unfused output magnitude. Rounding differences enter the
/// recursive state and are amplified by the filter's noise gain, so this is
/// stated for stable sections with pole radius at most 0.99 and blocks of at
/// most 4096 samples, the regime the tests exercise, not derived in general.
pub fn filter_tolerance(peak: f32) -> f32 {
    1e-4 * peak.max(1.0)
}
