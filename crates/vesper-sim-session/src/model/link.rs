//! A bounded, deterministic datagram link model (`datagram-v1`).
//!
//! Each [`Link`] is one direction of a WAN path. Every datagram handed to
//! [`Link::transmit`] draws its fate from the link's own seeded generator, in a
//! fixed order, so a run is a pure function of the scenario:
//! 1. **Gilbert–Elliott state step.** With a configured two-state chain the
//!    link moves Good→Bad with probability `good_to_bad_ppm` and Bad→Good with
//!    `bad_to_good_ppm` (parts per million, one step per datagram). The
//!    datagram is then lost with `loss_good_ppm` or `loss_bad_ppm` for the
//!    state it sees. The stationary loss rate is
//!    `πG·loss_good + πB·loss_bad` with `πB = g2b / (g2b + b2g)`
//!    ([`GilbertElliott::stationary_loss_ppm`]).
//! 2. **Independent loss** with `loss_ppm`.
//! 3. **Bottleneck queue.** A surviving datagram joins a FIFO with a finite
//!    `buffer_packets` and a fixed service rate. A datagram that finds the
//!    buffer full is tail-dropped (a modelled outcome, not an error). Otherwise
//!    it departs at `max(at, previous departure) + service`, so a micro-burst
//!    builds queueing delay (bufferbloat) before it overflows.
//! 4. **Reordering.** With `reorder.ppm` the datagram is held for an extra
//!    `reorder.extra_delay_ns` after the propagation delay, so later datagrams
//!    overtake it.
//!
//! Duplication is decided separately by [`Link::duplicates`]; the session
//! transmits the copy as an independent datagram. NAT rebinding is a timed
//! event that changes a flow's source address; it is applied by the session,
//! not by the link.
//!
//! The generator is SplitMix64 (not cryptographic). It never reads a real
//! clock or random source.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const PPM: u32 = 1_000_000;

/// SplitMix64: a tiny, well-mixed deterministic generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitMix64(u64);
impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        // Wrapping arithmetic is the definition of SplitMix64.
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// One Bernoulli draw with probability `ppm / 10^6`. Always consumes one
    /// value, so the draw sequence does not depend on the probabilities.
    pub fn chance(&mut self, ppm: u32) -> bool {
        (self.next_u64() % u64::from(PPM)) < u64::from(ppm)
    }
}

/// FNV-1a of a name, used to derive per-link and per-flow seeds.
pub fn name_seed(seed: u64, name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ seed.rotate_left(17)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GilbertElliott {
    pub good_to_bad_ppm: u32,
    pub bad_to_good_ppm: u32,
    pub loss_good_ppm: u32,
    pub loss_bad_ppm: u32,
}
impl GilbertElliott {
    /// Stationary probability of the Bad state, in ppm (rounded down).
    pub fn stationary_bad_ppm(&self) -> u32 {
        let (g, b) = (
            u64::from(self.good_to_bad_ppm),
            u64::from(self.bad_to_good_ppm),
        );
        if g + b == 0 {
            return 0;
        }
        (g * u64::from(PPM) / (g + b)) as u32
    }
    /// Stationary loss probability of the chain alone, in ppm.
    pub fn stationary_loss_ppm(&self) -> u32 {
        let bad = u64::from(self.stationary_bad_ppm());
        let good = u64::from(PPM) - bad;
        ((good * u64::from(self.loss_good_ppm) + bad * u64::from(self.loss_bad_ppm))
            / u64::from(PPM)) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reorder {
    pub ppm: u32,
    pub extra_delay_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bottleneck {
    /// Datagrams the queue holds, including the one in service.
    pub buffer_packets: u32,
    pub rate_bytes_per_sec: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkSpec {
    pub name: String,
    pub base_delay_ns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gilbert_elliott: Option<GilbertElliott>,
    #[serde(default)]
    pub loss_ppm: u32,
    #[serde(default)]
    pub duplicate_ppm: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reorder: Option<Reorder>,
    pub bottleneck: Bottleneck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LossCause {
    GilbertElliott,
    Independent,
    QueueOverflow,
}

/// What happened to one transmitted datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Fate {
    Lost {
        cause: LossCause,
    },
    Delivered {
        /// Time spent in the bottleneck, including its own service.
        queue_delay_ns: u64,
        reorder_extra_ns: u64,
        arrive_at_ns: u64,
    },
}

/// The link's choices for one datagram, recorded in the model trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transmission {
    /// The Gilbert–Elliott state this datagram saw (`false` without a chain).
    pub ge_bad: bool,
    pub fate: Fate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// The virtual clock would overflow.
    TimeOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    spec: LinkSpec,
    rng: SplitMix64,
    bad: bool,
    /// Departure times of queued datagrams, ascending.
    departures: VecDeque<u64>,
}

impl Link {
    pub fn new(spec: LinkSpec, seed: u64) -> Self {
        let rng = SplitMix64::new(name_seed(seed, &spec.name));
        Self {
            spec,
            rng,
            bad: false,
            departures: VecDeque::new(),
        }
    }
    pub fn spec(&self) -> &LinkSpec {
        &self.spec
    }
    pub fn in_bad_state(&self) -> bool {
        self.bad
    }
    /// Datagrams queued in the bottleneck at `at_ns`.
    pub fn queued_at(&self, at_ns: u64) -> u32 {
        let served = self.departures.partition_point(|d| *d <= at_ns);
        (self.departures.len() - served) as u32
    }

    /// Service time of a datagram of `bytes`, rounded up (at least 1 ns).
    pub fn service_ns(&self, bytes: u32) -> u64 {
        let rate = u128::from(self.spec.bottleneck.rate_bytes_per_sec.max(1));
        let ns = (u128::from(bytes) * 1_000_000_000).div_ceil(rate);
        u64::try_from(ns).unwrap_or(u64::MAX).max(1)
    }

    /// Whether the next datagram is duplicated (one draw).
    pub fn duplicates(&mut self) -> bool {
        self.rng.chance(self.spec.duplicate_ppm)
    }

    /// Draw the fate of one datagram of `bytes` handed to the link at
    /// `at_ns`. `now_ns` is the virtual clock (never later than `at_ns`);
    /// queue entries that departed by then are discarded.
    pub fn transmit(
        &mut self,
        now_ns: u64,
        at_ns: u64,
        bytes: u32,
    ) -> Result<Transmission, LinkError> {
        let served = self.departures.partition_point(|d| *d <= now_ns);
        self.departures.drain(..served);
        // Fixed draw order: GE step, GE loss, independent loss, reorder.
        let mut ge_lost = false;
        if let Some(ge) = self.spec.gilbert_elliott {
            let flip = if self.bad {
                self.rng.chance(ge.bad_to_good_ppm)
            } else {
                self.rng.chance(ge.good_to_bad_ppm)
            };
            if flip {
                self.bad = !self.bad;
            }
            let loss = if self.bad {
                ge.loss_bad_ppm
            } else {
                ge.loss_good_ppm
            };
            ge_lost = self.rng.chance(loss);
        }
        let independent = self.rng.chance(self.spec.loss_ppm);
        let reorder = self
            .spec
            .reorder
            .map(|r| (self.rng.chance(r.ppm), r.extra_delay_ns));
        let ge_bad = self.bad;
        let lost = |cause| Transmission {
            ge_bad,
            fate: Fate::Lost { cause },
        };
        if ge_lost {
            return Ok(lost(LossCause::GilbertElliott));
        }
        if independent {
            return Ok(lost(LossCause::Independent));
        }
        if self.queued_at(at_ns) >= self.spec.bottleneck.buffer_packets {
            return Ok(lost(LossCause::QueueOverflow));
        }
        let start = self
            .departures
            .back()
            .copied()
            .map_or(at_ns, |last| last.max(at_ns));
        let departure = start
            .checked_add(self.service_ns(bytes))
            .ok_or(LinkError::TimeOverflow)?;
        let reorder_extra_ns = match reorder {
            Some((true, extra)) => extra,
            _ => 0,
        };
        let arrive_at_ns = departure
            .checked_add(self.spec.base_delay_ns)
            .and_then(|t| t.checked_add(reorder_extra_ns))
            .ok_or(LinkError::TimeOverflow)?;
        // Insert keeping the deque ascending (departures are nondecreasing
        // for nondecreasing `at_ns`; later `at_ns` values only append).
        let index = self.departures.partition_point(|d| *d <= departure);
        self.departures.insert(index, departure);
        Ok(Transmission {
            ge_bad,
            fate: Fate::Delivered {
                queue_delay_ns: departure - at_ns,
                reorder_extra_ns,
                arrive_at_ns,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> LinkSpec {
        LinkSpec {
            name: "wan".into(),
            base_delay_ns: 1000,
            gilbert_elliott: None,
            loss_ppm: 0,
            duplicate_ppm: 0,
            reorder: None,
            bottleneck: Bottleneck {
                buffer_packets: 4,
                rate_bytes_per_sec: 1_000_000_000,
            },
        }
    }

    /// Stationary loss within a documented tolerance: over 10^6 datagrams
    /// per seed the measured GE loss rate is within 5% (relative) of
    /// `stationary_loss_ppm`, the Bad-state share within 5% of
    /// `stationary_bad_ppm`, and the mean Bad sojourn within 5% of
    /// `10^6 / bad_to_good_ppm` datagrams.
    #[test]
    fn gilbert_elliott_matches_stationary_rates() {
        let ge = GilbertElliott {
            good_to_bad_ppm: 10_000,
            bad_to_good_ppm: 100_000,
            loss_good_ppm: 1_000,
            loss_bad_ppm: 500_000,
        };
        assert_eq!(ge.stationary_bad_ppm(), 90_909);
        assert_eq!(ge.stationary_loss_ppm(), 46_363);
        for seed in [1, 2, 3] {
            let mut s = spec();
            s.gilbert_elliott = Some(ge);
            s.bottleneck.buffer_packets = u32::MAX;
            let mut link = Link::new(s, seed);
            let n = 1_000_000u64;
            let (mut lost, mut bad, mut bursts) = (0u64, 0u64, 0u64);
            let mut previous = false;
            for i in 0..n {
                // Far-apart sends keep the queue empty.
                let t = link.transmit(i * 1_000_000, i * 1_000_000, 100).unwrap();
                if matches!(t.fate, Fate::Lost { .. }) {
                    lost += 1;
                }
                if t.ge_bad {
                    bad += 1;
                    if !previous {
                        bursts += 1;
                    }
                }
                previous = t.ge_bad;
            }
            let within =
                |measured: f64, expected: f64| ((measured - expected) / expected).abs() < 0.05;
            let loss = lost as f64 * 1e6 / n as f64;
            let bad_share = bad as f64 * 1e6 / n as f64;
            let sojourn = bad as f64 / bursts as f64;
            assert!(within(loss, 46_363.0), "seed {seed}: loss {loss} ppm");
            assert!(within(bad_share, 90_909.0), "seed {seed}: bad {bad_share}");
            assert!(within(sojourn, 10.0), "seed {seed}: sojourn {sojourn}");
        }
    }

    #[test]
    fn independent_loss_and_duplication_rates() {
        let mut s = spec();
        s.loss_ppm = 20_000;
        s.duplicate_ppm = 30_000;
        s.bottleneck.buffer_packets = u32::MAX;
        let mut link = Link::new(s, 9);
        let n = 500_000u64;
        let (mut lost, mut dup) = (0u64, 0u64);
        for i in 0..n {
            dup += u64::from(link.duplicates());
            let t = link.transmit(i * 10_000, i * 10_000, 100).unwrap();
            lost += u64::from(matches!(t.fate, Fate::Lost { .. }));
        }
        let rel = |m: u64, e: f64| ((m as f64 * 1e6 / n as f64 - e) / e).abs();
        assert!(rel(lost, 20_000.0) < 0.05);
        assert!(rel(dup, 30_000.0) < 0.05);
    }

    #[test]
    fn micro_burst_builds_queue_delay_then_tail_drops() {
        // 1000-byte datagrams at 10^9 B/s: 1000 ns of service each.
        let mut link = Link::new(spec(), 0);
        let mut fates = vec![];
        for _ in 0..6 {
            fates.push(link.transmit(0, 0, 1000).unwrap().fate);
        }
        let delays: Vec<u64> = fates
            .iter()
            .filter_map(|f| match f {
                Fate::Delivered { queue_delay_ns, .. } => Some(*queue_delay_ns),
                _ => None,
            })
            .collect();
        assert_eq!(delays, [1000, 2000, 3000, 4000]);
        assert_eq!(
            fates[4..],
            [
                Fate::Lost {
                    cause: LossCause::QueueOverflow
                },
                Fate::Lost {
                    cause: LossCause::QueueOverflow
                }
            ]
        );
        assert_eq!(link.queued_at(0), 4);
        assert_eq!(link.queued_at(2500), 2);
        // After the queue drains, a datagram sees only its own service.
        let t = link.transmit(10_000, 10_000, 1000).unwrap();
        assert_eq!(
            t.fate,
            Fate::Delivered {
                queue_delay_ns: 1000,
                reorder_extra_ns: 0,
                arrive_at_ns: 12_000
            }
        );
    }

    #[test]
    fn reordering_lets_later_datagrams_overtake() {
        let mut s = spec();
        s.reorder = Some(Reorder {
            ppm: PPM,
            extra_delay_ns: 50_000,
        });
        let mut link = Link::new(s, 0);
        let Fate::Delivered { arrive_at_ns, .. } = link.transmit(0, 0, 10).unwrap().fate else {
            panic!()
        };
        assert_eq!(arrive_at_ns, 10 + 1000 + 50_000);
    }

    #[test]
    fn same_seed_same_choices_and_overflow_is_an_error() {
        let mut s = spec();
        s.gilbert_elliott = Some(GilbertElliott {
            good_to_bad_ppm: 300_000,
            bad_to_good_ppm: 300_000,
            loss_good_ppm: 100_000,
            loss_bad_ppm: 900_000,
        });
        let run = |seed| {
            let mut link = Link::new(s.clone(), seed);
            (0..200)
                .map(|i| link.transmit(i * 5000, i * 5000, 100).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(run(4), run(4));
        assert_ne!(run(4), run(5));
        let mut link = Link::new(spec(), 0);
        assert_eq!(
            link.transmit(u64::MAX - 1, u64::MAX - 1, 1000),
            Err(LinkError::TimeOverflow)
        );
    }
}
