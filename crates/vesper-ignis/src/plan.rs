//! Hazard analysis, scheduling, and barrier/fence planning.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt::{self, Write as _};

use crate::d3d12::{Access, Layout, QueueKind, Sync, sync_union};
use crate::graph::{
    BufferHandle, PassId, QueueId, RenderGraph, ResourceKind, ResourceRef, SubresourceRange,
    TextureDims, TextureHandle, UseRange, overlaps,
};

/// Planner options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanOptions {
    /// Emit split barriers (`SyncAfter = SPLIT` after the producer,
    /// `SyncBefore = SPLIT` before the consumer) when other passes on the same
    /// queue run between the last access and the next one.
    pub split_barriers: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions {
            split_barriers: true,
        }
    }
}

/// Why one pass must run after another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DependencyKind {
    ReadAfterWrite,
    WriteAfterRead,
    WriteAfterWrite,
    /// Two overlapping reads whose layouts are not both explicit and equal.
    /// They are kept in declaration order because a layout transition may be
    /// needed between them.
    ReadOrder,
    /// Added with [`RenderGraph::add_dependency`].
    Explicit,
}

/// An ordering edge derived from the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dependency {
    pub before: PassId,
    pub after: PassId,
    pub kind: DependencyKind,
    pub resource: Option<ResourceRef>,
}

/// `D3D12_BUFFER_BARRIER`. Enhanced barriers require `Offset = 0` and
/// `Size = UINT64_MAX` (or the buffer size), so buffer barriers always cover
/// the whole buffer even though hazards are tracked per byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferBarrier {
    pub resource: BufferHandle,
    pub sync_before: Sync,
    pub sync_after: Sync,
    pub access_before: Access,
    pub access_after: Access,
}

/// `D3D12_TEXTURE_BARRIER` for one box of subresources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureBarrier {
    pub resource: TextureHandle,
    pub sync_before: Sync,
    pub sync_after: Sync,
    pub access_before: Access,
    pub access_after: Access,
    pub layout_before: Layout,
    pub layout_after: Layout,
    pub subresources: SubresourceRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Barrier {
    Buffer(BufferBarrier),
    Texture(TextureBarrier),
}

/// One step of a queue's command stream. Every `Wait` and `Signal` is an
/// `ID3D12CommandQueue::Wait`/`Signal` on the queue timeline fences, and so
/// implies an `ExecuteCommandLists` boundary; how passes between those
/// boundaries are grouped into command lists is left to the executor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Wait until `queue`'s fence reaches `value`.
    Wait {
        queue: QueueId,
        value: u64,
    },
    /// One `ID3D12GraphicsCommandList7::Barrier` batch.
    Barriers(Vec<Barrier>),
    Execute(PassId),
    /// Signal this queue's fence with `value` (strictly increasing per queue).
    Signal {
        value: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueStream {
    pub queue: QueueId,
    pub kind: QueueKind,
    pub commands: Vec<Command>,
}

/// A deterministic barrier/fence plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// A global topological order of the passes, consistent with every
    /// queue's stream.
    pub order: Vec<PassId>,
    pub dependencies: Vec<Dependency>,
    pub queues: Vec<QueueStream>,
}

/// Structured planning errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// The ordering constraints form a cycle. `passes[i]` must run before
    /// `passes[i + 1]`, and the last before the first.
    Cycle { passes: Vec<PassId> },
    /// No layout supported on the pass's queue admits the requested access.
    NoLayoutForAccess {
        pass: PassId,
        resource: TextureHandle,
        subresource: u32,
        access: Access,
    },
    /// The pass's queue cannot transition the subresource out of its current
    /// layout, and no single other queue that last used it can do so first.
    UnsupportedQueueHandoff {
        pass: PassId,
        resource: TextureHandle,
        subresource: u32,
        from: Layout,
        to: Layout,
    },
    /// No queue can perform the requested final layout transition.
    FinalLayoutUnreachable {
        resource: TextureHandle,
        subresource: u32,
        from: Layout,
        to: Layout,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::Cycle { passes } => {
                let ids: Vec<String> = passes.iter().map(|p| p.index().to_string()).collect();
                write!(f, "ordering cycle through passes {}", ids.join(" -> "))
            }
            PlanError::NoLayoutForAccess {
                pass,
                subresource,
                access,
                ..
            } => write!(
                f,
                "pass {}: no layout on its queue admits access {access} (subresource {subresource})",
                pass.index()
            ),
            PlanError::UnsupportedQueueHandoff {
                pass,
                subresource,
                from,
                to,
                ..
            } => write!(
                f,
                "pass {}: cannot transition subresource {subresource} from {from} to {to} on any single queue",
                pass.index()
            ),
            PlanError::FinalLayoutUnreachable {
                subresource,
                from,
                to,
                ..
            } => {
                write!(
                    f,
                    "no queue can transition subresource {subresource} from {from} to {to}"
                )
            }
        }
    }
}

impl std::error::Error for PlanError {}

// ---------------------------------------------------------------------------
// Internal state

/// One access recorded against a subresource or byte segment.
#[derive(Clone, Copy, Debug)]
struct Rec {
    pass: u32,
    queue: u16,
    sync: Sync,
    access: Access,
}

/// Which later accesses on a queue are already ordered after `last_write`.
#[derive(Clone, Copy, Debug)]
enum Covered {
    /// A barrier with this after-scope already followed the write.
    Scope(Sync, Access),
    /// A fence wait followed the write; all later work on the queue is
    /// ordered after it.
    Full,
}

#[derive(Clone, Debug)]
struct SubState {
    layout: Option<Layout>,
    last_write: Option<Rec>,
    reads: Vec<Rec>,
    covered: Vec<(u16, Covered)>,
}

impl SubState {
    fn new(layout: Option<Layout>) -> Self {
        SubState {
            layout,
            last_write: None,
            reads: Vec::new(),
            covered: Vec::new(),
        }
    }

    fn covered_on(&self, queue: u16) -> Option<Covered> {
        self.covered
            .iter()
            .find(|(q, _)| *q == queue)
            .map(|(_, c)| *c)
    }

    fn set_covered(&mut self, queue: u16, c: Covered) {
        match self.covered.iter_mut().find(|(q, _)| *q == queue) {
            Some(slot) => slot.1 = c,
            None => self.covered.push((queue, c)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    Pre(u32),
    Post(u32),
    Final(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Placement {
    Single(Slot),
    Split { begin_after: u32, end_before: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TexKey {
    placement: Placement,
    resource: u32,
    sync_before: Sync,
    sync_after: Sync,
    access_before: Access,
    access_after: Access,
    layout_before: Layout,
    layout_after: Layout,
}

#[derive(Clone, Copy, Debug)]
struct BufScope {
    sync_before: Sync,
    sync_after: Sync,
    access_before: Access,
    access_after: Access,
}

/// A pending barrier for one subresource or segment.
#[derive(Clone, Copy, Debug)]
struct Pending {
    placement: Placement,
    sync_before: Sync,
    access_before: Access,
    sync_after: Sync,
    access_after: Access,
    layouts: Option<(Layout, Layout)>,
}

enum ResState {
    Buffer {
        segments: Vec<(u64, u64, SubState)>,
    },
    Texture {
        dims: TextureDims,
        subs: Vec<SubState>,
    },
}

/// Requirement of one pass on one subresource or segment.
#[derive(Clone, Copy, Debug)]
struct Req {
    access: Access,
    sync: Sync,
    layout: Option<Layout>,
}

struct Planner<'g> {
    graph: &'g RenderGraph,
    options: PlanOptions,
    /// Queue-local 1-based position of each pass.
    qpos: Vec<u32>,
    /// Position of each pass in the global order.
    gpos: Vec<u32>,
    /// Last pass on each queue that touched each resource.
    last_touch: Vec<BTreeMap<u16, u32>>,
    tex: BTreeMap<TexKey, BTreeSet<u32>>,
    buf: BTreeMap<(Placement, u32), BufScope>,
    /// Cross-queue dependencies: consumer pass -> producer passes.
    cross: BTreeMap<u32, BTreeSet<u32>>,
    /// End-of-stream waits per queue.
    final_waits: BTreeMap<u16, BTreeSet<u32>>,
}

impl RenderGraph {
    /// Plans with [`PlanOptions::default`].
    pub fn plan(&self) -> Result<Plan, PlanError> {
        self.plan_with(PlanOptions::default())
    }

    /// Orders the passes and computes per-queue barrier batches and fence
    /// signals/waits. The result depends only on the graph contents.
    pub fn plan_with(&self, options: PlanOptions) -> Result<Plan, PlanError> {
        let deps = self.hazard_edges();
        let order = self.topological_order(&deps)?;
        let mut planner = Planner {
            graph: self,
            options,
            qpos: vec![0; self.passes.len()],
            gpos: vec![0; self.passes.len()],
            last_touch: vec![BTreeMap::new(); self.resources.len()],
            tex: BTreeMap::new(),
            buf: BTreeMap::new(),
            cross: BTreeMap::new(),
            final_waits: BTreeMap::new(),
        };
        let mut counts = vec![0u32; self.queues.len()];
        for (g, &p) in order.iter().enumerate() {
            planner.gpos[p as usize] = g as u32;
            let q = self.passes[p as usize].queue as usize;
            counts[q] += 1;
            planner.qpos[p as usize] = counts[q];
        }
        let mut states: Vec<ResState> = self
            .resources
            .iter()
            .map(|r| match r.kind {
                ResourceKind::Buffer { size } => ResState::Buffer {
                    segments: vec![(0, size, SubState::new(None))],
                },
                ResourceKind::Texture {
                    dims,
                    initial_layout,
                    ..
                } => ResState::Texture {
                    dims,
                    subs: vec![SubState::new(Some(initial_layout)); dims.count() as usize],
                },
            })
            .collect();
        for &(before, after) in &self.explicit {
            if self.passes[before as usize].queue != self.passes[after as usize].queue {
                planner.cross.entry(after).or_default().insert(before);
            }
        }
        for &p in &order {
            planner.plan_pass(p, &mut states)?;
        }
        planner.plan_final_layouts(&mut states)?;
        let dependencies = deps
            .iter()
            .map(|&(before, after, kind, resource)| Dependency {
                before: self.pass_id(before),
                after: self.pass_id(after),
                kind,
                resource: resource.map(|r| self.resource_ref(r)),
            })
            .collect();
        Ok(planner.assemble(&order, dependencies))
    }

    pub(crate) fn pass_id(&self, index: u32) -> PassId {
        PassId {
            graph: self.id,
            index,
        }
    }

    fn resource_ref(&self, index: u32) -> ResourceRef {
        match self.resources[index as usize].kind {
            ResourceKind::Buffer { .. } => ResourceRef::Buffer(BufferHandle {
                graph: self.id,
                index,
            }),
            ResourceKind::Texture { .. } => ResourceRef::Texture(TextureHandle {
                graph: self.id,
                index,
            }),
        }
    }

    fn texture_handle(&self, index: u32) -> TextureHandle {
        TextureHandle {
            graph: self.id,
            index,
        }
    }

    fn queue_id(&self, index: u16) -> QueueId {
        QueueId {
            graph: self.id,
            index,
        }
    }

    /// Pairwise hazards in declaration order plus explicit edges, sorted.
    fn hazard_edges(&self) -> Vec<(u32, u32, DependencyKind, Option<u32>)> {
        let mut edges = BTreeSet::new();
        for (j, pj) in self.passes.iter().enumerate() {
            for (i, pi) in self.passes[..j].iter().enumerate() {
                for a in &pi.uses {
                    for b in &pj.uses {
                        if a.resource != b.resource || !overlaps(a.range, b.range) {
                            continue;
                        }
                        let kind = match (a.is_write(), b.is_write()) {
                            (true, true) => DependencyKind::WriteAfterWrite,
                            (true, false) => DependencyKind::ReadAfterWrite,
                            (false, true) => DependencyKind::WriteAfterRead,
                            (false, false) => {
                                let is_buffer = matches!(a.range, UseRange::Bytes { .. });
                                if is_buffer || (a.layout.is_some() && a.layout == b.layout) {
                                    continue;
                                }
                                DependencyKind::ReadOrder
                            }
                        };
                        edges.insert((i as u32, j as u32, kind, Some(a.resource)));
                    }
                }
            }
        }
        for &(before, after) in &self.explicit {
            edges.insert((before, after, DependencyKind::Explicit, None));
        }
        edges.into_iter().collect()
    }

    /// Kahn's algorithm, always taking the lowest declared index that is
    /// ready, so the order is deterministic and equals declaration order
    /// unless explicit edges require otherwise.
    fn topological_order(
        &self,
        deps: &[(u32, u32, DependencyKind, Option<u32>)],
    ) -> Result<Vec<u32>, PlanError> {
        let n = self.passes.len();
        let mut succ: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); n];
        let mut pred: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); n];
        for &(a, b, _, _) in deps {
            succ[a as usize].insert(b);
            pred[b as usize].insert(a);
        }
        let mut indegree: Vec<usize> = pred.iter().map(BTreeSet::len).collect();
        let mut ready: BinaryHeap<Reverse<u32>> = (0..n as u32)
            .filter(|&p| indegree[p as usize] == 0)
            .map(Reverse)
            .collect();
        let mut order = Vec::with_capacity(n);
        while let Some(Reverse(p)) = ready.pop() {
            order.push(p);
            for &s in &succ[p as usize] {
                indegree[s as usize] -= 1;
                if indegree[s as usize] == 0 {
                    ready.push(Reverse(s));
                }
            }
        }
        if order.len() == n {
            return Ok(order);
        }
        // Every remaining node has a remaining predecessor; walk the smallest
        // ones until a node repeats.
        let remaining: BTreeSet<u32> = (0..n as u32)
            .filter(|&p| indegree[p as usize] > 0)
            .collect();
        let mut walk = vec![*remaining.first().expect("cycle has nodes")];
        loop {
            let cur = *walk.last().expect("walk is non-empty");
            let next = *pred[cur as usize]
                .iter()
                .find(|p| remaining.contains(p))
                .expect("remaining node has a remaining predecessor");
            if let Some(pos) = walk.iter().position(|&w| w == next) {
                let mut cycle: Vec<u32> = walk[pos..].to_vec();
                cycle.reverse();
                let min = cycle
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, p)| **p)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                cycle.rotate_left(min);
                return Err(PlanError::Cycle {
                    passes: cycle.into_iter().map(|p| self.pass_id(p)).collect(),
                });
            }
            walk.push(next);
        }
    }
}

impl Planner<'_> {
    fn queue_kind(&self, queue: u16) -> QueueKind {
        self.graph.queues[queue as usize]
    }

    fn plan_pass(&mut self, p: u32, states: &mut [ResState]) -> Result<(), PlanError> {
        let graph = self.graph;
        let pass = &graph.passes[p as usize];
        let queue = pass.queue;
        let touched: BTreeSet<u32> = pass.uses.iter().map(|u| u.resource).collect();
        for &resource in &touched {
            let uses: Vec<_> = pass
                .uses
                .iter()
                .filter(|u| u.resource == resource)
                .collect();
            match &mut states[resource as usize] {
                ResState::Texture { dims, subs } => {
                    let dims = *dims;
                    let mut reqs: BTreeMap<u32, Req> = BTreeMap::new();
                    for u in &uses {
                        let UseRange::Subresources(range) = u.range else {
                            unreachable!()
                        };
                        for s in range.indices(dims) {
                            let r = reqs.entry(s).or_insert(Req {
                                access: u.access,
                                sync: u.sync,
                                layout: u.layout,
                            });
                            r.access |= u.access;
                            r.sync |= u.sync;
                            r.layout = r.layout.or(u.layout);
                        }
                    }
                    for (s, req) in reqs {
                        let mut st = std::mem::replace(&mut subs[s as usize], SubState::new(None));
                        let result = self.process(p, queue, resource, Some(s), &mut st, req);
                        subs[s as usize] = st;
                        result?;
                    }
                }
                ResState::Buffer { segments } => {
                    for u in &uses {
                        let UseRange::Bytes { start, end } = u.range else {
                            unreachable!()
                        };
                        split_segments(segments, start);
                        split_segments(segments, end);
                    }
                    let mut segs = std::mem::take(segments);
                    let mut result = Ok(());
                    for (start, end, st) in &mut segs {
                        let mut req: Option<Req> = None;
                        for u in &uses {
                            let UseRange::Bytes { start: us, end: ue } = u.range else {
                                unreachable!()
                            };
                            if us < *end && *start < ue {
                                let r = req.get_or_insert(Req {
                                    access: u.access,
                                    sync: u.sync,
                                    layout: None,
                                });
                                r.access |= u.access;
                                r.sync |= u.sync;
                            }
                        }
                        if let Some(req) = req {
                            result = result.and(self.process(p, queue, resource, None, st, req));
                        }
                    }
                    *segments = segs;
                    result?;
                }
            }
            self.last_touch[resource as usize].insert(queue, p);
        }
        Ok(())
    }

    /// Advances one subresource/segment state for one requirement.
    fn process(
        &mut self,
        p: u32,
        queue: u16,
        resource: u32,
        sub: Option<u32>,
        st: &mut SubState,
        req: Req,
    ) -> Result<(), PlanError> {
        let kind = self.queue_kind(queue);
        let target = match st.layout {
            None => None,
            Some(cur) => Some(match req.layout {
                Some(l) => l,
                None if cur != Layout::Undefined
                    && cur.allows(req.access)
                    && kind.supports_layout(cur) =>
                {
                    cur
                }
                None => Layout::AUTO_CANDIDATES
                    .into_iter()
                    .chain([Layout::Common])
                    .find(|l| l.allows(req.access) && kind.supports_layout(*l))
                    .ok_or(PlanError::NoLayoutForAccess {
                        pass: self.graph.pass_id(p),
                        resource: self.graph.texture_handle(resource),
                        subresource: sub.unwrap_or(0),
                        access: req.access,
                    })?,
            }),
        };

        // A queue that cannot leave the current layout needs the queue that
        // last used the subresource to transition it first.
        if let (Some(cur), Some(target)) = (st.layout, target)
            && cur != target
            && !kind.can_transition(cur, target)
        {
            self.handoff(p, queue, resource, sub.unwrap_or(0), st, cur, target)?;
        }

        let needs_transition = matches!((st.layout, target), (Some(c), Some(t)) if c != t);
        let is_write = req.access.is_write() || needs_transition;

        let mut same: Vec<Rec> = Vec::new();
        let mut cross: Vec<u32> = Vec::new();
        if let Some(w) = st.last_write {
            if w.queue == queue {
                let covered = match st.covered_on(queue) {
                    Some(Covered::Full) => true,
                    Some(Covered::Scope(s, a)) => s.covers(req.sync) && a.contains(req.access),
                    None => false,
                };
                if is_write || !covered {
                    same.push(w);
                }
            } else if is_write || !matches!(st.covered_on(queue), Some(Covered::Full)) {
                cross.push(w.pass);
            }
        }
        if is_write {
            for r in &st.reads {
                if r.queue == queue {
                    same.push(*r);
                } else {
                    cross.push(r.pass);
                }
            }
        }

        let barrier = !same.is_empty() || needs_transition;
        if barrier {
            let sync_before = same
                .iter()
                .fold(Sync::NONE, |acc, r| sync_union(acc, r.sync));
            let access_before = same
                .iter()
                .fold(Access::NO_ACCESS, |acc, r| acc.union_nonempty(r.access));
            let placement = self.placement(p, queue, resource, !same.is_empty());
            self.push_pending(
                resource,
                sub,
                Pending {
                    placement,
                    sync_before,
                    access_before,
                    sync_after: req.sync,
                    access_after: req.access,
                    layouts: st.layout.zip(target),
                },
            );
        }
        if !cross.is_empty() {
            self.cross
                .entry(p)
                .or_default()
                .extend(cross.iter().copied());
        }

        let rec = Rec {
            pass: p,
            queue,
            sync: req.sync,
            access: req.access,
        };
        if is_write {
            st.last_write = Some(rec);
            st.reads.clear();
            st.covered.clear();
            if !req.access.is_write() {
                // A layout transition for a read: the transition barrier's
                // after-scope already orders this scope.
                st.set_covered(queue, Covered::Scope(req.sync, req.access));
            }
            st.layout = target;
        } else {
            if !cross.is_empty() {
                st.set_covered(queue, Covered::Full);
            } else if barrier {
                let merged = match st.covered_on(queue) {
                    Some(Covered::Scope(s, a)) => Covered::Scope(s | req.sync, a | req.access),
                    Some(Covered::Full) => Covered::Full,
                    None => Covered::Scope(req.sync, req.access),
                };
                st.set_covered(queue, merged);
            }
            st.reads.push(rec);
        }
        Ok(())
    }

    /// Chooses split or single placement for a barrier ordering earlier work
    /// on the same queue before pass `p`.
    fn placement(
        &self,
        p: u32,
        queue: u16,
        resource: u32,
        has_same_queue_prior: bool,
    ) -> Placement {
        if self.options.split_barriers && has_same_queue_prior {
            // Whole-resource last touch: no other barrier or access on the
            // resource may sit between the begin and end halves.
            if let Some(&t) = self.last_touch[resource as usize].get(&queue)
                && t != p
                && self.qpos[p as usize] - self.qpos[t as usize] > 1
            {
                return Placement::Split {
                    begin_after: t,
                    end_before: p,
                };
            }
        }
        Placement::Single(Slot::Pre(p))
    }

    #[allow(clippy::too_many_arguments)]
    fn handoff(
        &mut self,
        p: u32,
        queue: u16,
        resource: u32,
        sub: u32,
        st: &mut SubState,
        cur: Layout,
        target: Layout,
    ) -> Result<(), PlanError> {
        let unsupported = PlanError::UnsupportedQueueHandoff {
            pass: self.graph.pass_id(p),
            resource: self.graph.texture_handle(resource),
            subresource: sub,
            from: cur,
            to: target,
        };
        let recs: Vec<Rec> = st
            .last_write
            .iter()
            .chain(st.reads.iter())
            .copied()
            .collect();
        let Some(first) = recs.first() else {
            return Err(unsupported);
        };
        let producer = first.queue;
        if producer == queue || recs.iter().any(|r| r.queue != producer) {
            return Err(unsupported);
        }
        let pkind = self.queue_kind(producer);
        let kind = self.queue_kind(queue);
        let mid = if pkind.can_transition(cur, target) {
            target
        } else if pkind.can_transition(cur, Layout::Common)
            && (target == Layout::Common || kind.can_transition(Layout::Common, target))
        {
            Layout::Common
        } else {
            return Err(unsupported);
        };
        let t = *self.last_touch[resource as usize]
            .get(&producer)
            .expect("producer touched the resource");
        let sync_before = recs
            .iter()
            .fold(Sync::NONE, |acc, r| sync_union(acc, r.sync));
        let access_before = recs
            .iter()
            .fold(Access::NO_ACCESS, |acc, r| acc.union_nonempty(r.access));
        self.push_pending(
            resource,
            Some(sub),
            Pending {
                placement: Placement::Single(Slot::Post(t)),
                sync_before,
                access_before,
                sync_after: Sync::NONE,
                access_after: Access::NO_ACCESS,
                layouts: Some((cur, mid)),
            },
        );
        st.layout = Some(mid);
        st.last_write = Some(Rec {
            pass: t,
            queue: producer,
            sync: Sync::NONE,
            access: Access::NO_ACCESS,
        });
        st.reads.clear();
        st.covered.clear();
        Ok(())
    }

    fn push_pending(&mut self, resource: u32, sub: Option<u32>, b: Pending) {
        match (sub, b.layouts) {
            (Some(s), Some((layout_before, layout_after))) => {
                self.tex
                    .entry(TexKey {
                        placement: b.placement,
                        resource,
                        sync_before: b.sync_before,
                        sync_after: b.sync_after,
                        access_before: b.access_before,
                        access_after: b.access_after,
                        layout_before,
                        layout_after,
                    })
                    .or_default()
                    .insert(s);
            }
            _ => {
                let e = self.buf.entry((b.placement, resource)).or_insert(BufScope {
                    sync_before: b.sync_before,
                    sync_after: b.sync_after,
                    access_before: b.access_before,
                    access_after: b.access_after,
                });
                e.sync_before = sync_union(e.sync_before, b.sync_before);
                e.sync_after = sync_union(e.sync_after, b.sync_after);
                e.access_before = e.access_before.union_nonempty(b.access_before);
                e.access_after = e.access_after.union_nonempty(b.access_after);
            }
        }
    }

    fn plan_final_layouts(&mut self, states: &mut [ResState]) -> Result<(), PlanError> {
        for (resource, info) in self.graph.resources.iter().enumerate() {
            let ResourceKind::Texture {
                final_layout: Some(fin),
                ..
            } = info.kind
            else {
                continue;
            };
            let ResState::Texture { subs, .. } = &mut states[resource] else {
                continue;
            };
            for (s, st) in subs.iter_mut().enumerate() {
                let cur = st.layout.expect("texture state has a layout");
                if cur == fin {
                    continue;
                }
                let recs: Vec<Rec> = st
                    .last_write
                    .iter()
                    .chain(st.reads.iter())
                    .copied()
                    .collect();
                // Prefer the queue of the most recently executed access.
                let mut candidates: Vec<u16> = Vec::new();
                let mut by_recency: Vec<&Rec> = recs.iter().collect();
                by_recency.sort_by_key(|r| Reverse(self.gpos[r.pass as usize]));
                for r in by_recency {
                    if !candidates.contains(&r.queue) {
                        candidates.push(r.queue);
                    }
                }
                for q in 0..self.graph.queues.len() as u16 {
                    if !candidates.contains(&q) {
                        candidates.push(q);
                    }
                }
                let Some(&q) = candidates
                    .iter()
                    .find(|&&q| self.queue_kind(q).can_transition(cur, fin))
                else {
                    return Err(PlanError::FinalLayoutUnreachable {
                        resource: self.graph.texture_handle(resource as u32),
                        subresource: s as u32,
                        from: cur,
                        to: fin,
                    });
                };
                let same: Vec<&Rec> = recs.iter().filter(|r| r.queue == q).collect();
                for r in recs.iter().filter(|r| r.queue != q) {
                    self.final_waits.entry(q).or_default().insert(r.pass);
                }
                let sync_before = same
                    .iter()
                    .fold(Sync::NONE, |acc, r| sync_union(acc, r.sync));
                let access_before = same
                    .iter()
                    .fold(Access::NO_ACCESS, |acc, r| acc.union_nonempty(r.access));
                self.push_pending(
                    resource as u32,
                    Some(s as u32),
                    Pending {
                        placement: Placement::Single(Slot::Final(q)),
                        sync_before,
                        access_before,
                        sync_after: Sync::NONE,
                        access_after: Access::NO_ACCESS,
                        layouts: Some((cur, fin)),
                    },
                );
                st.layout = Some(fin);
            }
        }
        Ok(())
    }

    fn assemble(self, order: &[u32], dependencies: Vec<Dependency>) -> Plan {
        let graph = self.graph;
        let nq = graph.queues.len();

        // Barrier slots.
        let mut slots: BTreeMap<Slot, Vec<Barrier>> = BTreeMap::new();
        for (key, subs) in &self.tex {
            let ResourceKind::Texture { dims, .. } = graph.resources[key.resource as usize].kind
            else {
                unreachable!()
            };
            for range in boxes(dims, subs) {
                let base = TextureBarrier {
                    resource: graph.texture_handle(key.resource),
                    sync_before: key.sync_before,
                    sync_after: key.sync_after,
                    access_before: key.access_before,
                    access_after: key.access_after,
                    layout_before: key.layout_before,
                    layout_after: key.layout_after,
                    subresources: range,
                };
                match key.placement {
                    Placement::Single(slot) => {
                        slots.entry(slot).or_default().push(Barrier::Texture(base))
                    }
                    Placement::Split {
                        begin_after,
                        end_before,
                    } => {
                        slots
                            .entry(Slot::Post(begin_after))
                            .or_default()
                            .push(Barrier::Texture(TextureBarrier {
                                sync_after: Sync::SPLIT,
                                ..base
                            }));
                        slots
                            .entry(Slot::Pre(end_before))
                            .or_default()
                            .push(Barrier::Texture(TextureBarrier {
                                sync_before: Sync::SPLIT,
                                ..base
                            }));
                    }
                }
            }
        }
        for (&(placement, resource), scope) in &self.buf {
            let base = BufferBarrier {
                resource: BufferHandle {
                    graph: graph.id,
                    index: resource,
                },
                sync_before: scope.sync_before,
                sync_after: scope.sync_after,
                access_before: scope.access_before,
                access_after: scope.access_after,
            };
            match placement {
                Placement::Single(slot) => {
                    slots.entry(slot).or_default().push(Barrier::Buffer(base))
                }
                Placement::Split {
                    begin_after,
                    end_before,
                } => {
                    slots
                        .entry(Slot::Post(begin_after))
                        .or_default()
                        .push(Barrier::Buffer(BufferBarrier {
                            sync_after: Sync::SPLIT,
                            ..base
                        }));
                    slots
                        .entry(Slot::Pre(end_before))
                        .or_default()
                        .push(Barrier::Buffer(BufferBarrier {
                            sync_before: Sync::SPLIT,
                            ..base
                        }));
                }
            }
        }
        for batch in slots.values_mut() {
            batch.sort_by_key(barrier_sort_key);
        }

        // Fence waits with vector-clock elision: a queue skips a wait when it
        // already knows (directly or transitively) that the producer finished.
        let mut by_qpos: Vec<Vec<u32>> = vec![Vec::new(); nq];
        for &p in order {
            by_qpos[graph.passes[p as usize].queue as usize].push(p);
        }
        let mut knowledge: Vec<Vec<u32>> = vec![vec![0; nq]; nq];
        let mut clock_after: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        let mut waits: BTreeMap<u32, Vec<(u16, u32)>> = BTreeMap::new();
        let mut signaled: BTreeSet<u32> = BTreeSet::new();
        let needed_waits = |q: u16,
                            producers: &BTreeSet<u32>,
                            knowledge: &mut Vec<Vec<u32>>,
                            clock_after: &BTreeMap<u32, Vec<u32>>,
                            signaled: &mut BTreeSet<u32>| {
            let mut latest: BTreeMap<u16, u32> = BTreeMap::new();
            for &d in producers {
                let dq = graph.passes[d as usize].queue;
                if dq == q {
                    continue;
                }
                let pos = self.qpos[d as usize];
                let e = latest.entry(dq).or_insert(0);
                *e = (*e).max(pos);
            }
            let candidates: Vec<(u16, u32)> = latest
                .into_iter()
                .filter(|&(dq, pos)| knowledge[q as usize][dq as usize] < pos)
                .map(|(dq, pos)| (dq, by_qpos[dq as usize][pos as usize - 1]))
                .collect();
            // Drop a wait that another needed wait already implies.
            let mut out = Vec::new();
            for &(dq, d) in &candidates {
                let pos = self.qpos[d as usize];
                let implied = candidates
                    .iter()
                    .any(|&(oq, o)| oq != dq && clock_after[&o][dq as usize] >= pos);
                if !implied {
                    out.push((dq, d));
                }
            }
            for &(_, d) in &out {
                signaled.insert(d);
                let clock = &clock_after[&d];
                for (k, v) in knowledge[q as usize].iter_mut().zip(clock) {
                    *k = (*k).max(*v);
                }
            }
            out
        };
        for &p in order {
            let q = graph.passes[p as usize].queue;
            let empty = BTreeSet::new();
            let producers = self.cross.get(&p).unwrap_or(&empty);
            let w = needed_waits(q, producers, &mut knowledge, &clock_after, &mut signaled);
            if !w.is_empty() {
                waits.insert(p, w);
            }
            knowledge[q as usize][q as usize] = self.qpos[p as usize];
            clock_after.insert(p, knowledge[q as usize].clone());
        }
        let mut final_wait_list: BTreeMap<u16, Vec<(u16, u32)>> = BTreeMap::new();
        for (&q, producers) in &self.final_waits {
            let w = needed_waits(q, producers, &mut knowledge, &clock_after, &mut signaled);
            if !w.is_empty() {
                final_wait_list.insert(q, w);
            }
        }

        // Fence values: 1, 2, ... in each queue's stream order.
        let mut value_of: BTreeMap<u32, u64> = BTreeMap::new();
        for passes in &by_qpos {
            let mut v = 0;
            for &p in passes {
                if signaled.contains(&p) {
                    v += 1;
                    value_of.insert(p, v);
                }
            }
        }

        let mut queues = Vec::with_capacity(nq);
        for (qi, passes) in by_qpos.iter().enumerate() {
            let q = qi as u16;
            let mut commands = Vec::new();
            let push_waits = |commands: &mut Vec<Command>, list: &[(u16, u32)]| {
                for &(dq, d) in list {
                    commands.push(Command::Wait {
                        queue: graph.queue_id(dq),
                        value: value_of[&d],
                    });
                }
            };
            for &p in passes {
                if let Some(list) = waits.get(&p) {
                    push_waits(&mut commands, list);
                }
                if let Some(b) = slots.remove(&Slot::Pre(p)) {
                    commands.push(Command::Barriers(b));
                }
                commands.push(Command::Execute(graph.pass_id(p)));
                if let Some(b) = slots.remove(&Slot::Post(p)) {
                    commands.push(Command::Barriers(b));
                }
                if let Some(&v) = value_of.get(&p) {
                    commands.push(Command::Signal { value: v });
                }
            }
            if let Some(list) = final_wait_list.get(&q) {
                push_waits(&mut commands, list);
            }
            if let Some(b) = slots.remove(&Slot::Final(q)) {
                commands.push(Command::Barriers(b));
            }
            queues.push(QueueStream {
                queue: graph.queue_id(q),
                kind: graph.queues[qi],
                commands,
            });
        }
        debug_assert!(slots.is_empty(), "every barrier slot belongs to a queue");
        Plan {
            order: order.iter().map(|&p| graph.pass_id(p)).collect(),
            dependencies,
            queues,
        }
    }
}

fn barrier_sort_key(b: &Barrier) -> (u32, SubresourceRange) {
    match b {
        Barrier::Buffer(b) => (b.resource.index, SubresourceRange::single(0, 0, 0)),
        Barrier::Texture(t) => (t.resource.index, t.subresources),
    }
}

fn split_segments(segments: &mut Vec<(u64, u64, SubState)>, at: u64) {
    if let Some(i) = segments.iter().position(|(s, e, _)| *s < at && at < *e) {
        let (s, e, st) = segments[i].clone();
        segments[i] = (s, at, st.clone());
        segments.insert(i + 1, (at, e, st));
    }
}

/// Greedy deterministic cover of a subresource set by boxes: extend along
/// mips, then slices, then planes, starting from the lowest uncovered index.
fn boxes(dims: TextureDims, set: &BTreeSet<u32>) -> Vec<SubresourceRange> {
    let mut left = set.clone();
    let mut out = Vec::new();
    while let Some(&first) = left.first() {
        let (m0, s0, p0) = dims.unflat(first);
        let has = |m: u32, s: u32, p: u32, left: &BTreeSet<u32>| {
            m < dims.mips
                && s < dims.slices
                && p < dims.planes
                && left.contains(&dims.flat(m, s, p))
        };
        let mut mips = 1;
        while has(m0 + mips, s0, p0, &left) {
            mips += 1;
        }
        let mut slices = 1;
        while (m0..m0 + mips).all(|m| has(m, s0 + slices, p0, &left)) {
            slices += 1;
        }
        let mut planes = 1;
        while (s0..s0 + slices).all(|s| (m0..m0 + mips).all(|m| has(m, s, p0 + planes, &left))) {
            planes += 1;
        }
        let r = SubresourceRange {
            first_mip: m0,
            mip_count: mips,
            first_slice: s0,
            slice_count: slices,
            first_plane: p0,
            plane_count: planes,
        };
        for i in r.indices(dims) {
            left.remove(&i);
        }
        out.push(r);
    }
    out
}

impl Plan {
    /// Number of barriers (split halves counted separately).
    pub fn barrier_count(&self) -> usize {
        self.queues
            .iter()
            .flat_map(|q| &q.commands)
            .map(|c| {
                if let Command::Barriers(b) = c {
                    b.len()
                } else {
                    0
                }
            })
            .sum()
    }

    /// Stable, human-readable rendering of the plan for review and golden
    /// tests.
    pub fn render(&self, graph: &RenderGraph) -> String {
        let name = |p: PassId| graph.pass_name(p).unwrap_or("?").to_owned();
        let mut out = String::new();
        let order: Vec<String> = self.order.iter().map(|&p| name(p)).collect();
        let _ = writeln!(out, "order: {}", order.join(", "));
        for q in &self.queues {
            let _ = writeln!(out, "queue {} ({}):", q.queue.index(), q.kind);
            for c in &q.commands {
                match c {
                    Command::Wait { queue, value } => {
                        let _ = writeln!(out, "  wait q{} >= {value}", queue.index());
                    }
                    Command::Signal { value } => {
                        let _ = writeln!(out, "  signal {value}");
                    }
                    Command::Execute(p) => {
                        let _ = writeln!(out, "  execute {}", name(*p));
                    }
                    Command::Barriers(list) => {
                        let _ = writeln!(out, "  barriers:");
                        for b in list {
                            match b {
                                Barrier::Buffer(b) => {
                                    let _ = writeln!(
                                        out,
                                        "    buffer {} sync {}->{} access {}->{}",
                                        graph.resource_name(b.resource).unwrap_or("?"),
                                        b.sync_before,
                                        b.sync_after,
                                        b.access_before,
                                        b.access_after
                                    );
                                }
                                Barrier::Texture(t) => {
                                    let _ = writeln!(
                                        out,
                                        "    texture {} [{}] sync {}->{} access {}->{} layout {}->{}",
                                        graph.resource_name(t.resource).unwrap_or("?"),
                                        t.subresources,
                                        t.sync_before,
                                        t.sync_after,
                                        t.access_before,
                                        t.access_after,
                                        t.layout_before,
                                        t.layout_after
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }
}
