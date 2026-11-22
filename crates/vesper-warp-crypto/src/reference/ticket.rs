//! Linear nonce tickets, the per-key sealer that issues them, and the
//! matching opener.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::protect::{KEY_PHASE_BIT, LONG_HEADER_BIT, PacketKey, Unprotected, truncated_matches};
use crate::schedule::DirectionalKeys;
use crate::{Cipher, CryptoError, PACKET_NUMBER_LIMIT, SAMPLE_LEN, TAG_LEN, check_pn};

static NEXT_SEALER_ID: AtomicU64 = AtomicU64::new(1);
static LEAKED_TICKETS: AtomicU64 = AtomicU64::new(0);

/// Process-wide count of [`NonceTicket`]s dropped without being sealed or
/// abandoned. Their packet numbers stay burned; the count is for auditors.
pub fn leaked_nonce_tickets() -> u64 {
    LEAKED_TICKETS.load(Ordering::Relaxed)
}

/// The right to seal exactly one packet with one packet number under one
/// key. Issued only by [`Sealer::ticket`]; consumed only by
/// [`Sealer::seal`] or [`Sealer::abandon`]. Not `Clone`/`Copy`, so a nonce
/// can be used at most once.
#[must_use = "a NonceTicket must be sealed or abandoned; dropping it is counted as a leak"]
#[derive(Debug)]
pub struct NonceTicket {
    sealer: u64,
    generation: u64,
    pn: u64,
    live: bool,
}

impl NonceTicket {
    /// The full packet number this ticket authorises.
    pub fn packet_number(&self) -> u64 {
        self.pn
    }

    /// The key generation (number of key updates) it was issued under.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn consume(mut self) {
        self.live = false;
    }
}

impl Drop for NonceTicket {
    fn drop(&mut self) {
        if self.live {
            LEAKED_TICKETS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A failed seal: the ticket is handed back unconsumed.
#[must_use = "the returned ticket must be sealed again or abandoned"]
#[derive(Debug)]
pub struct SealError {
    pub ticket: NonceTicket,
    pub error: CryptoError,
}

/// Ticket accounting for one sealer. `outstanding` includes leaked tickets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketAudit {
    pub issued: u64,
    pub sealed: u64,
    pub abandoned: u64,
    pub outstanding: u64,
}

/// Per-key, per-direction packet sealer. Issues tickets for strictly
/// increasing packet numbers and enforces the RFC 9001 §6.6
/// confidentiality limit per key.
pub struct Sealer {
    id: u64,
    generation: u64,
    keys: DirectionalKeys,
    next_pn: u64,
    issued_this_key: u64,
    confidentiality_limit: u64,
    issued: u64,
    sealed: u64,
    abandoned: u64,
    /// `Some` for 1-RTT keys: short headers only, with this key-phase bit.
    key_phase: Option<bool>,
}

impl Sealer {
    /// A sealer for Initial/Handshake/0-RTT keys (no key phase).
    pub fn new(keys: DirectionalKeys) -> Self {
        Self::with_phase(keys, None)
    }

    pub(crate) fn with_phase(keys: DirectionalKeys, key_phase: Option<bool>) -> Self {
        let confidentiality_limit = keys.packet.cipher().confidentiality_limit();
        Self {
            id: NEXT_SEALER_ID.fetch_add(1, Ordering::Relaxed),
            generation: 0,
            keys,
            next_pn: 0,
            issued_this_key: 0,
            confidentiality_limit,
            issued: 0,
            sealed: 0,
            abandoned: 0,
            key_phase,
        }
    }

    /// Lowers (never raises) the per-key confidentiality limit, e.g. for a
    /// stricter local policy or tests.
    pub fn with_confidentiality_limit(mut self, limit: u64) -> Self {
        self.confidentiality_limit = self.confidentiality_limit.min(limit);
        self
    }

    pub fn cipher(&self) -> Cipher {
        self.keys.packet.cipher()
    }

    /// The smallest packet number a new ticket may carry.
    pub fn next_packet_number(&self) -> u64 {
        self.next_pn
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Tickets still available under the current key.
    pub fn remaining_under_key(&self) -> u64 {
        self.confidentiality_limit
            .saturating_sub(self.issued_this_key)
            .min(PACKET_NUMBER_LIMIT - self.next_pn)
    }

    pub fn audit(&self) -> TicketAudit {
        TicketAudit {
            issued: self.issued,
            sealed: self.sealed,
            abandoned: self.abandoned,
            outstanding: self.issued - self.sealed - self.abandoned,
        }
    }

    /// Issues a ticket for `pn`, which must exceed every packet number
    /// already issued by this sealer and be `< 2^62`.
    pub fn ticket(&mut self, pn: u64) -> Result<NonceTicket, CryptoError> {
        check_pn(pn)?;
        if pn < self.next_pn {
            return Err(if pn + 1 == self.next_pn {
                CryptoError::PacketNumberReused(pn)
            } else {
                CryptoError::PacketNumberRegression {
                    requested: pn,
                    next_allowed: self.next_pn,
                }
            });
        }
        if self.issued_this_key >= self.confidentiality_limit {
            return Err(CryptoError::ConfidentialityLimitReached);
        }
        self.next_pn = pn + 1; // pn < 2^62, cannot overflow
        self.issued_this_key += 1;
        self.issued += 1;
        Ok(NonceTicket {
            sealer: self.id,
            generation: self.generation,
            pn,
            live: true,
        })
    }

    /// Issues a ticket for the next packet number.
    pub fn next_ticket(&mut self) -> Result<NonceTicket, CryptoError> {
        if self.next_pn >= PACKET_NUMBER_LIMIT {
            return Err(CryptoError::PacketNumbersExhausted);
        }
        self.ticket(self.next_pn)
    }

    /// Seals one packet in place and consumes the ticket.
    ///
    /// `buf` holds the unprotected header (first byte at 0, truncated packet
    /// number at `pn_offset`, its length in the first byte's low two bits),
    /// then `payload_len` plaintext bytes, then at least [`TAG_LEN`] spare
    /// bytes. Returns the protected packet length. On any error the buffer
    /// is untouched and the ticket is returned.
    pub fn seal(
        &mut self,
        ticket: NonceTicket,
        buf: &mut [u8],
        pn_offset: usize,
        payload_len: usize,
    ) -> Result<usize, SealError> {
        match self.check_seal(&ticket, buf, pn_offset, payload_len) {
            Ok(header_len) => {
                let len = self
                    .keys
                    .packet
                    .seal(ticket.pn, buf, header_len, payload_len)
                    .and_then(|len| {
                        self.keys
                            .header
                            .apply(&mut buf[..len], pn_offset)
                            .map(|_| len)
                    });
                match len {
                    Ok(len) => {
                        ticket.consume();
                        self.sealed += 1;
                        Ok(len)
                    }
                    // Unreachable after check_seal; kept typed regardless.
                    Err(error) => Err(SealError { ticket, error }),
                }
            }
            Err(error) => Err(SealError { ticket, error }),
        }
    }

    fn check_seal(
        &self,
        ticket: &NonceTicket,
        buf: &[u8],
        pn_offset: usize,
        payload_len: usize,
    ) -> Result<usize, CryptoError> {
        if ticket.sealer != self.id || ticket.generation != self.generation {
            return Err(CryptoError::StaleTicket);
        }
        if pn_offset == 0 {
            return Err(CryptoError::InvalidPacketNumberOffset);
        }
        let first = *buf.first().ok_or(CryptoError::BufferTooShort {
            needed: 1,
            available: 0,
        })?;
        if let Some(phase) = self.key_phase {
            if first & LONG_HEADER_BIT != 0 {
                return Err(CryptoError::InvalidHeaderForm);
            }
            if (first & KEY_PHASE_BIT != 0) != phase {
                return Err(CryptoError::KeyPhaseMismatch);
            }
        }
        let pn_len = usize::from(first & 0x03) + 1;
        let too_short = |needed| CryptoError::BufferTooShort {
            needed,
            available: buf.len(),
        };
        let header_len = pn_offset.checked_add(pn_len).ok_or(too_short(usize::MAX))?;
        let total = header_len
            .checked_add(payload_len)
            .and_then(|n| n.checked_add(TAG_LEN))
            .ok_or(too_short(usize::MAX))?;
        if total > buf.len() {
            return Err(too_short(total));
        }
        let sample_end = pn_offset + 4 + SAMPLE_LEN; // pn_offset < total
        if sample_end > total {
            return Err(CryptoError::SampleOutOfBounds {
                sample_end,
                packet_len: total,
            });
        }
        if !truncated_matches(buf, pn_offset, pn_len, ticket.pn) {
            return Err(CryptoError::PacketNumberMismatch);
        }
        Ok(header_len)
    }

    /// Gives up a ticket without sealing; its packet number stays burned.
    /// A ticket from another sealer is handed back.
    pub fn abandon(&mut self, ticket: NonceTicket) -> Result<(), NonceTicket> {
        if ticket.sealer != self.id {
            return Err(ticket);
        }
        ticket.consume();
        self.abandoned += 1;
        Ok(())
    }

    /// Installs the next-phase packet key (header key is unchanged, RFC
    /// 9001 §6). Outstanding tickets become stale.
    pub(crate) fn rekey(&mut self, packet: PacketKey) {
        self.keys.packet = packet;
        self.generation += 1;
        self.issued_this_key = 0;
        self.key_phase = self.key_phase.map(|p| !p);
    }

    pub(crate) fn key_phase(&self) -> Option<bool> {
        self.key_phase
    }
}

/// A successfully opened packet. The plaintext is at
/// `buf[header_len..header_len + payload_len]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    pub packet_number: u64,
    pub header_len: usize,
    pub payload_len: usize,
    /// Unprotected first byte.
    pub first_byte: u8,
}

/// Per-key, per-direction opener for Initial/Handshake/0-RTT packets.
pub struct Opener {
    keys: DirectionalKeys,
    failures: u64,
    integrity_limit: u64,
    largest_opened: Option<u64>,
}

impl Opener {
    pub fn new(keys: DirectionalKeys) -> Self {
        let integrity_limit = keys.packet.cipher().integrity_limit();
        Self {
            keys,
            failures: 0,
            integrity_limit,
            largest_opened: None,
        }
    }

    /// Lowers (never raises) the integrity limit.
    pub fn with_integrity_limit(mut self, limit: u64) -> Self {
        self.integrity_limit = self.integrity_limit.min(limit);
        self
    }

    /// Packets that failed authentication (RFC 9001 §6.6 counter).
    pub fn authentication_failures(&self) -> u64 {
        self.failures
    }

    pub fn largest_opened(&self) -> Option<u64> {
        self.largest_opened
    }

    /// Step 2 of the receive boundary: removes header protection in place.
    /// No opener state changes.
    pub fn remove_header_protection(
        &self,
        buf: &mut [u8],
        pn_offset: usize,
    ) -> Result<Unprotected, CryptoError> {
        self.keys.header.remove(buf, pn_offset)
    }

    /// Step 4 of the receive boundary: opens `buf[hdr.header_len()..]` with
    /// the **full** reconstructed packet number. `buf` must be exactly one
    /// packet (coalesced packets split by the packet core). On error the
    /// payload bytes are unchanged; the only state change is the §6.6
    /// failure counter on authentication failure.
    pub fn open(
        &mut self,
        buf: &mut [u8],
        hdr: &Unprotected,
        full_pn: u64,
    ) -> Result<Opened, CryptoError> {
        if self.failures >= self.integrity_limit {
            return Err(CryptoError::IntegrityLimitReached);
        }
        check_pn(full_pn)?;
        if !hdr.encodes(full_pn) {
            return Err(CryptoError::PacketNumberMismatch);
        }
        match self.keys.packet.open(full_pn, buf, hdr.header_len()) {
            Ok(payload_len) => {
                self.largest_opened = Some(self.largest_opened.map_or(full_pn, |l| l.max(full_pn)));
                Ok(Opened {
                    packet_number: full_pn,
                    header_len: hdr.header_len(),
                    payload_len,
                    first_byte: hdr.first_byte,
                })
            }
            Err(e) => {
                if e == CryptoError::AuthenticationFailed {
                    self.failures += 1;
                }
                Err(e)
            }
        }
    }

    /// Steps 2–4 in one call: `reconstruct` maps the unprotected header to
    /// the full packet number (the packet core's job). On any error header
    /// protection is re-applied, so `buf` is byte-identical to its input.
    pub fn open_packet(
        &mut self,
        buf: &mut [u8],
        pn_offset: usize,
        reconstruct: impl FnOnce(&Unprotected) -> Option<u64>,
    ) -> Result<Opened, CryptoError> {
        if self.failures >= self.integrity_limit {
            return Err(CryptoError::IntegrityLimitReached);
        }
        let hdr = self.remove_header_protection(buf, pn_offset)?;
        let result = match reconstruct(&hdr) {
            Some(pn) => self.open(buf, &hdr, pn),
            None => Err(CryptoError::PacketNumberReconstruction),
        };
        if result.is_err() {
            hdr.reapply(buf);
        }
        result
    }
}
