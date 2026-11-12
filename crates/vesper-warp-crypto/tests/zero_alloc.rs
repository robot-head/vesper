//! Seal/open on the warmed-up hot path perform zero heap allocations.
//!
//! The counting allocator counts only on the thread that enables it, so
//! the test harness's own threads cannot perturb the result.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use common::decode_pn;
use vesper_warp_crypto::{Cipher, InitialSecrets, OneRttKeys, Opener, Sealer, Side, TrafficSecret};

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
}

fn note() {
    if ENABLED.try_with(Cell::get).unwrap_or(false) {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

// The library is `#![forbid(unsafe_code)]`; this test-only allocator is the
// sole `unsafe` in the crate. SAFETY: every method forwards unchanged to
// `System`, which upholds the `GlobalAlloc` contract.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn counted<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let before = ALLOCS.load(Ordering::Relaxed);
    ENABLED.with(|e| e.set(true));
    let r = f();
    ENABLED.with(|e| e.set(false));
    (r, ALLOCS.load(Ordering::Relaxed) - before)
}

const PN_OFFSET: usize = 9;
const PAYLOAD: usize = 1200;

fn write_short(buf: &mut [u8], phase: bool, pn: u64) {
    buf[0] = 0x40 | (u8::from(phase) << 2) | 0x01;
    buf[1..9].fill(0xcd);
    buf[9..11].copy_from_slice(&(pn as u16).to_be_bytes());
    buf[11..11 + PAYLOAD].fill(pn as u8);
}

#[test]
fn hot_path_allocates_nothing() {
    // Positive control: the counter sees allocations on this thread.
    let (v, allocs) = counted(|| vec![1u8; 64]);
    assert!(allocs > 0 && v.len() == 64);

    for cipher in [
        Cipher::Aes128Gcm,
        Cipher::Aes256Gcm,
        Cipher::ChaCha20Poly1305,
    ] {
        let n = cipher.secret_len();
        let mk = |f: u8| TrafficSecret::new(cipher, &vec![f; n]).unwrap();
        let mut c = OneRttKeys::new(mk(1), mk(2), 1_000).unwrap();
        let mut s = OneRttKeys::new(mk(2), mk(1), 1_000).unwrap();
        let mut buf = vec![0u8; 11 + PAYLOAD + 16];

        let mut round = |c: &mut OneRttKeys, s: &mut OneRttKeys, pn: u64| {
            write_short(&mut buf, c.send_key_phase(), pn);
            let t = c.ticket(pn).unwrap();
            c.seal(t, &mut buf, PN_OFFSET, PAYLOAD).unwrap();
            let o = s
                .open_packet(&mut buf, PN_OFFSET, pn, |h| {
                    Some(decode_pn(pn.checked_sub(1), h.truncated_pn, h.pn_len))
                })
                .unwrap();
            assert_eq!(o.payload_len, PAYLOAD);
        };
        for pn in 0..16 {
            round(&mut c, &mut s, pn); // warm-up
        }
        let ((), allocs) = counted(|| {
            for pn in 16..1_016 {
                round(&mut c, &mut s, pn);
            }
            // A key update (derivation and promotion) is on-stack too.
            c.on_packet_acked(1_015);
            c.initiate_key_update().unwrap();
            for pn in 1_016..1_100 {
                round(&mut c, &mut s, pn);
            }
        });
        assert_eq!(allocs, 0, "{cipher:?} hot path allocated");
        assert_eq!(s.recv_generation(), 1);
    }

    // Initial packets through Sealer/Opener.
    let secrets = InitialSecrets::new_v1(&[7; 8]).unwrap();
    let (seal, _) = secrets.keys_for(Side::Client);
    let (_, open) = secrets.keys_for(Side::Server);
    let mut sealer = Sealer::new(seal);
    let mut opener = Opener::new(open);
    let mut buf = vec![0u8; 11 + PAYLOAD + 16];
    let mut round = |pn: u64| {
        write_short(&mut buf, false, pn);
        buf[0] = 0xc1;
        let t = sealer.next_ticket().unwrap();
        sealer.seal(t, &mut buf, PN_OFFSET, PAYLOAD).unwrap();
        opener
            .open_packet(&mut buf, PN_OFFSET, |h| {
                Some(decode_pn(pn.checked_sub(1), h.truncated_pn, h.pn_len))
            })
            .unwrap();
    };
    for pn in 0..4 {
        round(pn);
    }
    let ((), allocs) = counted(|| (4..500).for_each(&mut round));
    assert_eq!(allocs, 0, "Initial hot path allocated");
}
