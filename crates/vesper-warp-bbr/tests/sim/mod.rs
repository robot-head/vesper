//! Deterministic packet-level link simulator for behavioural tests.
//!
//! One bulk sender drives a [`Bbrv3`] through a single bottleneck: a FIFO
//! with a finite byte buffer (tail drop) served at a fixed rate, fixed
//! one-way propagation delay in each direction, optional seeded random and
//! Gilbert-Elliott burst loss after the queue, and an unconstrained ACK
//! path with one ACK per packet. Loss detection is a simplified RFC 9002:
//! packet threshold 3, time threshold 9/8 RTT, and a timeout for tails.
//!
//! The simulator validates the modelled algorithm only. It is not evidence
//! about real networks, fairness or interoperability.

#![allow(dead_code)]

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use vesper_warp_bbr::{
    AckedPacket, BbrConfig, BbrState, Bbrv3, Delivered, LostPacket, PacketDeliveryState, RttSample,
    SentPacket, SplitMix64,
};

pub const MS: u64 = 1_000_000;
pub const SEC: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug)]
pub struct Burst {
    /// Probability (ppm, per packet) of entering the bad state.
    pub enter_ppm: u64,
    /// Probability (ppm, per packet) of leaving the bad state.
    pub exit_ppm: u64,
    /// Loss probability (ppm) while in the bad state.
    pub loss_ppm: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct LinkConfig {
    pub rate_bytes_per_sec: u64,
    pub one_way_ns: u64,
    pub buffer_bytes: u64,
    pub random_loss_ppm: u64,
    pub burst: Option<Burst>,
    /// Probability (ppm) that a delivered packet is CE-marked when the queue
    /// is above `ecn_threshold_bytes`.
    pub ecn_threshold_bytes: Option<u64>,
    pub seed: u64,
    pub packet_bytes: u32,
}

impl LinkConfig {
    pub fn new(rate_bytes_per_sec: u64, rtt_ns: u64, buffer_bdp_permille: u64) -> Self {
        let bdp = rate_bytes_per_sec as u128 * rtt_ns as u128 / SEC as u128;
        Self {
            rate_bytes_per_sec,
            one_way_ns: rtt_ns / 2,
            buffer_bytes: (bdp * buffer_bdp_permille as u128 / 1000) as u64,
            random_loss_ppm: 0,
            burst: None,
            ecn_threshold_bytes: None,
            seed: 1,
            packet_bytes: 1200,
        }
    }

    pub fn bdp_bytes(&self) -> u64 {
        (self.rate_bytes_per_sec as u128 * (2 * self.one_way_ns) as u128 / SEC as u128) as u64
    }
}

#[derive(Clone, Copy, Debug)]
struct Outstanding {
    packet: SentPacket,
    state: PacketDeliveryState,
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub t: u64,
    pub state: BbrState,
    pub cwnd: u32,
    pub inflight: u64,
    pub pacing: u64,
    pub inflight_hi: Option<u64>,
    pub queue_bytes: u64,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub sent_packets: u64,
    pub dropped_packets: u64,
    pub declared_lost: u64,
    pub ce_marks: u64,
    /// (time, bytes) for every acknowledged packet, by ACK arrival.
    pub acked: Vec<(u64, u64)>,
    /// (time, new state) at every state change.
    pub transitions: Vec<(u64, BbrState)>,
    /// Queue occupancy (bytes) seen by each enqueued packet.
    pub queue_at_enqueue: Vec<(u64, u64)>,
    /// Largest `inflight - cwnd` right after a guarded send (should stay < MSS).
    pub max_send_overshoot: i64,
    /// Departure times returned by `schedule_edt_ns`, in order.
    pub launches: Vec<u64>,
    pub samples: Vec<Sample>,
}

pub struct Sim {
    pub bbr: Bbrv3,
    pub link: LinkConfig,
    pub now: u64,
    pub stats: Stats,
    rng: SplitMix64,
    in_burst: bool,
    link_free_ns: u64,
    /// (ack arrival time, pn, ce)
    acks: BinaryHeap<Reverse<(u64, u64, bool)>>,
    outstanding: BTreeMap<u64, Outstanding>,
    next_pn: u64,
    largest_acked: Option<u64>,
    latest_rtt: u64,
    min_rtt: u64,
    srtt: Option<u64>,
    last_state: BbrState,
    /// Stop sending (application-limited) while `now` is in one of these.
    pub idle_windows: Vec<(u64, u64)>,
    sample_every_ns: u64,
    next_sample_ns: u64,
}

impl Sim {
    pub fn new(link: LinkConfig) -> Self {
        let mut config = BbrConfig::quic(link.packet_bytes);
        config.probe_seed = link.seed ^ 0xbb;
        let bbr = Bbrv3::new(config, 0).expect("valid config");
        let last_state = bbr.state();
        Self {
            bbr,
            link,
            now: 0,
            stats: Stats::default(),
            rng: SplitMix64(link.seed),
            in_burst: false,
            link_free_ns: 0,
            acks: BinaryHeap::new(),
            outstanding: BTreeMap::new(),
            next_pn: 0,
            largest_acked: None,
            latest_rtt: 0,
            min_rtt: u64::MAX,
            srtt: None,
            last_state,
            idle_windows: Vec::new(),
            sample_every_ns: 10 * MS,
            next_sample_ns: 0,
        }
    }

    fn queue_bytes_at(&self, t: u64) -> u64 {
        let backlog_ns = self.link_free_ns.saturating_sub(t);
        (backlog_ns as u128 * self.link.rate_bytes_per_sec as u128 / SEC as u128) as u64
    }

    fn chance(&mut self, ppm: u64) -> bool {
        ppm > 0 && self.rng.below(1_000_000) < ppm
    }

    fn pto_ns(&self) -> u64 {
        match self.srtt {
            Some(s) => 3 * s + 10 * MS,
            None => SEC,
        }
    }

    fn record(&mut self) {
        let state = self.bbr.state();
        if state != self.last_state {
            self.stats.transitions.push((self.now, state));
            self.last_state = state;
        }
        if self.now >= self.next_sample_ns {
            self.next_sample_ns = self.now + self.sample_every_ns;
            self.stats.samples.push(Sample {
                t: self.now,
                state,
                cwnd: self.bbr.cwnd_bytes(),
                inflight: self.bbr.bytes_in_flight(),
                pacing: self.bbr.pacing_rate_bytes_per_sec(),
                inflight_hi: self.bbr.inflight_hi(),
                queue_bytes: self.queue_bytes_at(self.now),
            });
        }
    }

    /// Run until virtual time `end_ns`.
    pub fn run_until(&mut self, end_ns: u64) {
        while self.now < end_ns {
            let next_ack = self.acks.peek().map(|Reverse((t, _, _))| *t);
            let send_at = if self.bbr.can_send() {
                Some(self.now.max(self.bbr.next_edt_ns()))
            } else {
                None
            };
            let timeout = self
                .outstanding
                .values()
                .next()
                .map(|o| o.packet.sent_ns + self.pto_ns());
            let t = [next_ack, send_at, timeout, Some(end_ns)]
                .into_iter()
                .flatten()
                .min()
                .unwrap();
            self.now = t;
            if next_ack == Some(t) {
                let Reverse((_, pn, ce)) = self.acks.pop().unwrap();
                self.on_ack(pn, ce);
            } else if send_at == Some(t) && t < end_ns {
                if let Some(&(_, resume)) =
                    self.idle_windows.iter().find(|&&(a, b)| t >= a && t < b)
                {
                    // The application has nothing to send: tell BBR once,
                    // then wait out the window.
                    self.bbr.on_app_limited(t).expect("app limited");
                    let next = self.acks.peek().map(|Reverse((a, _, _))| *a);
                    self.now = next.map_or(resume, |a| a.min(resume)).min(end_ns);
                    if next.is_some_and(|a| a <= resume) {
                        let Reverse((_, pn, ce)) = self.acks.pop().unwrap();
                        self.on_ack(pn, ce);
                    }
                    self.record();
                    continue;
                }
                self.send_one();
            } else if timeout == Some(t) && t < end_ns {
                self.timeout_losses();
            }
            self.record();
        }
    }

    fn send_one(&mut self) {
        let bytes = self.link.packet_bytes;
        let launch = self.bbr.schedule_edt_ns(bytes, self.now);
        self.stats.launches.push(launch);
        assert_eq!(launch, self.now, "sender waits for the EDT");
        let pn = self.next_pn;
        self.next_pn += 1;
        let packet = SentPacket {
            pn,
            sent_ns: launch,
            bytes,
            ack_eliciting: true,
            in_flight: true,
        };
        let state = self
            .bbr
            .on_packet_sent(self.now, packet)
            .expect("guarded send");
        let overshoot = self.bbr.bytes_in_flight() as i64 - self.bbr.cwnd_bytes() as i64;
        self.stats.max_send_overshoot = self.stats.max_send_overshoot.max(overshoot);
        self.stats.sent_packets += 1;
        self.outstanding.insert(pn, Outstanding { packet, state });

        // Bottleneck.
        let queue = self.queue_bytes_at(self.now);
        self.stats.queue_at_enqueue.push((self.now, queue));
        if queue + bytes as u64 > self.link.buffer_bytes {
            self.stats.dropped_packets += 1;
            return;
        }
        let start = self.link_free_ns.max(self.now);
        let service = (bytes as u128 * SEC as u128).div_ceil(self.link.rate_bytes_per_sec as u128);
        self.link_free_ns = start + service as u64;
        let ce = match self.link.ecn_threshold_bytes {
            Some(threshold) => queue > threshold,
            None => false,
        };
        // Loss after the queue.
        let mut drop = self.chance(self.link.random_loss_ppm);
        if let Some(burst) = self.link.burst {
            if self.in_burst {
                if self.chance(burst.exit_ppm) {
                    self.in_burst = false;
                }
            } else if self.chance(burst.enter_ppm) {
                self.in_burst = true;
            }
            if self.in_burst && self.chance(burst.loss_ppm) {
                drop = true;
            }
        }
        if drop {
            self.stats.dropped_packets += 1;
            return;
        }
        let ack_at = self.link_free_ns + 2 * self.link.one_way_ns;
        self.acks.push(Reverse((ack_at, pn, ce)));
    }

    fn on_ack(&mut self, pn: u64, ce: bool) {
        let Some(o) = self.outstanding.remove(&pn) else {
            return; // already declared lost
        };
        if ce {
            self.stats.ce_marks += 1;
            self.bbr.on_ecn_ce(self.now, 1).expect("ce");
        }
        self.largest_acked = Some(self.largest_acked.map_or(pn, |l| l.max(pn)));
        self.latest_rtt = self.now - o.packet.sent_ns;
        self.min_rtt = self.min_rtt.min(self.latest_rtt);
        self.srtt = Some(match self.srtt {
            None => self.latest_rtt,
            Some(s) => (7 * s + self.latest_rtt) / 8,
        });
        let rtt = RttSample {
            latest_ns: self.latest_rtt,
            min_ns: self.min_rtt,
            smoothed_ns: self.srtt.unwrap(),
            ack_delay_ns: 0,
        };
        let delivered = [Delivered {
            packet: AckedPacket {
                pn,
                sent_ns: o.packet.sent_ns,
                bytes: o.packet.bytes,
            },
            state: o.state,
        }];
        self.bbr
            .on_ack(self.now, &delivered, Some(rtt))
            .expect("valid ack");
        self.stats.acked.push((self.now, o.packet.bytes as u64));
        self.threshold_losses();
    }

    fn declare_lost(&mut self, pn: u64) {
        let o = self.outstanding.remove(&pn).unwrap();
        self.stats.declared_lost += 1;
        self.bbr
            .on_packet_lost(
                self.now,
                LostPacket {
                    pn,
                    sent_ns: o.packet.sent_ns,
                    bytes: o.packet.bytes,
                },
                o.state,
            )
            .expect("valid loss");
    }

    fn threshold_losses(&mut self) {
        let Some(largest) = self.largest_acked else {
            return;
        };
        let window = 9 * self.latest_rtt.max(self.srtt.unwrap_or(0)) / 8;
        let lost: Vec<u64> = self
            .outstanding
            .range(..largest)
            .filter(|(pn, o)| **pn + 3 <= largest || o.packet.sent_ns + window <= self.now)
            .map(|(pn, _)| *pn)
            .collect();
        for pn in lost {
            self.declare_lost(pn);
        }
    }

    fn timeout_losses(&mut self) {
        let pto = self.pto_ns();
        let lost: Vec<u64> = self
            .outstanding
            .iter()
            .filter(|(_, o)| o.packet.sent_ns + pto <= self.now)
            .map(|(pn, _)| *pn)
            .collect();
        for pn in lost {
            self.declare_lost(pn);
        }
    }

    /// Bytes acknowledged with ACK time in `[from, to)`.
    pub fn goodput_bytes_per_sec(&self, from: u64, to: u64) -> u64 {
        let bytes: u64 = self
            .stats
            .acked
            .iter()
            .filter(|(t, _)| *t >= from && *t < to)
            .map(|(_, b)| *b)
            .sum();
        (bytes as u128 * SEC as u128 / (to - from) as u128) as u64
    }

    pub fn first_time_in(&self, state: BbrState) -> Option<u64> {
        self.stats
            .transitions
            .iter()
            .find(|(_, s)| *s == state)
            .map(|(t, _)| *t)
    }

    pub fn times_in(&self, state: BbrState) -> Vec<u64> {
        self.stats
            .transitions
            .iter()
            .filter(|(_, s)| *s == state)
            .map(|(t, _)| *t)
            .collect()
    }

    /// Mean and max queue occupancy seen by packets enqueued in `[from, to)`.
    pub fn queue_stats(&self, from: u64, to: u64) -> (u64, u64) {
        let window: Vec<u64> = self
            .stats
            .queue_at_enqueue
            .iter()
            .filter(|(t, _)| *t >= from && *t < to)
            .map(|(_, q)| *q)
            .collect();
        let max = window.iter().copied().max().unwrap_or(0);
        let mean = if window.is_empty() {
            0
        } else {
            window.iter().sum::<u64>() / window.len() as u64
        };
        (mean, max)
    }
}
