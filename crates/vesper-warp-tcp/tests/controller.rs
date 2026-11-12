//! Congestion-controller contract: event accounting and EDT handling.

mod common;
use common::*;
use std::cell::RefCell;
use std::rc::Rc;
use vesper_warp_tcp::cc::{CongestionController, CongestionEvent, RttSample, SentPacket};
use vesper_warp_tcp::harness::{Harness, LinkConfig};
use vesper_warp_tcp::{Connection, NewReno, State};

/// NewReno that schedules every departure `delay_ns` ahead and records the
/// `bytes_in_flight` reported with each acknowledgment.
struct Paced {
    inner: NewReno,
    delay_ns: u64,
    acked_flight: Rc<RefCell<Vec<u32>>>,
}

impl CongestionController for Paced {
    fn on_mss(&mut self, smss: u32) {
        self.inner.on_mss(smss);
    }
    fn on_sent(&mut self, p: SentPacket) {
        self.inner.on_sent(p);
    }
    fn on_rtt_sample(&mut self, s: RttSample) {
        self.inner.on_rtt_sample(s);
    }
    fn on_event(&mut self, e: CongestionEvent, now: u64) {
        if let CongestionEvent::Acked {
            bytes_in_flight, ..
        } = e
        {
            self.acked_flight.borrow_mut().push(bytes_in_flight);
        }
        self.inner.on_event(e, now);
    }
    fn cwnd_bytes(&self) -> u32 {
        self.inner.cwnd_bytes()
    }
    fn pacing_rate_bytes_per_sec(&self) -> u64 {
        self.inner.pacing_rate_bytes_per_sec()
    }
    fn schedule_edt_ns(&mut self, _bytes: u32, now_ns: u64) -> u64 {
        now_ns + self.delay_ns
    }
}

fn paced(port: u16, delay_ns: u64, log: &Rc<RefCell<Vec<u32>>>) -> Connection<Paced> {
    let mut c = cfg(port);
    c.max_retransmits = 2;
    c.rto.initial_ns = 100 * MS;
    c.rto.min_ns = 50 * MS;
    let inner = NewReno::new(u32::from(c.mss), c.initial_window_segments);
    Connection::with_controller(
        c,
        Paced {
            inner,
            delay_ns,
            acked_flight: Rc::clone(log),
        },
    )
    .unwrap()
}

#[test]
fn pacing_delay_is_not_mistaken_for_loss_or_rtt() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut h = Harness::new(
        paced(40000, 2 * SEC, &log),
        paced(80, 0, &log),
        LinkConfig::default(),
    )
    .unwrap();
    assert!(
        h.run_until(30 * SEC, |h| h.both(State::Established))
            .unwrap(),
        "{:?} {:?}",
        h.a.conn.state(),
        h.a.conn.close_reason()
    );
    assert_eq!(h.a.conn.stats().rtos, 0);
    // RTT excludes the 2 s pacing delay (link RTT is ~10 ms).
    assert!(
        h.a.conn.rtt().srtt_ns() < 100 * MS,
        "{}",
        h.a.conn.rtt().srtt_ns()
    );
}

#[test]
fn acked_events_report_post_ack_bytes_in_flight() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut h = Harness::new(
        paced(40000, 0, &log),
        paced(80, 0, &log),
        LinkConfig::default(),
    )
    .unwrap();
    assert!(h.run_until(SEC, |h| h.both(State::Established)).unwrap());
    log.borrow_mut().clear();
    h.a.write(&[1u8; 100]).unwrap();
    assert!(
        h.run_until(5 * SEC, |h| h.b.received.len() == 100
            && h.a.conn.bytes_in_flight() == 0)
            .unwrap()
    );
    assert_eq!(
        *log.borrow(),
        vec![0],
        "the only outstanding segment is acknowledged: nothing left in flight"
    );
}
