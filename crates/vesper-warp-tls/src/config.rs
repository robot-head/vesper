//! rustls client and server configurations prepared for QUIC, and the
//! secret export hook.
//!
//! rustls keeps QUIC traffic secrets private (`quic::Secrets` exposes only
//! derived `PacketKey` objects). `vesper-warp-crypto` consumes secrets, so
//! this crate exports them through rustls's public [`KeyLog`] hook, which
//! rustls calls synchronously while it derives each TLS 1.3 secret. The
//! hook writes into a thread-local slot that the handshake driver arms
//! only for the duration of its own `read_hs`/`write_hs` calls, so a secret
//! can only reach the connection whose call derived it. Every exported
//! secret is then checked against rustls's own packet keys before use
//! (see `handshake.rs`).

use std::cell::RefCell;
use std::fmt;
use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::{ClientConfig, KeyLog, RootCertStore, ServerConfig};
use zeroize::Zeroizing;

use crate::error::TlsError;

const CLIENT_HS: &str = "CLIENT_HANDSHAKE_TRAFFIC_SECRET";
const SERVER_HS: &str = "SERVER_HANDSHAKE_TRAFFIC_SECRET";
const CLIENT_APP: &str = "CLIENT_TRAFFIC_SECRET_0";
const SERVER_APP: &str = "SERVER_TRAFFIC_SECRET_0";

/// Secrets captured during one driver call. Zeroized on drop.
#[derive(Default)]
pub(crate) struct Captured {
    pub client_hs: Option<Zeroizing<Vec<u8>>>,
    pub server_hs: Option<Zeroizing<Vec<u8>>>,
    pub client_app: Option<Zeroizing<Vec<u8>>>,
    pub server_app: Option<Zeroizing<Vec<u8>>>,
}

thread_local! {
    static SLOT: RefCell<Option<Captured>> = const { RefCell::new(None) };
}

/// Runs `f` with the capture slot armed and moves anything captured into
/// `into`. The slot is disarmed even if `f` panics.
pub(crate) fn with_capture<R>(into: &mut Captured, f: impl FnOnce() -> R) -> R {
    struct Disarm<'a>(&'a mut Captured);
    impl Drop for Disarm<'_> {
        fn drop(&mut self) {
            if let Some(c) = SLOT.with(|s| s.borrow_mut().take()) {
                merge(self.0, c);
            }
        }
    }
    SLOT.with(|s| *s.borrow_mut() = Some(Captured::default()));
    let _guard = Disarm(into);
    f()
}

fn merge(into: &mut Captured, from: Captured) {
    let Captured {
        client_hs,
        server_hs,
        client_app,
        server_app,
    } = from;
    if client_hs.is_some() {
        into.client_hs = client_hs;
    }
    if server_hs.is_some() {
        into.server_hs = server_hs;
    }
    if client_app.is_some() {
        into.client_app = client_app;
    }
    if server_app.is_some() {
        into.server_app = server_app;
    }
}

/// The [`KeyLog`] installed on every configuration this crate builds. It
/// records nothing unless a handshake driver call is in progress on the
/// current thread.
#[derive(Debug)]
struct ExportSecrets;

impl KeyLog for ExportSecrets {
    fn log(&self, label: &str, _client_random: &[u8], secret: &[u8]) {
        SLOT.with(|s| {
            if let Some(c) = s.borrow_mut().as_mut() {
                let v = Some(Zeroizing::new(secret.to_vec()));
                match label {
                    CLIENT_HS => c.client_hs = v,
                    SERVER_HS => c.server_hs = v,
                    CLIENT_APP => c.client_app = v,
                    SERVER_APP => c.server_app = v,
                    _ => {}
                }
            }
        });
    }

    fn will_log(&self, label: &str) -> bool {
        matches!(label, CLIENT_HS | SERVER_HS | CLIENT_APP | SERVER_APP)
    }
}

/// A fixed wall-clock time for certificate validation, for tests and for
/// callers that supply time explicitly instead of letting rustls read the
/// system clock.
#[derive(Clone, Copy)]
pub struct FixedTime(pub UnixTime);

impl fmt::Debug for FixedTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FixedTime({})", self.0.as_secs())
    }
}

impl TimeProvider for FixedTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(self.0)
    }
}

/// The crypto provider: aws-lc-rs (already used by rustls in this
/// workspace and built on Linux, macOS AArch64 and Windows MSVC in CI),
/// restricted to the TLS 1.3 suites `vesper-warp-crypto` implements.
pub fn provider() -> Arc<CryptoProvider> {
    use rustls::crypto::aws_lc_rs::cipher_suite as cs;
    let mut p = rustls::crypto::aws_lc_rs::default_provider();
    p.cipher_suites = vec![
        cs::TLS13_AES_128_GCM_SHA256,
        cs::TLS13_AES_256_GCM_SHA384,
        cs::TLS13_CHACHA20_POLY1305_SHA256,
    ];
    Arc::new(p)
}

fn check_alpn(alpn: &[Vec<u8>]) -> Result<(), TlsError> {
    if alpn.is_empty() || alpn.iter().any(|p| p.is_empty() || p.len() > 255) {
        return Err(TlsError::Config(
            "QUIC requires at least one ALPN protocol of 1 to 255 bytes (RFC 9001 §8.1)".into(),
        ));
    }
    Ok(())
}

fn cfg_err(e: rustls::Error) -> TlsError {
    TlsError::Config(e.to_string())
}

/// A client configuration usable by [`crate::QuicTls::client`].
#[derive(Clone)]
pub struct ClientSetup {
    pub(crate) config: Arc<ClientConfig>,
}

impl ClientSetup {
    /// TLS 1.3 only, server authentication against `roots`, no client
    /// certificate, the given ALPN list, rustls's default clock.
    pub fn new(roots: RootCertStore, alpn: Vec<Vec<u8>>) -> Result<Self, TlsError> {
        Self::build(
            roots,
            alpn,
            Arc::new(rustls::time_provider::DefaultTimeProvider),
        )
    }

    /// As [`ClientSetup::new`], with certificate validity checked at the
    /// time `time` reports.
    pub fn with_time(
        roots: RootCertStore,
        alpn: Vec<Vec<u8>>,
        time: Arc<dyn TimeProvider>,
    ) -> Result<Self, TlsError> {
        Self::build(roots, alpn, time)
    }

    fn build(
        roots: RootCertStore,
        alpn: Vec<Vec<u8>>,
        time: Arc<dyn TimeProvider>,
    ) -> Result<Self, TlsError> {
        let config = ClientConfig::builder_with_details(provider(), time)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(cfg_err)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Self::from_config(config, alpn)
    }

    /// Uses a caller-built configuration (for example with a custom
    /// [`rustls::client::danger::ServerCertVerifier`]). 0-RTT is disabled and the secret export
    /// hook replaces any `key_log`.
    pub fn from_config(mut config: ClientConfig, alpn: Vec<Vec<u8>>) -> Result<Self, TlsError> {
        check_alpn(&alpn)?;
        config.alpn_protocols = alpn;
        config.enable_early_data = false;
        config.key_log = Arc::new(ExportSecrets);
        Ok(Self {
            config: Arc::new(config),
        })
    }

    pub fn config(&self) -> &Arc<ClientConfig> {
        &self.config
    }
}

/// A server configuration usable by [`crate::QuicTls::server`].
#[derive(Clone)]
pub struct ServerSetup {
    pub(crate) config: Arc<ServerConfig>,
}

impl ServerSetup {
    /// TLS 1.3 only, one certificate chain, no client authentication, the
    /// given ALPN list (the server's preference order).
    pub fn new(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        alpn: Vec<Vec<u8>>,
    ) -> Result<Self, TlsError> {
        let config = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(cfg_err)?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(cfg_err)?;
        Self::from_config(config, alpn)
    }

    /// Uses a caller-built configuration. 0-RTT is disabled and the secret
    /// export hook replaces any `key_log`.
    pub fn from_config(mut config: ServerConfig, alpn: Vec<Vec<u8>>) -> Result<Self, TlsError> {
        check_alpn(&alpn)?;
        config.alpn_protocols = alpn;
        config.max_early_data_size = 0;
        config.key_log = Arc::new(ExportSecrets);
        Ok(Self {
            config: Arc::new(config),
        })
    }

    pub fn config(&self) -> &Arc<ServerConfig> {
        &self.config
    }
}
