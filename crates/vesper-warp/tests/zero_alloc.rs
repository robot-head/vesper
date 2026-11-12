//! Steady-state receive/ack/recovery path performs zero heap allocations,
//! measured with a counting global allocator on this test's thread after
//! warm-up. This is the only test in the binary so no other test thread
//! allocates concurrently; counting is per-thread regardless.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use vesper_warp::PacketType;
use vesper_warp::PnSpace;
use vesper_warp::ack::{Ecn, ReceivedPackets};
use vesper_warp::frame::{AckFrame, AckRange, Frame, StreamFrame, scan_payload};
use vesper_warp::header::{HeaderParser, ParsedPacket};
use vesper_warp::packet_number::{PnLen, encoded_len};
use vesper_warp::recovery::{
    CongestionEvent, HandshakeStatus, LossRecovery, RecoveryConfig, SentPacket,
};

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            ALLOCS.with(|a| a.set(a.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            ALLOCS.with(|a| a.set(a.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const DCID: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

struct Path {
    parser: HeaderParser,
    rx: ReceivedPackets<32>,
    recovery: LossRecovery,
    packet: [u8; 256],
    ack_out: [u8; 64],
    events: u64,
}

impl Path {
    fn step(&mut self, i: u64) {
        let now = 1_000_000 + i * 100_000;
        // Build the "received" packet in place: short header + ACK + STREAM.
        let pn_len = encoded_len(i, i.checked_sub(1)).unwrap_or(PnLen::new(4).unwrap());
        let header_len = self
            .parser
            .write_short(&mut self.packet, &DCID, false, false, i, pn_len)
            .unwrap();
        let ranges = [AckRange {
            smallest: i,
            largest: i,
        }];
        let ack = Frame::Ack(AckFrame::from_ranges(&ranges, 3, None).unwrap());
        let mut end = header_len + ack.encode(&mut self.packet[header_len..]).unwrap();
        let stream = Frame::Stream(StreamFrame {
            stream_id: 4,
            offset: i * 100,
            data: &[0xab; 100],
            fin: false,
            has_length: true,
        });
        end += stream.encode(&mut self.packet[end..]).unwrap();
        let datagram = &self.packet[..end];

        // Receive path (header protection and AEAD are identity here).
        let ParsedPacket::Protected(h) = self.parser.parse(datagram).unwrap() else {
            panic!()
        };
        let u = h.packet_number_field(datagram).unwrap();
        let pn = u.pn.decode(self.rx.largest()).unwrap();
        assert_eq!(pn, i);
        let payload = u.payload.of(datagram);
        let mut acked_largest = None;
        let info = scan_payload(payload, PacketType::OneRtt, |f| {
            if let Frame::Ack(a) = f {
                acked_largest = Some(*a);
            }
        })
        .unwrap();
        self.rx
            .on_packet_received(pn, now, info.ack_eliciting, Ecn::Ect0)
            .unwrap();

        // Our own send and the peer's ACK of it.
        self.recovery
            .on_packet_sent(
                PnSpace::ApplicationData,
                SentPacket {
                    pn: i,
                    sent_ns: now,
                    bytes: 1200,
                    ack_eliciting: true,
                    in_flight: true,
                },
            )
            .unwrap();
        let ack = acked_largest.unwrap();
        let events = &mut self.events;
        self.recovery
            .on_ack_received(
                PnSpace::ApplicationData,
                &ack,
                0,
                now + 50_000,
                &mut |e: CongestionEvent| {
                    if matches!(e, CongestionEvent::Acked { .. }) {
                        *events += 1;
                    }
                },
            )
            .unwrap();

        // Generate our ACK.
        let out = self.rx.ack_frame(now + 10_000, 3).unwrap().unwrap();
        Frame::Ack(out).encode(&mut self.ack_out).unwrap();
        self.rx.on_ack_sent();
        if i.is_multiple_of(16) {
            self.rx.on_ack_of_ack(i.saturating_sub(4));
        }
    }
}

#[test]
fn steady_state_receive_and_recovery_do_not_allocate() {
    let status = HandshakeStatus {
        confirmed: true,
        peer_completed_address_validation: true,
        has_handshake_keys: true,
        at_amplification_limit: false,
    };
    let mut path = Path {
        parser: HeaderParser::new(8).unwrap(),
        rx: ReceivedPackets::new(PnSpace::ApplicationData, 25_000_000),
        recovery: LossRecovery::new(RecoveryConfig::default(), status),
        packet: [0; 256],
        ack_out: [0; 64],
        events: 0,
    };
    for i in 0..1_000 {
        path.step(i);
    }
    // Self-check: the counter does see an allocation on this thread.
    COUNTING.with(|c| c.set(true));
    drop(std::hint::black_box(Box::new(7u64)));
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCS.with(Cell::get), 1, "counting allocator is live");
    ALLOCS.with(|a| a.set(0));

    COUNTING.with(|c| c.set(true));
    for i in 1_000..20_000 {
        path.step(i);
    }
    COUNTING.with(|c| c.set(false));
    let allocations = ALLOCS.with(Cell::get);
    assert_eq!(path.events, 20_000);
    assert_eq!(allocations, 0, "steady-state allocations");
}
