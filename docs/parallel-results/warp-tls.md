# Wave-five lane 12: Warp QUIC TLS 1.3 handshake

Issue #132. Code: `crates/vesper-warp-tls`. Base: `main` at
`af03122ea74d403f203170926945f9d35786b289`. Covers the handshake gap that
the wave-four integration (PR #101, `warp-integration.md`) recorded for
design §35, following RFC 9001 §4 and RFC 9000 §7 and §18.

## What is implemented

| Piece | Module | Notes |
| --- | --- | --- |
| Sans-I/O handshake driver, client and server | `handshake.rs` (`QuicTls`) | Over `rustls::quic` 0.23.46. Input: CRYPTO bytes per space. Output: `Event`s in order. |
| Secrets in `vesper-warp-crypto` form | `handshake.rs` (`SpaceKeys`) | `into_sealer_opener()` for Initial/Handshake, `into_one_rtt()` for 1-RTT (`OneRttKeys`, RFC 9001 §6 key updates). |
| Secret export from rustls | `config.rs` | See "Secret export" below. |
| CRYPTO reassembly per space | `reassembly.rs` | Fixed ring (64 KiB default), bounded range list (256), out-of-order, duplicates, typed overflow. |
| Transport parameters | `params.rs` | All 17 RFC 9000 §18.2 parameters: encode, decode, ranges, duplicates, server-only, required, §7.3 CID authentication. |
| Key lifecycle and confirmation | `handshake.rs` (`KeyState`) | RFC 9001 §4.9 discard rules, §4.1.2 confirmation, HANDSHAKE_DONE, key updates refused before confirmation. |
| Configuration | `config.rs` | `ClientSetup`/`ServerSetup`: TLS 1.3 only, ALPN required, 0-RTT off, aws-lc-rs provider limited to the three suites `vesper-warp-crypto` implements, optional caller-supplied clock (`FixedTime`). |
| Errors | `error.rs` | Every failure is a `TlsError` with a QUIC error code (`CRYPTO_ERROR + alert`, `TRANSPORT_PARAMETER_ERROR`, `CRYPTO_BUFFER_EXCEEDED`, `PROTOCOL_VIOLATION`, `FRAME_ENCODING_ERROR`, `KEY_UPDATE_ERROR`). |

### Event flow

`QuicTls::client` emits Initial keys (from the original DCID, via
`InitialSecrets::new_v1`) and the ClientHello. `QuicTls::server` emits
Initial keys (from the Retry SCID if the server sent a Retry, else the
original DCID). Then, as CRYPTO data is fed in:

1. `Crypto { space, offset, data }` for each outgoing flight, with offsets
   tracked per space;
2. `KeysReady(SpaceKeys)` for Handshake, then 1-RTT;
3. `PeerTransportParameters` once the peer's list is decoded and
   authenticated;
4. `HandshakeComplete` (with an agreed ALPN protocol);
5. server: `SendHandshakeDone`, then `HandshakeConfirmed`;
   client: `HandshakeConfirmed` after `on_handshake_done_received()` or
   `on_one_rtt_ack_received()`;
6. `DiscardKeys(Initial)` when the client sends its first Handshake packet
   or the server first processes one (the caller reports these), and
   `DiscardKeys(Handshake)` at confirmation.

### Rules enforced on CRYPTO input

* Data for a space with `Pending` keys is `SpaceUnavailable`; for a
  `Discarded` space, `SpaceDiscarded` (both `PROTOCOL_VIOLATION`).
* Data for a space TLS has moved past may only repeat bytes already
  received; anything past them is `PROTOCOL_VIOLATION` (RFC 9001 §4.1.3).
* Data for a later space whose keys exist (the server's 1-RTT space before
  the client Finished) is buffered and delivered when TLS reaches it.
* A client must record the server's Initial SCID before feeding server
  Initial data (`set_peer_initial_scid`), so the §7.3 check has its input;
  a later different SCID is refused (discard that packet).
* A client accepts at most one Retry, before any server Initial, and not
  one whose SCID equals the original DCID.
* Fatal errors close the driver; later calls return `Closed { code }`.

## Secret export

rustls keeps QUIC traffic secrets private: `KeyChange` gives `PacketKey`
and `HeaderProtectionKey` trait objects and `quic::Secrets` has private
fields. `vesper-warp-crypto` consumes secrets. The crate therefore installs
rustls's public `KeyLog` hook on every configuration it builds, accepting
only the four TLS 1.3 handshake and application traffic secret labels.
rustls calls the hook synchronously while deriving each secret; the hook
writes into a thread-local slot that the driver arms only for the duration
of its own `read_hs`/`write_hs` call, so a secret reaches only the driver
whose call derived it. Captured bytes are held in `Zeroizing` buffers and
moved into `TrafficSecret`s (zeroized on drop).

Because this path is unusual, every export is checked at the moment rustls
hands over its keys: the driver seals a probe packet with rustls's packet
and header keys and opens it with `vesper-warp-crypto` keys derived from the
captured secret, for both directions. For 1-RTT it also checks generation 1
(rustls's `next` secrets, i.e. `quic ku`) against
`TrafficSecret::next_phase`, which is what `OneRttKeys` uses for key
updates. A mismatch is `KeyExportMismatch` (internal error), never a
silent fallback.

## Tests

`cargo test -p vesper-warp-tls --locked`: 22 tests (3 unit, 14 handshake,
5 transport-parameter), all passing.

* Full handshake in memory: both sides complete and confirm, agree on ALPN
  and all three spaces' secrets (mirrored), receive each other's transport
  parameters intact, and discard Initial then Handshake keys in RFC 9001
  §4.9 order. Initial and Handshake keys seal/open long-header packets;
  1-RTT keys in `OneRttKeys` seal/open short-header packets both ways; a key
  update is refused until a current-phase packet is acknowledged, then the
  peer opens the next phase.
* Each supported suite (AES-128-GCM, AES-256-GCM, ChaCha20-Poly1305)
  negotiated separately, with matching 1-RTT keys.
* CRYPTO data split into 7-byte pieces, reversed within each space and
  partly duplicated: the handshake still completes.
* Retry: Initial keys re-derived from the Retry SCID match the server's;
  `retry_source_connection_id` is authenticated; a server that omits it is
  rejected with `TRANSPORT_PARAMETER_ERROR`.
* Certificates (test CA in `tests/fixtures`, regenerated by `gen.sh`):
  untrusted issuer (alert 48), wrong server name (42), expired at a fixed
  clock (45) are rejected with `CRYPTO_ERROR + alert`.
* ALPN mismatch: server fails with `no_application_protocol` (0x178).
* Transport parameters: round trip of every parameter; unknown and
  reserved ids ignored; truncation, duplicates, non-minimal-length values,
  each out-of-range value, missing required parameters, server-only
  parameters from a client and a zero-length preferred-address CID
  rejected; CID substitution (original DCID, peer SCID either side, Retry
  SCID) rejected. Corrupted lists sent by a real rustls QUIC server reach
  our client and close it with `TRANSPORT_PARAMETER_ERROR`.
* Misuse: CRYPTO data before keys, after discard, before the SCID is
  recorded, garbage instead of a ClientHello, data beyond the buffer, an
  offset past 2^62 − 1, HANDSHAKE_DONE to a server or before completion,
  key update before confirmation.

## Checks run

```
cargo fmt --all -- --check
cargo clippy -p vesper-warp-tls --locked --all-targets -- -D warnings
cargo test -p vesper-warp-tls --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
```

Results are in the PR description.

## Root patch

One commit: `crates/vesper-warp-tls` added to `members` and
`default-members` (before `experiments/native-pulse`), a new
`[workspace.dependencies]` entry
`rustls = { version = "=0.23.46", default-features = false, features = ["std", "aws_lc_rs"] }`
(the version already in `Cargo.lock`, pulled in by `tough`), and the
lockfile entry for the new crate. No new external packages enter the
lockfile.

## Decisions

* **Provider**: aws-lc-rs, which rustls in this workspace already builds
  with on Linux, macOS AArch64 and Windows MSVC CI. Cipher suites are
  limited to the three `vesper-warp-crypto` implements; any other is
  `UnsupportedCipherSuite`.
* **Clock**: certificate validity needs wall-clock time, which the
  wave-five contracts do not let library code read. `ClientSetup::with_time`
  takes a caller-supplied `TimeProvider` (`FixedTime` for tests);
  `ClientSetup::new` uses rustls's default clock and says so.
* **Varints** are implemented locally (`varint.rs`): the contracts let this
  crate depend only on `vesper-warp-crypto`.
* **Duplicate detection** covers the 17 parameters RFC 9000 defines;
  repeated unknown parameters are ignored with the rest of the unknown
  ones (RFC 9000 §7.4.2 requires ignoring them).
* **Server confirmation** discards Initial keys too if the caller never
  reported a processed Handshake packet, so Initial keys never outlive
  confirmation.

## Not done / gaps

* No endpoint wiring: `WarpEndpoint` does not send or accept long-header
  packets yet. Integration A (#135) wires this crate in with lane 13's
  CRYPTO buffers.
* 0-RTT is disabled (client `enable_early_data = false`, server
  `max_early_data_size = 0`) and untested; session resumption without
  0-RTT uses rustls defaults and is not exercised by a test.
* No client certificates, no QUIC v2, no version negotiation.
* The secret export depends on rustls continuing to call `KeyLog` for QUIC
  connections; the probe check turns any change into a typed error rather
  than wrong keys.
* No allocation claim: the driver allocates for CRYPTO output and rustls
  allocates internally. Nothing was measured.
* Rust ownership of `TrafficSecret` and `NonceTicket` is host-library
  discipline; it is not evidence that the Vesper source typing rules for
  linear values are implemented.
