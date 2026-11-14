//! Sleeping islands.
//!
//! An island is a connected component of dynamic bodies under this tick's
//! contact manifolds and joints (static bodies and the ground do not join
//! islands). After each step a dynamic body whose speed and spin stay below
//! the thresholds counts one more quiet tick; when every body of an island
//! has been quiet for [`SleepConfig::ticks`] ticks, the whole island falls
//! asleep and its velocities are zeroed. A sleeping body is not integrated,
//! and constraints between sleeping (or static) bodies are not solved.
//!
//! Collision between two sleeping bodies is deferred: at the start of a
//! step, any island that holds an awake body wakes completely, the deferred
//! pairs it now reaches are collided, and this repeats until no island
//! wakes, so a disturbance propagates through a resting pile in one tick.
//! Every pass visits bodies and pairs in index order: sleeping is as
//! deterministic as the rest of the world. Sleep state is part of
//! [`RigidWorld::hash`] when sleeping is enabled.

use super::{RigidError, RigidWorld, dot};

/// When bodies fall asleep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SleepConfig {
    /// Speed below which a body is quiet (m/s).
    pub linear_threshold: f32,
    /// Spin below which a body is quiet (rad/s).
    pub angular_threshold: f32,
    /// Quiet ticks before an island sleeps (at least 1).
    pub ticks: u32,
}

impl Default for SleepConfig {
    fn default() -> Self {
        Self {
            linear_threshold: 0.05,
            angular_threshold: 0.05,
            ticks: 30,
        }
    }
}

impl RigidWorld {
    /// Enable (or with `None` disable) sleeping. Enabling starts every body
    /// awake with no quiet ticks; disabling wakes every body.
    pub fn set_sleeping(&mut self, config: Option<SleepConfig>) -> Result<(), RigidError> {
        if let Some(c) = config {
            let ok = |v: f32| v.is_finite() && v >= 0.0;
            if !(ok(c.linear_threshold) && ok(c.angular_threshold)) || c.ticks == 0 {
                return Err(RigidError::InvalidConfig(
                    "sleep thresholds must be finite and >= 0, ticks >= 1",
                ));
            }
        }
        self.sleep = config;
        self.asleep.fill(false);
        self.sleep_ticks.fill(0);
        Ok(())
    }

    /// Whether body `i` is asleep (`false` when sleeping is off).
    pub fn is_asleep(&self, i: usize) -> bool {
        self.sleep.is_some() && self.asleep.get(i).copied().unwrap_or(false)
    }

    /// Number of bodies asleep.
    pub fn sleeping_count(&self) -> usize {
        if self.sleep.is_none() {
            return 0;
        }
        self.asleep.iter().filter(|a| **a).count()
    }

    /// Wake body `i` (no effect when sleeping is off or `i` is out of
    /// range). Its island wakes at the next step.
    pub fn wake(&mut self, i: usize) {
        if let (Some(a), Some(t)) = (self.asleep.get_mut(i), self.sleep_ticks.get_mut(i)) {
            *a = false;
            *t = 0;
        }
    }

    fn find(&mut self, mut i: u32) -> u32 {
        while self.island[i as usize] != i {
            let p = self.island[i as usize];
            self.island[i as usize] = self.island[p as usize];
            i = p;
        }
        i
    }

    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        // The smaller root wins: the forest does not depend on visit order.
        match ra.cmp(&rb) {
            std::cmp::Ordering::Less => self.island[rb as usize] = ra,
            std::cmp::Ordering::Greater => self.island[ra as usize] = rb,
            std::cmp::Ordering::Equal => {}
        }
    }

    /// Union-find over this tick's manifolds and joints between dynamic
    /// bodies; afterwards `find(i)` is `i`'s island root.
    fn build_islands(&mut self) {
        for i in 0..self.island.len() {
            self.island[i] = i as u32;
        }
        for k in 0..self.manifolds.len() {
            let m = &self.manifolds[k];
            if let Some(b) = m.b {
                let a = m.a;
                if self.bodies[a as usize].inv_mass > 0.0 && self.bodies[b as usize].inv_mass > 0.0
                {
                    self.union(a, b);
                }
            }
        }
        for k in 0..self.joints.len() {
            let j = &self.joints[k].joint;
            if let Some(b) = j.b {
                let a = j.a;
                if self.bodies[a as usize].inv_mass > 0.0 && self.bodies[b as usize].inv_mass > 0.0
                {
                    self.union(a, b);
                }
            }
        }
    }

    /// Wake every island that holds an awake body, colliding the deferred
    /// pairs that woken bodies reach, until nothing more wakes.
    pub(super) fn wake_islands(&mut self) -> Result<(), RigidError> {
        let n = self.bodies.len();
        loop {
            self.build_islands();
            self.island_awake.fill(false);
            for i in 0..n {
                if self.bodies[i].inv_mass > 0.0 && !self.asleep[i] {
                    let r = self.find(i as u32) as usize;
                    self.island_awake[r] = true;
                }
            }
            let mut woke = false;
            for i in 0..n {
                if self.bodies[i].inv_mass > 0.0 && self.asleep[i] {
                    let r = self.find(i as u32) as usize;
                    if self.island_awake[r] {
                        self.asleep[i] = false;
                        self.sleep_ticks[i] = 0;
                        woke = true;
                    }
                }
            }
            if !woke {
                return Ok(());
            }
            // Collide deferred pairs that now involve an awake body, in
            // broadphase order.
            let mut kept = 0;
            for k in 0..self.deferred.len() {
                let pair = self.deferred[k];
                let (a, b) = self.pairs[pair as usize];
                if self.active(a) || self.active(b) {
                    self.pair_manifold(a as usize, b as usize)?;
                } else {
                    self.deferred[kept] = pair;
                    kept += 1;
                }
            }
            self.deferred.truncate(kept);
        }
    }

    /// After a successful step: count quiet ticks and put quiet islands to
    /// sleep.
    pub(super) fn update_sleep(&mut self) {
        let Some(cfg) = self.sleep else {
            return;
        };
        let lin2 = cfg.linear_threshold * cfg.linear_threshold;
        let ang2 = cfg.angular_threshold * cfg.angular_threshold;
        let n = self.bodies.len();
        for i in 0..n {
            let s = &self.bodies[i];
            if s.inv_mass == 0.0 || self.asleep[i] {
                continue;
            }
            if dot(s.v, s.v) < lin2 && dot(s.w, s.w) < ang2 {
                self.sleep_ticks[i] = self.sleep_ticks[i].saturating_add(1);
            } else {
                self.sleep_ticks[i] = 0;
            }
        }
        self.build_islands();
        // `island_awake[root]`: some dynamic awake member is not ready.
        self.island_awake.fill(false);
        for i in 0..n {
            if self.bodies[i].inv_mass > 0.0 && !self.asleep[i] && self.sleep_ticks[i] < cfg.ticks {
                let r = self.find(i as u32) as usize;
                self.island_awake[r] = true;
            }
        }
        for i in 0..n {
            if self.bodies[i].inv_mass > 0.0 && !self.asleep[i] {
                let r = self.find(i as u32) as usize;
                if !self.island_awake[r] {
                    self.asleep[i] = true;
                    self.bodies[i].v = [0.0; 3];
                    self.bodies[i].w = [0.0; 3];
                }
            }
        }
    }
}
