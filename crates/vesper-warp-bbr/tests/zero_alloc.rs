//! "0 Alloc": `on_ack` and `schedule_edt_ns` (plus the send/loss/ECN paths
//! used in steady state) perform no heap allocation after warm-up,
//! measured with a counting global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use vesper_warp_bbr::{
    AckedPacket, BbrConfig, Bbrv3, Delivered, LostPacket, PacketDeliveryState, RttSample,
    SentPacket,
};

struct Counting;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards to the system allocator; only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const RING: usize = 256;

/// A fixed-capacity ring of outstanding packets (no allocation).
struct Ring {
    slots: [(SentPacket, PacketDeliveryState); RING],
    head: usize,
    len: usize,
}

fn step(bbr: &mut Bbrv3, ring: &mut Ring, now: &mut u64, pn: &mut u64, loss_every: u64) {
    // Send while the guard allows, paced by EDT.
    while bbr.can_send() && ring.len < RING {
        let launch = bbr.schedule_edt_ns(1200, *now);
        *now = launch;
        let packet = SentPacket {
            pn: *pn,
            sent_ns: launch,
            bytes: 1200,
            ack_eliciting: true,
            in_flight: true,
        };
        *pn += 1;
        let state = bbr.on_packet_sent(launch, packet).unwrap();
        let tail = (ring.head + ring.len) % RING;
        ring.slots[tail] = (packet, state);
        ring.len += 1;
    }
    // ACK (or lose) the oldest packet one fixed RTT after it was sent.
    let (packet, state) = ring.slots[ring.head];
    ring.head = (ring.head + 1) % RING;
    ring.len -= 1;
    *now = (*now).max(packet.sent_ns + 20_000_000);
    if loss_every != 0 && packet.pn % loss_every == 0 {
        bbr.on_ecn_ce(*now, 1).unwrap();
        bbr.on_packet_lost(
            *now,
            LostPacket {
                pn: packet.pn,
                sent_ns: packet.sent_ns,
                bytes: packet.bytes,
            },
            state,
        )
        .unwrap();
    } else {
        let acked = [Delivered {
            packet: AckedPacket {
                pn: packet.pn,
                sent_ns: packet.sent_ns,
                bytes: packet.bytes,
            },
            state,
            previously_lost: false,
        }];
        let rtt = *now - packet.sent_ns;
        bbr.on_ack(
            *now,
            &acked,
            Some(RttSample {
                latest_ns: rtt,
                min_ns: 20_000_000,
                smoothed_ns: rtt,
                ack_delay_ns: 0,
            }),
        )
        .unwrap();
    }
}

#[test]
fn steady_state_ack_and_pacing_paths_do_not_allocate() {
    let mut bbr = Bbrv3::new(BbrConfig::quic(1200), 0).unwrap();
    let empty = SentPacket {
        pn: 0,
        sent_ns: 0,
        bytes: 0,
        ack_eliciting: false,
        in_flight: false,
    };
    let blank = bbr.on_packet_sent(0, empty).unwrap();
    let mut ring = Ring {
        slots: [(empty, blank); RING],
        head: 0,
        len: 0,
    };
    let (mut now, mut pn) = (1u64, 1u64);
    // Warm-up: Startup, Drain, into ProbeBW.
    for _ in 0..5_000 {
        step(&mut bbr, &mut ring, &mut now, &mut pn, 0);
    }
    assert!(bbr.full_bw_reached());
    COUNTING.store(true, Ordering::SeqCst);
    // Steady state across ProbeBW phases and ProbeRTT (> 10 s of virtual
    // time), with periodic loss and CE marks.
    for _ in 0..200_000 {
        step(&mut bbr, &mut ring, &mut now, &mut pn, 97);
    }
    COUNTING.store(false, Ordering::SeqCst);
    assert!(now > 15_000_000_000, "covered a ProbeRTT interval: {now}");
    assert_eq!(
        ALLOCS.load(Ordering::SeqCst),
        0,
        "heap allocations in steady state"
    );
}
