//! Seed-246 regression (wave six, issue #247): the adapter's pacing schedule
//! never leads the clock by more than its configured lead, so a collapsed
//! BBR pacing rate cannot push every send out of the launch horizon.

use vesper_warp::cc::QuicBbr;
use vesper_warp_bbr::BbrConfig;

const MS: u64 = 1_000_000;

#[test]
fn the_pacing_schedule_stays_within_its_lead() {
    let lead = MS;
    let mut cc = QuicBbr::new(BbrConfig::quic(1200), 0, 64)
        .expect("config")
        .with_max_lead_ns(lead);
    let now = 10 * MS;
    // Thousands of back-to-back packets at one instant run BBR's own
    // schedule far past the lead.
    let mut last = 0;
    for _ in 0..5_000 {
        last = cc.schedule_edt_ns(1200, now);
        assert!(last <= now + lead, "launch {last} beyond the lead");
        assert!(
            cc.next_edt_ns(now) <= now + lead,
            "schedule beyond the lead"
        );
    }
    assert!(last > now, "the schedule still paces");
    assert!(
        cc.controller().next_edt_ns() > now + lead,
        "BBR's own schedule did run ahead, so the cap was exercised"
    );
}
