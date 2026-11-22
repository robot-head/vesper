//! 1-RTT keys with RFC 9001 §6 key updates.
//!
//! Generation `g` (the number of key updates) uses key-phase bit `g & 1`.
//! Send and receive generations are tracked separately:
//!
//! * [`OneRttKeys::initiate_key_update`] moves the send side to `g + 1`. It
//!   is permitted only once the peer has responded to the previous update
//!   (receive generation caught up) and a packet sent with the current send
//!   keys has been acknowledged (RFC 9001 §6.1).
//! * The receive side keeps the current key, the precomputed next key
//!   (RFC 9001 §6.3: no key derivation at trial-decryption time) and, after
//!   an update, the previous key for `retire_window_ns` (RFC 9001 §6.5
//!   recommends three times the PTO; the caller supplies the value and the
//!   clock).
//! * A packet whose phase bit differs from the current one is opened with
//!   the previous key if its packet number is below the first packet number
//!   received in the current phase, and with the next key otherwise
//!   (RFC 9001 §6.4/§6.5). Successfully opening with the next key promotes
//!   it; if the peer initiated, the send side follows (RFC 9001 §6.2).
//! * Failed openings count toward the integrity limit across all keys.
//!
//! Handshake confirmation (required before any update) and the
//! KEY_UPDATE_ERROR policy for consecutive updates are the connection's
//! responsibility and are not modelled here.

use std::mem;

use crate::protect::{HeaderKey, LONG_HEADER_BIT, PacketKey, Unprotected};
use crate::schedule::TrafficSecret;
use crate::ticket::{NonceTicket, SealError, Sealer, TicketAudit};
use crate::{Cipher, CryptoError, check_pn};

struct RecvPhase {
    key: PacketKey,
    generation: u64,
    /// Smallest packet number opened in this phase.
    first_pn: Option<u64>,
}

/// A successfully opened 1-RTT packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OneRttOpened {
    pub packet_number: u64,
    pub header_len: usize,
    pub payload_len: usize,
    pub first_byte: u8,
    /// Receive generation whose key opened the packet.
    pub generation: u64,
    /// This packet moved the receive side to a new key phase.
    pub promoted: bool,
}

enum Choice {
    Current,
    Previous,
    Next,
}

/// Both directions of 1-RTT packet protection for one connection.
pub struct OneRttKeys {
    sealer: Sealer,
    /// Secret for send generation `sealer.generation() + 1`.
    send_next_secret: TrafficSecret,
    send_phase_first_pn: u64,
    send_phase_acked: bool,

    recv_hp: HeaderKey,
    recv_current: RecvPhase,
    recv_previous: Option<(RecvPhase, u64)>,
    recv_next: PacketKey,
    /// Secret for receive generation `recv_current.generation + 2`.
    recv_after_next_secret: TrafficSecret,

    failures: u64,
    integrity_limit: u64,
    retire_window_ns: u64,
}

impl OneRttKeys {
    /// `local` seals, `remote` opens; both are generation-0 application
    /// traffic secrets from TLS. `retire_window_ns` bounds how long the
    /// previous receive key is kept after an update.
    pub fn new(
        local: TrafficSecret,
        remote: TrafficSecret,
        retire_window_ns: u64,
    ) -> Result<Self, CryptoError> {
        let cipher = local.cipher();
        if remote.cipher() != cipher {
            return Err(CryptoError::CipherMismatch);
        }
        let send_keys = local.keys();
        let send_next_secret = local.next_phase();
        let remote_keys = remote.keys();
        let remote_next = remote.next_phase();
        Ok(Self {
            sealer: Sealer::with_phase(send_keys, Some(false)),
            send_next_secret,
            send_phase_first_pn: 0,
            send_phase_acked: false,
            recv_hp: remote_keys.header,
            recv_current: RecvPhase {
                key: remote_keys.packet,
                generation: 0,
                first_pn: None,
            },
            recv_previous: None,
            recv_next: remote_next.keys().packet,
            recv_after_next_secret: remote_next.next_phase(),
            failures: 0,
            integrity_limit: cipher.integrity_limit(),
            retire_window_ns,
        })
    }

    /// Lowers the per-key confidentiality limit (tests, local policy).
    pub fn with_confidentiality_limit(mut self, limit: u64) -> Self {
        self.sealer = self.sealer.with_confidentiality_limit(limit);
        self
    }

    /// Lowers the connection integrity limit (tests, local policy).
    pub fn with_integrity_limit(mut self, limit: u64) -> Self {
        self.integrity_limit = self.integrity_limit.min(limit);
        self
    }

    pub fn cipher(&self) -> Cipher {
        self.sealer.cipher()
    }

    /// Key-phase bit the next sealed packet's short header must carry.
    pub fn send_key_phase(&self) -> bool {
        self.sealer.key_phase().unwrap_or(false)
    }

    pub fn send_generation(&self) -> u64 {
        self.sealer.generation()
    }

    pub fn recv_generation(&self) -> u64 {
        self.recv_current.generation
    }

    /// Whether the previous receive key is still retained.
    pub fn has_previous_recv_key(&self) -> bool {
        self.recv_previous.is_some()
    }

    pub fn authentication_failures(&self) -> u64 {
        self.failures
    }

    pub fn audit(&self) -> TicketAudit {
        self.sealer.audit()
    }

    pub fn ticket(&mut self, pn: u64) -> Result<NonceTicket, CryptoError> {
        self.sealer.ticket(pn)
    }

    pub fn next_ticket(&mut self) -> Result<NonceTicket, CryptoError> {
        self.sealer.next_ticket()
    }

    pub fn abandon(&mut self, ticket: NonceTicket) -> Result<(), NonceTicket> {
        self.sealer.abandon(ticket)
    }

    /// Seals a short-header packet; see [`Sealer::seal`]. The header's
    /// key-phase bit must equal [`Self::send_key_phase`].
    pub fn seal(
        &mut self,
        ticket: NonceTicket,
        buf: &mut [u8],
        pn_offset: usize,
        payload_len: usize,
    ) -> Result<usize, SealError> {
        self.sealer.seal(ticket, buf, pn_offset, payload_len)
    }

    /// Records that the peer acknowledged `pn`. Acknowledging a packet sent
    /// under the current send keys permits the next key update.
    pub fn on_packet_acked(&mut self, pn: u64) {
        if pn >= self.send_phase_first_pn && pn < self.sealer.next_packet_number() {
            self.send_phase_acked = true;
        }
    }

    /// Whether [`Self::initiate_key_update`] would succeed.
    pub fn key_update_permitted(&self) -> bool {
        self.sealer.generation() == self.recv_current.generation && self.send_phase_acked
    }

    /// Starts a key update (RFC 9001 §6.1). Outstanding tickets become
    /// stale and must be abandoned.
    pub fn initiate_key_update(&mut self) -> Result<(), CryptoError> {
        if !self.key_update_permitted() {
            return Err(CryptoError::KeyUpdateNotPermitted);
        }
        self.rotate_send();
        Ok(())
    }

    fn rotate_send(&mut self) {
        let after = self.send_next_secret.next_phase();
        let secret = mem::replace(&mut self.send_next_secret, after);
        self.sealer.rekey(secret.keys().packet);
        self.send_phase_first_pn = self.sealer.next_packet_number();
        self.send_phase_acked = false;
    }

    /// Drops the previous receive key once its window has passed.
    pub fn discard_expired(&mut self, now_ns: u64) {
        if matches!(self.recv_previous, Some((_, deadline)) if now_ns >= deadline) {
            self.recv_previous = None;
        }
    }

    /// Removes header protection, reconstructs the packet number through
    /// `reconstruct`, selects the key by phase bit and opens. On any error
    /// header protection is re-applied (so `buf` is byte-identical to its
    /// input) and no key state changes, except the §6.6 failure counter on
    /// authentication failure.
    pub fn open_packet(
        &mut self,
        buf: &mut [u8],
        pn_offset: usize,
        now_ns: u64,
        reconstruct: impl FnOnce(&Unprotected) -> Option<u64>,
    ) -> Result<OneRttOpened, CryptoError> {
        if self.failures >= self.integrity_limit {
            return Err(CryptoError::IntegrityLimitReached);
        }
        self.discard_expired(now_ns);
        let hdr = self.recv_hp.remove(buf, pn_offset)?;
        match self.open_unprotected(buf, &hdr, now_ns, reconstruct) {
            Ok(opened) => Ok(opened),
            Err(e) => {
                hdr.reapply(buf);
                Err(e)
            }
        }
    }

    fn open_unprotected(
        &mut self,
        buf: &mut [u8],
        hdr: &Unprotected,
        now_ns: u64,
        reconstruct: impl FnOnce(&Unprotected) -> Option<u64>,
    ) -> Result<OneRttOpened, CryptoError> {
        if hdr.first_byte & LONG_HEADER_BIT != 0 {
            return Err(CryptoError::InvalidHeaderForm);
        }
        let pn = reconstruct(hdr).ok_or(CryptoError::PacketNumberReconstruction)?;
        check_pn(pn)?;
        if !hdr.encodes(pn) {
            return Err(CryptoError::PacketNumberMismatch);
        }
        let phase = hdr.key_phase() == Some(true);
        let current_phase = self.recv_current.generation & 1 == 1;
        let below_current = self.recv_current.first_pn.is_some_and(|f| pn < f);
        let choice = if phase == current_phase {
            Choice::Current
        } else if below_current {
            if self.recv_previous.is_none() {
                return Err(CryptoError::KeyRetired);
            }
            Choice::Previous
        } else {
            Choice::Next
        };
        let (key, generation) = match &choice {
            Choice::Current => (&self.recv_current.key, self.recv_current.generation),
            Choice::Previous => {
                let (prev, _) = self.recv_previous.as_ref().expect("checked above");
                (&prev.key, prev.generation)
            }
            Choice::Next => (&self.recv_next, self.recv_current.generation + 1),
        };
        let payload_len = match key.open(pn, buf, hdr.header_len()) {
            Ok(n) => n,
            Err(e) => {
                if e == CryptoError::AuthenticationFailed {
                    self.failures += 1;
                }
                return Err(e);
            }
        };
        let promoted = matches!(choice, Choice::Next);
        match choice {
            Choice::Current => {
                let f = &mut self.recv_current.first_pn;
                *f = Some(f.map_or(pn, |f| f.min(pn)));
            }
            Choice::Previous => {}
            Choice::Next => self.promote(pn, now_ns),
        }
        Ok(OneRttOpened {
            packet_number: pn,
            header_len: hdr.header_len(),
            payload_len,
            first_byte: hdr.first_byte,
            generation,
            promoted,
        })
    }

    fn promote(&mut self, pn: u64, now_ns: u64) {
        let after = self.recv_after_next_secret.next_phase();
        let next_secret = mem::replace(&mut self.recv_after_next_secret, after);
        let next_key = mem::replace(&mut self.recv_next, next_secret.keys().packet);
        let generation = self.recv_current.generation + 1;
        let old = mem::replace(
            &mut self.recv_current,
            RecvPhase {
                key: next_key,
                generation,
                first_pn: Some(pn),
            },
        );
        self.recv_previous = Some((old, now_ns.saturating_add(self.retire_window_ns)));
        if self.sealer.generation() < generation {
            // Peer-initiated update: respond with our next keys (§6.2).
            self.rotate_send();
        }
    }
}
