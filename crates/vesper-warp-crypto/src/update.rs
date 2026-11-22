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
//!
//! The state machine is Vesper (`vesper/src/lib.ves`, `onertt_*`): it
//! selects the key, counts failures and decides when keys rotate. This
//! module holds the keys and secrets and performs the rotations.

use std::mem;

use crate::port::{b, err, scratch};
use crate::protect::{HeaderKey, PacketKey, Unprotected};
use crate::schedule::TrafficSecret;
use crate::ticket::{NonceTicket, SealError, Sealer, TicketAudit};
use crate::{Cipher, CryptoError};

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

/// Both directions of 1-RTT packet protection for one connection.
pub struct OneRttKeys {
    state: b::repr::OneRttSt,
    sealer: Sealer,
    /// Secret for send generation `sealer.generation() + 1`.
    send_next_secret: TrafficSecret,

    recv_hp: HeaderKey,
    recv_current: PacketKey,
    /// Kept while the state's `has_previous` holds.
    recv_previous: Option<PacketKey>,
    recv_next: PacketKey,
    /// Secret for receive generation `recv_generation + 2`.
    recv_after_next_secret: TrafficSecret,
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
            state: b::onertt_new(cipher.into(), retire_window_ns),
            sealer: Sealer::with_phase(send_keys, Some(false)),
            send_next_secret,
            recv_hp: remote_keys.header,
            recv_current: remote_keys.packet,
            recv_previous: None,
            recv_next: remote_next.keys().packet,
            recv_after_next_secret: remote_next.next_phase(),
        })
    }

    /// Lowers the per-key confidentiality limit (tests, local policy).
    pub fn with_confidentiality_limit(mut self, limit: u64) -> Self {
        self.sealer = self.sealer.with_confidentiality_limit(limit);
        self
    }

    /// Lowers the connection integrity limit (tests, local policy).
    pub fn with_integrity_limit(mut self, limit: u64) -> Self {
        b::onertt_lower_limit(&mut self.state, limit);
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
        self.state.recv_generation
    }

    /// Whether the previous receive key is still retained.
    pub fn has_previous_recv_key(&self) -> bool {
        self.recv_previous.is_some()
    }

    pub fn authentication_failures(&self) -> u64 {
        self.state.failures
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
        b::onertt_on_acked(&mut self.state, self.sealer.state(), pn);
    }

    /// Whether [`Self::initiate_key_update`] would succeed.
    pub fn key_update_permitted(&self) -> bool {
        b::onertt_update_permitted(&self.state, self.sealer.state())
    }

    /// Starts a key update (RFC 9001 §6.1). Outstanding tickets become
    /// stale and must be abandoned.
    pub fn initiate_key_update(&mut self) -> Result<(), CryptoError> {
        b::onertt_check_update(&self.state, self.sealer.state()).map_err(err)?;
        self.rotate_send();
        Ok(())
    }

    fn rotate_send(&mut self) {
        let after = self.send_next_secret.next_phase();
        let secret = mem::replace(&mut self.send_next_secret, after);
        self.sealer.install_packet_key(secret.keys().packet);
        b::onertt_rotated_send(&mut self.state, self.sealer.state_mut());
    }

    /// Drops the previous receive key once its window has passed.
    pub fn discard_expired(&mut self, now_ns: u64) {
        if b::onertt_discard_expired(&mut self.state, now_ns) {
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
        b::onertt_check_limit(&self.state).map_err(err)?;
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
        let h = hdr.to_repr();
        b::onertt_check_form(&h).map_err(err)?;
        let pn = reconstruct(hdr).ok_or(CryptoError::PacketNumberReconstruction)?;
        let choice = b::onertt_choose(&self.state, &h, pn).map_err(err)?;
        let key = match choice {
            b::Choice::Current => &self.recv_current,
            b::Choice::Previous => self
                .recv_previous
                .as_ref()
                .expect("the state holds a previous key"),
            b::Choice::Next => &self.recv_next,
        };
        let o = b::onertt_open(
            &mut self.state,
            self.sealer.state(),
            choice,
            key.lend(),
            key.iv(),
            buf,
            &h,
            pn,
            now_ns,
            &mut scratch(),
        )
        .map_err(err)?;
        if o.promoted.get() {
            self.promote();
        }
        if o.rotate_send.get() {
            // Peer-initiated update: respond with our next keys (§6.2).
            self.rotate_send();
        }
        Ok(OneRttOpened {
            packet_number: o.packet_number,
            header_len: o.header_len as usize,
            payload_len: o.payload_len as usize,
            first_byte: o.first_byte.get(),
            generation: o.generation,
            promoted: o.promoted.get(),
        })
    }

    /// The key swaps of a promotion: next becomes current, current becomes
    /// previous, and the next key is derived from the after-next secret.
    fn promote(&mut self) {
        let after = self.recv_after_next_secret.next_phase();
        let next_secret = mem::replace(&mut self.recv_after_next_secret, after);
        let next_key = mem::replace(&mut self.recv_next, next_secret.keys().packet);
        let old = mem::replace(&mut self.recv_current, next_key);
        self.recv_previous = Some(old);
    }
}
