//! Two Warp endpoints over lane 3's software NIC, joined by a test wire.
#![allow(dead_code)]

use vesper_warp::endpoint::{EndpointConfig, Responder, RxOutcome, TxTap, WarpEndpoint};
use vesper_warp_crypto::{Cipher, OneRttKeys, TrafficSecret};
use vesper_warp_shard::SlotHandle;
use vesper_warp_umem::{SoftNic, SoftNicMode, UmemConfig};

pub const MS: u64 = 1_000_000;
pub const CLIENT_CID: [u8; 8] = *b"client01";
pub const SERVER_CID: [u8; 8] = *b"server01";
const CLIENT_SECRET: [u8; 32] = [0x11; 32];
const SERVER_SECRET: [u8; 32] = [0x22; 32];

/// Datagrams in a fixed ring of preallocated buffers, so capturing
/// allocates nothing once built.
pub struct Wire {
    bufs: Vec<[u8; 2048]>,
    lens: Vec<usize>,
    head: usize,
    len: usize,
    /// Every datagram's (pn, edt) in transmit order.
    pub log: Vec<(u64, u64)>,
    pub log_enabled: bool,
    /// Drop the next datagram whose pn equals this.
    pub drop_pn: Option<u64>,
    pub dropped: u64,
    pub captured: u64,
}

impl Wire {
    pub fn new(capacity: usize) -> Self {
        Self {
            bufs: vec![[0; 2048]; capacity],
            lens: vec![0; capacity],
            head: 0,
            len: 0,
            log: Vec::new(),
            log_enabled: true,
            drop_pn: None,
            dropped: 0,
            captured: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Removes the oldest datagram into `out`, returning its length.
    pub fn pop_into(&mut self, out: &mut [u8; 2048]) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let n = self.lens[self.head];
        out[..n].copy_from_slice(&self.bufs[self.head][..n]);
        self.head = (self.head + 1) % self.bufs.len();
        self.len -= 1;
        Some(n)
    }
}

impl TxTap for Wire {
    fn on_transmit(&mut self, _conn: SlotHandle, pn: u64, edt_ns: u64, datagram: &[u8]) {
        if self.log_enabled {
            self.log.push((pn, edt_ns));
        }
        if self.drop_pn == Some(pn) {
            self.drop_pn = None;
            self.dropped += 1;
            return;
        }
        assert!(self.len < self.bufs.len(), "test wire full");
        let tail = (self.head + self.len) % self.bufs.len();
        self.bufs[tail][..datagram.len()].copy_from_slice(datagram);
        self.lens[tail] = datagram.len();
        self.len += 1;
        self.captured += 1;
    }
}

pub type Endpoint<R> = WarpEndpoint<SoftNic, R, Wire>;

pub fn endpoint<R: Responder>(config: EndpointConfig, app: R) -> Endpoint<R> {
    let umem = UmemConfig::with_frames(512);
    let xsk = SoftNic::xsk(umem, SoftNicMode::Scripted).unwrap();
    WarpEndpoint::new(config, xsk, app, Wire::new(1024)).unwrap()
}

fn secret(bytes: &[u8; 32]) -> TrafficSecret {
    TrafficSecret::new(Cipher::Aes128Gcm, bytes).unwrap()
}

pub fn client_keys() -> OneRttKeys {
    OneRttKeys::new(secret(&CLIENT_SECRET), secret(&SERVER_SECRET), 100 * MS).unwrap()
}

pub fn server_keys() -> OneRttKeys {
    OneRttKeys::new(secret(&SERVER_SECRET), secret(&CLIENT_SECRET), 100 * MS).unwrap()
}

/// A client and a server endpoint with one connection between them.
pub struct Pair<C: Responder, S: Responder> {
    pub client: Endpoint<C>,
    pub server: Endpoint<S>,
    pub c_conn: SlotHandle,
    pub s_conn: SlotHandle,
    pub now: u64,
    /// One-way wire delay.
    pub delay_ns: u64,
    buf: Box<[u8; 2048]>,
    pub outcomes: Vec<RxOutcome>,
    pub record: bool,
}

impl<C: Responder, S: Responder> Pair<C, S> {
    pub fn new(config: EndpointConfig, client_app: C, server_app: S) -> Self {
        let mut client = endpoint(config, client_app);
        let mut server = endpoint(config, server_app);
        let c_conn = client
            .open_connection(&CLIENT_CID, &SERVER_CID, client_keys(), 0)
            .unwrap();
        let s_conn = server
            .open_connection(&SERVER_CID, &CLIENT_CID, server_keys(), 0)
            .unwrap();
        Self {
            client,
            server,
            c_conn,
            s_conn,
            now: 0,
            delay_ns: 5 * MS,
            buf: Box::new([0; 2048]),
            outcomes: Vec::new(),
            record: true,
        }
    }

    /// Delivers every datagram on both wires, runs both receive paths dry
    /// and both NICs and timers, then advances time by the wire delay.
    /// Returns the number of datagrams moved.
    pub fn step(&mut self) -> usize {
        let now = self.now;
        let mut moved = 0;
        while let Some(n) = self.client.tap_mut().pop_into(&mut self.buf) {
            self.server.xsk_mut().backend_mut().deliver(&self.buf[..n]);
            moved += 1;
        }
        while let Some(n) = self.server.tap_mut().pop_into(&mut self.buf) {
            self.client.xsk_mut().backend_mut().deliver(&self.buf[..n]);
            moved += 1;
        }
        for _ in 0..4096 {
            let a = self.server.poll_and_step_quic_rx_one(now).unwrap();
            let b = self.client.poll_and_step_quic_rx_one(now).unwrap();
            let idle = a == RxOutcome::Idle && b == RxOutcome::Idle;
            if self.record {
                for o in [a, b] {
                    if o != RxOutcome::Idle {
                        self.outcomes.push(o);
                    }
                }
            }
            if idle {
                break;
            }
        }
        self.client.xsk_mut().backend_mut().step();
        self.server.xsk_mut().backend_mut().step();
        self.client.poll(now).unwrap();
        self.server.poll(now).unwrap();
        self.now += self.delay_ns;
        moved
    }

    /// Steps until both wires are empty and nothing is queued or parked, or
    /// `max_steps` pass. Returns whether it settled.
    pub fn settle(&mut self, max_steps: usize) -> bool {
        for _ in 0..max_steps {
            self.step();
            if self.client.tap().is_empty()
                && self.server.tap().is_empty()
                && self.client.queued_launches() == 0
                && self.server.queued_launches() == 0
                && self.client.parked_leases() == 0
                && self.server.parked_leases() == 0
            {
                return true;
            }
        }
        false
    }
}
