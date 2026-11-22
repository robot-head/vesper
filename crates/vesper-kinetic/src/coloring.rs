//! Deterministic greedy constraint-graph colouring (§36.1 item 2, §36.4).
//!
//! Constraints are visited in a stable order (sorted by their body ids, ties
//! broken by input index) and each one takes the lowest colour that no
//! earlier constraint sharing one of its bodies holds. Every colour is
//! therefore an island of constraints with pairwise disjoint bodies, so the
//! solver may project a colour's constraints in any order (or in parallel)
//! and still get the same bits. `lean/KineticColoring.lean` proves the
//! disjointness of the modelled greedy colouring; this Rust code is checked
//! against it on concrete inputs, not proved.

/// Index of a body in the world.
pub type BodyId = u32;

/// Most colours a colouring may use (one bit per colour per body).
pub const MAX_COLORS: usize = 64;

/// The bodies a constraint touches: two bodies, or one body against static
/// geometry (`b == None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstraintBodies {
    pub a: BodyId,
    pub b: Option<BodyId>,
}

impl ConstraintBodies {
    #[cfg_attr(vesper_port, allow(dead_code))]
    pub(crate) fn key(self) -> (BodyId, BodyId) {
        match self.b {
            Some(b) if b < self.a => (b, self.a),
            Some(b) => (self.a, b),
            None => (self.a, self.a),
        }
    }

    /// Whether two constraints touch a common body.
    pub fn shares_body(self, other: Self) -> bool {
        let mine = [Some(self.a), self.b];
        let theirs = [Some(other.a), other.b];
        mine.iter()
            .flatten()
            .any(|x| theirs.iter().flatten().any(|y| x == y))
    }
}

/// Typed colouring failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColoringError {
    /// A constraint names a body outside `0..body_count`.
    BodyOutOfRange { constraint: usize, body: BodyId },
    /// A two-body constraint names the same body twice.
    SelfConstraint { constraint: usize, body: BodyId },
    /// A body takes part in more than [`MAX_COLORS`] mutually conflicting
    /// constraints.
    TooManyColors { constraint: usize },
    /// More constraints than the colourer was sized for.
    CapacityExceeded { constraints: usize, capacity: usize },
    /// A k-body constraint names no body, more than
    /// [`MAX_CONSTRAINT_BODIES`], or has malformed offsets.
    BadArity { constraint: usize },
}

impl std::fmt::Display for ColoringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyOutOfRange { constraint, body } => {
                write!(f, "constraint {constraint} names unknown body {body}")
            }
            Self::SelfConstraint { constraint, body } => {
                write!(f, "constraint {constraint} links body {body} to itself")
            }
            Self::TooManyColors { constraint } => {
                write!(
                    f,
                    "constraint {constraint} needs more than {MAX_COLORS} colours"
                )
            }
            Self::CapacityExceeded {
                constraints,
                capacity,
            } => write!(
                f,
                "{constraints} constraints exceed colourer capacity {capacity}"
            ),
            Self::BadArity { constraint } => write!(
                f,
                "constraint {constraint} must name 1 to {MAX_CONSTRAINT_BODIES} bodies"
            ),
        }
    }
}

impl std::error::Error for ColoringError {}

/// Reusable colourer with preallocated buffers: after construction,
/// [`Colorer::color`] does not allocate.
#[derive(Debug)]
pub struct Colorer {
    pub(crate) body_masks: Vec<u64>,
    pub(crate) order: Vec<u32>,
    pub(crate) colors: Vec<u8>,
    pub(crate) layer_offsets: Vec<u32>,
    pub(crate) sorted: Vec<u32>,
    pub(crate) layer_count: usize,
    #[cfg(vesper_port)]
    port: PortScratch,
}

/// The port-backed build's argument buffers: the constraints as flat `a`
/// and `b` columns (`b` above `u32::MAX` is `None`) and the packed sort
/// keys (two words per constraint: the port's radix sort ping-pongs between
/// the halves), reserved at construction so colouring stays allocation-free.
#[cfg(vesper_port)]
#[derive(Debug)]
struct PortScratch {
    a: Vec<u32>,
    b: Vec<u64>,
    keys: Vec<u64>,
}

#[cfg(vesper_port)]
impl PortScratch {
    fn with_capacity(n: usize) -> Self {
        Self {
            a: Vec::with_capacity(n),
            b: Vec::with_capacity(n),
            keys: vec![0; 2 * n],
        }
    }
}

impl Colorer {
    /// A colourer for worlds of `body_count` bodies and up to
    /// `max_constraints` constraints per call.
    pub fn new(body_count: usize, max_constraints: usize) -> Self {
        Self {
            body_masks: vec![0; body_count],
            order: Vec::with_capacity(max_constraints),
            colors: vec![0; max_constraints],
            layer_offsets: Vec::with_capacity(MAX_COLORS + 1),
            sorted: Vec::with_capacity(max_constraints),
            layer_count: 0,
            #[cfg(vesper_port)]
            port: PortScratch::with_capacity(max_constraints),
        }
    }

    /// Colour `constraints`. On success, [`Colorer::layers`] yields the colour
    /// islands in colour order, each listing constraint indices in the stable
    /// visiting order.
    pub fn color(&mut self, constraints: &[ConstraintBodies]) -> Result<(), ColoringError> {
        #[cfg(vesper_port)]
        {
            self.color_port(constraints)
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::coloring::color(self, constraints)
        }
    }

    /// The port-backed build of [`Colorer::color`]: the Vesper port
    /// (vesper/src/coloring.ves) colours into these buffers.
    #[cfg(vesper_port)]
    fn color_port(&mut self, constraints: &[ConstraintBodies]) -> Result<(), ColoringError> {
        let n = constraints.len();
        let capacity = self.colors.len();
        if n > capacity {
            // The buffers are sized for `capacity`; the port checks again.
            return Err(ColoringError::CapacityExceeded {
                constraints: n,
                capacity,
            });
        }
        let p = &mut self.port;
        p.a.clear();
        p.b.clear();
        for c in constraints {
            p.a.push(c.a);
            p.b.push(c.b.map_or(u64::MAX, u64::from));
        }
        self.order.clear();
        self.order.resize(n, 0);
        self.sorted.clear();
        self.sorted.resize(n, 0);
        self.layer_offsets.clear();
        self.layer_offsets.resize(MAX_COLORS + 1, 0);
        let used = crate::port::colorer_color(
            &p.a,
            &p.b,
            capacity as u64,
            &mut self.body_masks,
            &mut p.keys[..2 * n],
            &mut self.order,
            &mut self.colors,
            &mut self.layer_offsets,
            &mut self.sorted,
        )? as usize;
        self.layer_offsets.truncate(used + 1);
        self.layer_count = used;
        Ok(())
    }

    /// Most constraints one call may colour.
    pub fn capacity(&self) -> usize {
        self.colors.len()
    }

    /// Number of colours the last colouring used.
    pub fn layer_count(&self) -> usize {
        self.layer_count
    }

    /// Constraint indices of colour `color`.
    pub fn layer(&self, color: usize) -> &[u32] {
        let start = self.layer_offsets[color] as usize;
        let end = self.layer_offsets[color + 1] as usize;
        &self.sorted[start..end]
    }

    /// Colour islands in colour order.
    pub fn layers(&self) -> impl Iterator<Item = &[u32]> + '_ {
        (0..self.layer_count).map(|c| self.layer(c))
    }

    /// Colour of constraint `index` in the last colouring.
    pub fn color_of(&self, index: usize) -> u8 {
        self.colors[index]
    }

    /// Constraint indices in the stable visiting order of the last colouring.
    pub fn visiting_order(&self) -> &[u32] {
        &self.order
    }
}

/// Independent checker: every island has pairwise body-disjoint constraints
/// and every constraint appears in exactly one island.
pub fn islands_are_disjoint(colorer: &Colorer, constraints: &[ConstraintBodies]) -> bool {
    let mut seen = vec![false; constraints.len()];
    for layer in colorer.layers() {
        for (n, &i) in layer.iter().enumerate() {
            if std::mem::replace(&mut seen[i as usize], true) {
                return false;
            }
            for &j in &layer[n + 1..] {
                if constraints[i as usize].shares_body(constraints[j as usize]) {
                    return false;
                }
            }
        }
    }
    seen.iter().all(|s| *s)
}

/// Most bodies one k-body constraint may touch (a tetrahedron's four
/// particles).
pub const MAX_CONSTRAINT_BODIES: usize = 4;

/// Greedy colouring of constraints that touch any number of bodies (1 to
/// [`MAX_CONSTRAINT_BODIES`]): soft-body distance and tetrahedral volume
/// constraints, or joints and contacts given as body lists.
///
/// Constraint `i` touches `bodies[offsets[i]..offsets[i + 1]]`. The visiting
/// order sorts constraints by their smallest body id, then their largest,
/// then their index; each constraint takes the lowest colour that none of
/// its bodies already holds. Two-body constraints visited in the same order
/// get the same colours as [`Colorer`]. `lean/KineticColoring.lean` models
/// this colouring over body lists (`colorAllSets`) and proves that a
/// colour's constraints touch pairwise disjoint bodies; the Rust test
/// compares the Rust colours with the model's on concrete inputs.
#[derive(Debug)]
pub struct MultiColorer {
    pub(crate) body_masks: Vec<u64>,
    pub(crate) order: Vec<u32>,
    #[cfg(not(vesper_port))]
    pub(crate) keys: Vec<(BodyId, BodyId)>,
    /// `(min << 32) | max`, the port's packed sort key, two words per
    /// constraint (the port's radix sort ping-pongs between the halves).
    #[cfg(vesper_port)]
    keys: Vec<u64>,
    pub(crate) colors: Vec<u8>,
    pub(crate) layer_offsets: Vec<u32>,
    pub(crate) sorted: Vec<u32>,
    pub(crate) layer_count: usize,
}

impl MultiColorer {
    /// A colourer for `body_count` bodies and up to `max_constraints`
    /// constraints per call; [`MultiColorer::color`] does not allocate.
    pub fn new(body_count: usize, max_constraints: usize) -> Self {
        Self {
            body_masks: vec![0; body_count],
            order: Vec::with_capacity(max_constraints),
            #[cfg(not(vesper_port))]
            keys: vec![Default::default(); max_constraints],
            #[cfg(vesper_port)]
            keys: vec![0; 2 * max_constraints],
            colors: vec![0; max_constraints],
            layer_offsets: Vec::with_capacity(MAX_COLORS + 1),
            sorted: Vec::with_capacity(max_constraints),
            layer_count: 0,
        }
    }

    /// Colour the constraints given by `bodies` and `offsets`
    /// (`offsets.len()` is the constraint count plus one).
    pub fn color(&mut self, bodies: &[BodyId], offsets: &[u32]) -> Result<(), ColoringError> {
        #[cfg(vesper_port)]
        {
            self.color_port(bodies, offsets)
        }
        #[cfg(not(vesper_port))]
        {
            crate::engine::coloring::color_sets(self, bodies, offsets)
        }
    }

    /// The port-backed build of [`MultiColorer::color`] (vesper/src/coloring.ves).
    #[cfg(vesper_port)]
    fn color_port(&mut self, bodies: &[BodyId], offsets: &[u32]) -> Result<(), ColoringError> {
        let count = offsets.len().saturating_sub(1);
        let capacity = self.colors.len();
        if count > capacity {
            return Err(ColoringError::CapacityExceeded {
                constraints: count,
                capacity,
            });
        }
        self.order.clear();
        self.order.resize(count, 0);
        self.sorted.clear();
        self.sorted.resize(count, 0);
        self.layer_offsets.clear();
        self.layer_offsets.resize(MAX_COLORS + 1, 0);
        let used = crate::port::multi_colorer_color(
            bodies,
            offsets,
            capacity as u64,
            &mut self.body_masks,
            &mut self.keys[..2 * count],
            &mut self.order,
            &mut self.colors,
            &mut self.layer_offsets,
            &mut self.sorted,
        )? as usize;
        self.layer_offsets.truncate(used + 1);
        self.layer_count = used;
        Ok(())
    }

    /// Most constraints one call may colour.
    pub fn capacity(&self) -> usize {
        self.colors.len()
    }

    /// Number of colours the last colouring used.
    pub fn layer_count(&self) -> usize {
        self.layer_count
    }

    /// Constraint indices of colour `color`, in visiting order.
    pub fn layer(&self, color: usize) -> &[u32] {
        let start = self.layer_offsets[color] as usize;
        let end = self.layer_offsets[color + 1] as usize;
        &self.sorted[start..end]
    }

    /// Colour of constraint `index` in the last colouring.
    pub fn color_of(&self, index: usize) -> u8 {
        self.colors[index]
    }

    /// Constraint indices in the stable visiting order of the last colouring.
    pub fn visiting_order(&self) -> &[u32] {
        &self.order
    }
}

/// Independent checker for [`MultiColorer`]: every colour's constraints
/// touch pairwise disjoint bodies and every constraint has one colour.
pub fn sets_are_disjoint(colorer: &MultiColorer, bodies: &[BodyId], offsets: &[u32]) -> bool {
    let count = offsets.len().saturating_sub(1);
    let set = |i: u32| &bodies[offsets[i as usize] as usize..offsets[i as usize + 1] as usize];
    let mut seen = vec![false; count];
    for c in 0..colorer.layer_count() {
        let layer = colorer.layer(c);
        for (n, &i) in layer.iter().enumerate() {
            if std::mem::replace(&mut seen[i as usize], true) {
                return false;
            }
            for &j in &layer[n + 1..] {
                if set(i).iter().any(|b| set(j).contains(b)) {
                    return false;
                }
            }
        }
    }
    seen.iter().all(|s| *s)
}
