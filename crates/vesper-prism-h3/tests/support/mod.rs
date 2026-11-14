//! An in-memory QUIC stream pair for deterministic HTTP/3 tests. It models
//! stream IDs, ordered bytes, FIN, RESET_STREAM, STOP_SENDING and
//! CONNECTION_CLOSE codes, and optionally QUIC DATAGRAM frames; there is
//! no loss or flow control (Warp tests cover those). A per-side write
//! budget and write log let scheduler tests see the order bytes leave in.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use vesper_prism_h3::error::Code;
use vesper_prism_h3::transport::{ReadOutcome, Transport, TransportError};

#[derive(Default)]
struct Half {
    data: VecDeque<u8>,
    fin: bool,
    reset: Option<u64>,
    /// The receiver asked the sender to stop (STOP_SENDING code).
    stopped: Option<u64>,
}

#[derive(Default)]
struct Shared {
    /// (stream id, sending side 0 = client / 1 = server) -> half.
    halves: BTreeMap<(u64, usize), Half>,
    accept: [VecDeque<u64>; 2],
    next: [[u64; 2]; 2],
    closed: [Option<Code>; 2],
    /// Datagrams in flight towards each side.
    datagrams: [VecDeque<Vec<u8>>; 2],
}

pub struct Mem {
    shared: Rc<RefCell<Shared>>,
    side: usize,
    /// Max bytes accepted per write call (0 = unlimited).
    pub write_limit: usize,
    /// Unidirectional streams this side may still open (None = unlimited).
    pub uni_credit: Option<usize>,
    /// QUIC DATAGRAM support: the largest datagram (None = no datagrams).
    pub max_datagram: Option<usize>,
    /// Bytes this side may still write in total (None = unlimited).
    pub budget: Option<usize>,
    /// Every accepted write: (stream id, bytes).
    pub log: Vec<(u64, usize)>,
}

pub fn pair() -> (Mem, Mem) {
    let shared = Rc::new(RefCell::new(Shared::default()));
    (
        Mem {
            shared: shared.clone(),
            side: 0,
            write_limit: 0,
            uni_credit: None,
            max_datagram: None,
            budget: None,
            log: Vec::new(),
        },
        Mem {
            shared,
            side: 1,
            write_limit: 0,
            uni_credit: None,
            max_datagram: None,
            budget: None,
            log: Vec::new(),
        },
    )
}

impl Mem {
    fn open(&mut self, uni: bool) -> u64 {
        let mut s = self.shared.borrow_mut();
        let k = uni as usize;
        let id = s.next[self.side][k] << 2 | (uni as u64) << 1 | self.side as u64;
        s.next[self.side][k] += 1;
        s.halves.entry((id, self.side)).or_default();
        if !uni {
            s.halves.entry((id, 1 - self.side)).or_default();
        }
        s.accept[1 - self.side].push_back(id);
        id
    }

    pub fn closed(&self) -> Option<Code> {
        self.shared.borrow().closed[self.side]
    }
    pub fn peer_closed(&self) -> Option<Code> {
        self.shared.borrow().closed[1 - self.side]
    }
    /// The reset code our peer put on its sending half of `id`.
    pub fn peer_reset(&self, id: u64) -> Option<u64> {
        self.shared
            .borrow()
            .halves
            .get(&(id, 1 - self.side))
            .and_then(|h| h.reset)
    }
    /// STOP_SENDING our peer sent for our sending half of `id`.
    pub fn stopped(&self, id: u64) -> Option<u64> {
        self.shared
            .borrow()
            .halves
            .get(&(id, self.side))
            .and_then(|h| h.stopped)
    }
    /// Raw write that bypasses HTTP/3 (for malformed-input tests).
    pub fn raw_open(&mut self, uni: bool) -> u64 {
        self.open(uni)
    }
    /// Bytes waiting to be read on our peer's sending half of `id`.
    pub fn pending(&self, id: u64) -> usize {
        self.shared
            .borrow()
            .halves
            .get(&(id, 1 - self.side))
            .map_or(0, |h| h.data.len())
    }
    /// Bytes we wrote on `id` that the peer has not read yet.
    pub fn unread(&self, id: u64) -> usize {
        self.shared
            .borrow()
            .halves
            .get(&(id, self.side))
            .map_or(0, |h| h.data.len())
    }
    /// A raw datagram towards the peer (bypasses HTTP/3).
    pub fn raw_datagram(&mut self, d: &[u8]) {
        self.shared.borrow_mut().datagrams[1 - self.side].push_back(d.to_vec());
    }
}

impl Transport for Mem {
    fn open_uni(&mut self) -> Result<u64, TransportError> {
        match &mut self.uni_credit {
            Some(0) => return Err(TransportError::NotReady),
            Some(n) => *n -= 1,
            None => {}
        }
        Ok(self.open(true))
    }
    fn open_bidi(&mut self) -> Result<u64, TransportError> {
        Ok(self.open(false))
    }
    fn write(&mut self, id: u64, data: &[u8]) -> Result<usize, TransportError> {
        let mut s = self.shared.borrow_mut();
        if s.closed.iter().any(Option::is_some) {
            return Err(TransportError::Closed);
        }
        let h = s
            .halves
            .get_mut(&(id, self.side))
            .ok_or(TransportError::UnknownStream)?;
        if let Some(code) = h.reset {
            return Err(TransportError::Reset(code));
        }
        if h.fin {
            return Err(TransportError::UnknownStream);
        }
        let mut n = if self.write_limit == 0 {
            data.len()
        } else {
            data.len().min(self.write_limit)
        };
        if let Some(b) = &mut self.budget {
            n = n.min(*b);
            *b -= n;
        }
        h.data.extend(&data[..n]);
        if n > 0 {
            self.log.push((id, n));
        }
        Ok(n)
    }
    fn finish(&mut self, id: u64) -> Result<(), TransportError> {
        let mut s = self.shared.borrow_mut();
        let h = s
            .halves
            .get_mut(&(id, self.side))
            .ok_or(TransportError::UnknownStream)?;
        h.fin = true;
        Ok(())
    }
    fn read(&mut self, id: u64, out: &mut [u8]) -> Result<ReadOutcome, TransportError> {
        let mut s = self.shared.borrow_mut();
        let h = s
            .halves
            .get_mut(&(id, 1 - self.side))
            .ok_or(TransportError::UnknownStream)?;
        if let Some(code) = h.reset {
            return Err(TransportError::Reset(code));
        }
        let n = out.len().min(h.data.len());
        for (o, b) in out.iter_mut().zip(h.data.drain(..n)) {
            *o = b;
        }
        Ok(ReadOutcome {
            len: n,
            fin: h.fin && h.data.is_empty(),
        })
    }
    fn reset(&mut self, id: u64, code: Code) {
        let mut s = self.shared.borrow_mut();
        if let Some(h) = s.halves.get_mut(&(id, self.side))
            && !h.fin
        {
            h.reset = Some(code.0);
            h.data.clear();
        }
    }
    fn stop_sending(&mut self, id: u64, code: Code) {
        let mut s = self.shared.borrow_mut();
        if let Some(h) = s.halves.get_mut(&(id, 1 - self.side)) {
            h.stopped = Some(code.0);
            // A QUIC sender answers STOP_SENDING with RESET_STREAM.
            if !(h.fin && h.data.is_empty()) {
                h.reset = Some(code.0);
                h.data.clear();
            }
        }
    }
    fn accept(&mut self) -> Option<u64> {
        self.shared.borrow_mut().accept[self.side].pop_front()
    }
    fn close(&mut self, code: Code) {
        self.shared.borrow_mut().closed[self.side].get_or_insert(code);
    }
    fn max_datagram_size(&mut self) -> Option<usize> {
        self.max_datagram
    }
    fn send_datagram(&mut self, data: &[u8]) -> Result<(), TransportError> {
        match self.max_datagram {
            None => Err(TransportError::Unsupported),
            Some(m) if data.len() > m => Err(TransportError::TooLarge),
            Some(_) => {
                self.shared.borrow_mut().datagrams[1 - self.side].push_back(data.to_vec());
                Ok(())
            }
        }
    }
    fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        self.shared.borrow_mut().datagrams[self.side].pop_front()
    }
}
