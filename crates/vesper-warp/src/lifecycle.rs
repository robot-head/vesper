//! Connection lifecycle pieces for [`WarpEndpoint`](crate::endpoint::WarpEndpoint)
//! (wave six, issue #247): addresses, address-validation tokens (Retry and
//! NEW_TOKEN, RFC 9000 §8.1), stateless reset tokens (§10.3), connection
//! ID bookkeeping (§5.1) and path validation state (§8.2, §9).
//!
//! The endpoint is sans-IO and its UMEM frames carry QUIC datagrams only,
//! so the remote address of a datagram is metadata the caller supplies
//! ([`WarpEndpoint::set_rx_source`](crate::endpoint::WarpEndpoint::set_rx_source))
//! and the destination of a sent one is reported to the
//! [`TxTap`](crate::endpoint::TxTap). With no addresses supplied, a
//! connection has no path to validate or migrate and behaves as before.
//!
//! Cryptography: tokens are sealed with AES-256-GCM (`aes-gcm`), reset
//! tokens are HMAC-SHA-256 (`hkdf`'s `hmac` with `sha2`), keys come from
//! rustls's aws-lc-rs CSPRNG; comparisons use `subtle`. Nothing is
//! hand-rolled.

use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::frame::RESET_TOKEN_LEN;

/// A peer's UDP address, supplied by the caller's driver.
pub type PeerAddr = SocketAddr;

/// Fills `out` from rustls's provider CSPRNG (aws-lc-rs).
pub(crate) fn random(out: &mut [u8]) {
    if vesper_warp_tls::config::provider()
        .secure_random
        .fill(out)
        .is_err()
    {
        // aws-lc-rs's RNG does not fail in practice; a zero fill is still
        // safe for nonces under a key that is itself random or caller-set.
        out.fill(0);
    }
}

/// How a server validates client addresses before it commits state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressValidation {
    /// Accept every client Initial; the 3× amplification limit protects
    /// until a Handshake packet arrives (or a NEW_TOKEN token validates).
    #[default]
    Handshake,
    /// Answer a client Initial without a valid token with a Retry (RFC 9000
    /// §8.1.2); only a token-bearing Initial opens a connection.
    Retry,
}

/// Lifecycle parameters of an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// Server: Retry policy.
    pub address_validation: AddressValidation,
    /// How long a Retry token stays valid.
    pub retry_token_lifetime_ns: u64,
    /// Server: issue a NEW_TOKEN token after confirmation (0 disables),
    /// valid this long.
    pub new_token_lifetime_ns: u64,
    /// Answer short-header packets for unknown connection IDs with a
    /// stateless reset and advertise reset tokens (RFC 9000 §10.3).
    pub stateless_reset: bool,
    /// Our `active_connection_id_limit` (2..=8): connection IDs we accept
    /// from the peer. We issue `min(peer limit, this) - 1` extra IDs.
    pub active_connection_id_limit: u8,
    /// Allow the peer to migrate (we advertise `disable_active_migration`
    /// when false) and allow [`WarpEndpoint::migrate`](crate::endpoint::WarpEndpoint::migrate).
    pub migration: bool,
    /// Datagrams held while the keys to open them are not installed yet
    /// (RFC 9001 §5.7), endpoint-wide. 0 drops them.
    pub buffered_datagrams: u8,
    // Wave seven (issue #330).
    /// Server: advertise these addresses in `preferred_address` (RFC 9000
    /// §9.6, §18.2) with a connection ID of sequence 1, and move a client
    /// that validates one there.
    pub preferred_address: Option<PreferredAddressConfig>,
    /// Client: probe the server's `preferred_address` after the handshake
    /// is confirmed and move there once the path is validated (RFC 9000
    /// §9.6.1). On by default.
    pub follow_preferred_address: bool,
}

/// The server addresses a `preferred_address` transport parameter offers.
/// At least one must be set; an unset family is sent as all zeros.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PreferredAddressConfig {
    pub ipv4: Option<SocketAddrV4>,
    pub ipv6: Option<SocketAddrV6>,
}

impl PreferredAddressConfig {
    /// Is `addr` one of these addresses?
    pub fn contains(&self, addr: PeerAddr) -> bool {
        match addr {
            SocketAddr::V4(a) => self.ipv4 == Some(a),
            SocketAddr::V6(a) => self.ipv6 == Some(a),
        }
    }

    pub(crate) fn encode(
        &self,
        connection_id: vesper_warp_tls::ConnectionId,
        stateless_reset_token: [u8; RESET_TOKEN_LEN],
    ) -> vesper_warp_tls::PreferredAddress {
        let (ipv4, ipv4_port) = self
            .ipv4
            .map_or(([0; 4], 0), |a| (a.ip().octets(), a.port()));
        let (ipv6, ipv6_port) = self
            .ipv6
            .map_or(([0; 16], 0), |a| (a.ip().octets(), a.port()));
        vesper_warp_tls::PreferredAddress {
            ipv4,
            ipv4_port,
            ipv6,
            ipv6_port,
            connection_id,
            stateless_reset_token,
        }
    }
}

/// Client: the address of a received `preferred_address` to probe: the
/// family of the current path when the server offers it, else the other
/// one. An all-zero address or port means the family is not offered
/// (RFC 9000 §18.2). `None` when neither is usable.
pub(crate) fn choose_preferred(
    pa: &vesper_warp_tls::PreferredAddress,
    current: Option<PeerAddr>,
) -> Option<PeerAddr> {
    let v4 = (pa.ipv4 != [0; 4] && pa.ipv4_port != 0)
        .then(|| SocketAddr::V4(SocketAddrV4::new(pa.ipv4.into(), pa.ipv4_port)));
    let v6 = (pa.ipv6 != [0; 16] && pa.ipv6_port != 0)
        .then(|| SocketAddr::V6(SocketAddrV6::new(pa.ipv6.into(), pa.ipv6_port, 0, 0)));
    match current {
        Some(SocketAddr::V6(_)) => v6.or(v4),
        _ => v4.or(v6),
    }
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            address_validation: AddressValidation::Handshake,
            retry_token_lifetime_ns: 10_000_000_000,
            new_token_lifetime_ns: 24 * 3_600_000_000_000,
            stateless_reset: true,
            active_connection_id_limit: 4,
            migration: true,
            buffered_datagrams: 8,
            preferred_address: None,
            follow_preferred_address: true,
        }
    }
}

/// Largest `active_connection_id_limit` we store CIDs for.
pub const MAX_ACTIVE_CIDS: usize = 8;

const TOKEN_RETRY: u8 = 1;
const TOKEN_NEW: u8 = 2;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
/// kind, issued_ns, address (family, 16 address bytes, port), odcid.
const TOKEN_PLAIN_MAX: usize = 8 + 1 + 16 + 2 + 1 + 20;
/// Longest token this endpoint issues.
pub const MAX_TOKEN_LEN: usize = 1 + NONCE_LEN + TOKEN_PLAIN_MAX + TAG_LEN;

/// Whether a token claims to come from our Retry (its kind byte is not
/// secret); such a token must validate or the Initial is dropped.
pub(crate) fn is_retry_token(token: &[u8]) -> bool {
    token.first() == Some(&TOKEN_RETRY)
}

/// What a validated token says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// From our Retry: the client's original Destination Connection ID.
    Retry { odcid: [u8; 20], len: u8 },
    /// From our NEW_TOKEN.
    NewToken,
}

/// Why a token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// Not ours, corrupted or forged (AEAD failure).
    Invalid,
    /// Ours, but past its lifetime.
    Expired,
    /// Ours, but issued to another address.
    WrongAddress,
}

/// Seals and opens address-validation tokens (AES-256-GCM, random
/// 96-bit nonces). Every endpoint that should accept another's tokens
/// must share the key ([`TokenKey::from_bytes`]).
pub struct TokenKey {
    aead: Aes256Gcm,
}

impl std::fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenKey(..)")
    }
}

impl TokenKey {
    pub fn random() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        random(&mut k[..]);
        Self::from_bytes(&k)
    }

    pub fn from_bytes(key: &[u8; 32]) -> Self {
        Self {
            aead: Aes256Gcm::new(key.into()),
        }
    }

    fn encode_addr(addr: Option<PeerAddr>, out: &mut [u8; 19]) {
        out.fill(0);
        if let Some(a) = addr {
            match a.ip() {
                IpAddr::V4(ip) => {
                    out[0] = 4;
                    out[1..5].copy_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    out[0] = 6;
                    out[1..17].copy_from_slice(&ip.octets());
                }
            }
            out[17..19].copy_from_slice(&a.port().to_be_bytes());
        }
    }

    /// Writes a token into `out`; returns its length.
    pub(crate) fn seal(
        &self,
        kind: TokenKind,
        addr: Option<PeerAddr>,
        now_ns: u64,
        out: &mut [u8; MAX_TOKEN_LEN],
    ) -> usize {
        let (k, odcid): (u8, &[u8]) = match &kind {
            TokenKind::Retry { odcid, len } => (TOKEN_RETRY, &odcid[..usize::from(*len)]),
            TokenKind::NewToken => (TOKEN_NEW, &[]),
        };
        out[0] = k;
        random(&mut out[1..1 + NONCE_LEN]);
        let mut plain = [0u8; TOKEN_PLAIN_MAX];
        plain[..8].copy_from_slice(&now_ns.to_be_bytes());
        let mut a = [0u8; 19];
        Self::encode_addr(addr, &mut a);
        plain[8..27].copy_from_slice(&a);
        plain[27] = odcid.len() as u8;
        plain[28..28 + odcid.len()].copy_from_slice(odcid);
        let plain_len = 28 + odcid.len();
        let start = 1 + NONCE_LEN;
        out[start..start + plain_len].copy_from_slice(&plain[..plain_len]);
        let (head, body) = out.split_at_mut(start);
        let nonce = Nonce::from_slice(&head[1..]);
        // Only fails for multi-GiB payloads.
        let tag = self
            .aead
            .encrypt_in_place_detached(nonce, &head[..1], &mut body[..plain_len])
            .expect("short token");
        body[plain_len..plain_len + TAG_LEN].copy_from_slice(&tag);
        start + plain_len + TAG_LEN
    }

    /// Opens and checks a token received from `addr` at `now_ns`.
    pub(crate) fn open(
        &self,
        token: &[u8],
        addr: Option<PeerAddr>,
        now_ns: u64,
        lifetimes: (u64, u64),
    ) -> Result<TokenKind, TokenError> {
        let start = 1 + NONCE_LEN;
        if token.len() < start + 28 + TAG_LEN || token.len() > MAX_TOKEN_LEN {
            return Err(TokenError::Invalid);
        }
        let kind = token[0];
        if kind != TOKEN_RETRY && kind != TOKEN_NEW {
            return Err(TokenError::Invalid);
        }
        let plain_len = token.len() - start - TAG_LEN;
        let mut plain = [0u8; TOKEN_PLAIN_MAX];
        plain[..plain_len].copy_from_slice(&token[start..start + plain_len]);
        let tag = aes_gcm::Tag::from_slice(&token[start + plain_len..]);
        self.aead
            .decrypt_in_place_detached(
                Nonce::from_slice(&token[1..start]),
                &token[..1],
                &mut plain[..plain_len],
                tag,
            )
            .map_err(|_| TokenError::Invalid)?;
        let issued = u64::from_be_bytes(plain[..8].try_into().expect("8 bytes"));
        let lifetime = if kind == TOKEN_RETRY {
            lifetimes.0
        } else {
            lifetimes.1
        };
        if now_ns < issued || now_ns - issued > lifetime {
            return Err(TokenError::Expired);
        }
        let mut a = [0u8; 19];
        Self::encode_addr(addr, &mut a);
        if !bool::from(plain[8..27].ct_eq(&a)) {
            return Err(TokenError::WrongAddress);
        }
        let len = plain[27];
        if usize::from(len) + 28 != plain_len || len > 20 {
            return Err(TokenError::Invalid);
        }
        if kind == TOKEN_NEW {
            return if len == 0 {
                Ok(TokenKind::NewToken)
            } else {
                Err(TokenError::Invalid)
            };
        }
        let mut odcid = [0u8; 20];
        odcid[..usize::from(len)].copy_from_slice(&plain[28..28 + usize::from(len)]);
        Ok(TokenKind::Retry { odcid, len })
    }
}

/// Derives stateless reset tokens: HMAC-SHA-256(key, connection ID),
/// truncated to 16 bytes (RFC 9000 §10.3.2). Endpoints behind one
/// address must share the key so any of them can reset for the others.
pub struct ResetKey {
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for ResetKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResetKey(..)")
    }
}

impl ResetKey {
    pub fn random() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        random(&mut k[..]);
        Self { key: k }
    }

    pub fn from_bytes(key: &[u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(*key),
        }
    }

    /// The reset token for one of our connection IDs.
    pub fn token(&self, cid: &[u8]) -> [u8; RESET_TOKEN_LEN] {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&self.key[..]).expect("HMAC takes any key");
        mac.update(cid);
        let full = mac.finalize().into_bytes();
        let mut t = [0u8; RESET_TOKEN_LEN];
        t.copy_from_slice(&full[..RESET_TOKEN_LEN]);
        t
    }
}

/// Constant-time token comparison.
pub(crate) fn token_eq(a: &[u8; RESET_TOKEN_LEN], b: &[u8]) -> bool {
    b.len() == RESET_TOKEN_LEN && bool::from(a[..].ct_eq(b))
}

/// Smallest stateless reset we send, and smallest datagram we treat as a
/// possible reset (RFC 9000 §10.3: 5 unpredictable bytes and the token).
pub const MIN_STATELESS_RESET: usize = 21;

/// A connection ID we issued (RFC 9000 §5.1.1).
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalCid {
    pub seq: u64,
    pub cid: vesper_warp_shard::ConnectionId,
}

/// A connection ID the peer issued, with its reset token.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PeerCid {
    pub seq: u64,
    pub cid: vesper_warp_shard::ConnectionId,
    pub token: Option<[u8; RESET_TOKEN_LEN]>,
}

/// Connection-ID state of one connection.
#[derive(Debug, Clone)]
pub(crate) struct Cids {
    /// Extra IDs we issued, routed through the endpoint's `alt_cids`; the
    /// one the connection table is keyed by (sequence `table_seq`) is not
    /// listed here.
    pub local: [Option<LocalCid>; MAX_ACTIVE_CIDS],
    /// Sequence number of the ID the connection table is keyed by: 0 (the
    /// handshake ID) until the peer retires it, then the ID that replaced
    /// it in the table (RFC 9000 §5.1.2).
    pub table_seq: u64,
    /// The peer retired the table's ID and no replacement was available
    /// yet; it still routes until one is.
    pub table_retired: bool,
    pub next_local_seq: u64,
    /// IDs the peer issued, including the one in use.
    pub peer: [Option<PeerCid>; MAX_ACTIVE_CIDS],
    /// Sequence number of the peer ID we send to.
    pub active_peer_seq: u64,
    /// The peer's largest Retire Prior To.
    pub peer_retire_prior_to: u64,
    /// How many IDs we may issue (`min(peer limit, ours)`); 1 until the
    /// peer's transport parameters arrive.
    pub issue_limit: u64,
}

impl Cids {
    pub fn new() -> Self {
        Self {
            local: [None; MAX_ACTIVE_CIDS],
            table_seq: 0,
            table_retired: false,
            next_local_seq: 1,
            peer: [None; MAX_ACTIVE_CIDS],
            active_peer_seq: 0,
            peer_retire_prior_to: 0,
            issue_limit: 1,
        }
    }

    pub fn active_local(&self) -> usize {
        usize::from(!self.table_retired) + self.local.iter().flatten().count()
    }

    /// Records a peer ID; `Err` for a RFC 9000 §5.1.1 violation
    /// (CONNECTION_ID_LIMIT_ERROR or PROTOCOL_VIOLATION).
    pub fn add_peer(&mut self, c: PeerCid, limit: usize) -> Result<(), u64> {
        for e in self.peer.iter().flatten() {
            if e.seq == c.seq {
                // §19.15: the same sequence number must carry the same ID.
                return if e.cid == c.cid { Ok(()) } else { Err(0x0a) };
            }
        }
        if c.seq < self.peer_retire_prior_to {
            // Already retired by the peer: we answer with RETIRE (caller).
            return Ok(());
        }
        let live = self.peer.iter().flatten().count();
        if live >= limit {
            return Err(0x09);
        }
        if let Some(slot) = self.peer.iter_mut().find(|s| s.is_none()) {
            *slot = Some(c);
            Ok(())
        } else {
            Err(0x09)
        }
    }

    /// The peer ID with sequence `seq`.
    pub fn peer_cid(&self, seq: u64) -> Option<&PeerCid> {
        self.peer.iter().flatten().find(|p| p.seq == seq)
    }

    /// An unused peer ID other than the active one (lowest sequence).
    pub fn spare_peer(&self) -> Option<PeerCid> {
        self.peer
            .iter()
            .flatten()
            .filter(|p| p.seq != self.active_peer_seq)
            .min_by_key(|p| p.seq)
            .copied()
    }

    /// Forgets the peer ID `seq`.
    pub fn remove_peer(&mut self, seq: u64) {
        for s in &mut self.peer {
            if s.is_some_and(|p| p.seq == seq) {
                *s = None;
            }
        }
    }

    /// Does any active peer ID's reset token match `tail`?
    pub fn reset_matches(&self, tail: &[u8]) -> bool {
        // Every entry is compared so timing does not reveal which matched.
        let mut hit = false;
        for p in self.peer.iter().flatten() {
            if let Some(t) = &p.token {
                hit |= token_eq(t, tail);
            }
        }
        hit
    }
}

/// Path validation in progress (RFC 9000 §8.2).
#[derive(Debug, Clone, Copy)]
pub(crate) struct PathProbe {
    pub addr: Option<PeerAddr>,
    pub challenge: [u8; 8],
    /// When to give up.
    pub deadline_ns: u64,
    /// The validated path to return to if validation fails (server-side
    /// migration), else `None`.
    pub fallback: Option<Option<PeerAddr>>,
    /// When the last PATH_CHALLENGE was sent.
    pub sent_ns: u64,
    // Wave seven (issue #330).
    /// Our local address the challenge leaves from (server: its preferred
    /// address), `None` for the path's own.
    pub local: Option<PeerAddr>,
    /// The peer connection ID the challenge goes to, when not the active
    /// one (client: sequence 1, the preferred address's ID).
    pub dcid_seq: Option<u64>,
    /// Client: a probe of the server's preferred address; on success the
    /// connection moves there (RFC 9000 §9.6.1).
    pub preferred: bool,
    /// A challenge of this probe left below 1200 bytes (amplification
    /// limit): its response validates the address, not the MTU.
    pub unpadded_sent: bool,
    /// This is the expanded re-validation that follows (RFC 9000 §8.2.3).
    pub mtu_check: bool,
}

/// A connection's network path.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Path {
    /// Where we send; `None` when the caller supplies no addresses.
    pub remote: Option<PeerAddr>,
    /// Server: the local address we send from, once a client moved to our
    /// preferred address; `None` for the original one.
    pub local: Option<PeerAddr>,
    /// The path's address is validated (RFC 9000 §8).
    pub validated: bool,
    /// Bytes received on and sent to an unvalidated migrated path, for its
    /// 3× limit (RFC 9000 §9.3).
    pub received: u64,
    pub sent: u64,
}

impl Path {
    pub fn allows(&self, bytes: u64) -> bool {
        self.validated
            || self.sent.saturating_add(bytes)
                <= self
                    .received
                    .saturating_mul(crate::handshake::AMPLIFICATION_FACTOR)
    }
}

/// Does the address change only in its port (a NAT rebinding)? RFC 9000
/// §9.4 then lets the congestion controller and RTT estimate be kept.
pub(crate) fn port_only_change(a: Option<PeerAddr>, b: Option<PeerAddr>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a.ip() == b.ip())
}
