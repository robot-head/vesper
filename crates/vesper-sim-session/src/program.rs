//! Parsed and checked source owned independently of every session borrowing it.
use crate::machine::{Connection, Host};
use crate::world::World;
use serde_json::json;
#[cfg(not(feature = "vesper-port"))]
use sha2::{Digest, Sha256};

/// A source module accepted by the parser and the full checker, loaded in its
/// own `vesper sim-host` process. Sessions borrow this program, so the host
/// outlives every runtime and live `Suspension`.
pub struct CheckedProgram {
    source: String,
    world: World,
    digest: String,
    connection: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRejected {
    pub diagnostics: Vec<String>,
}
impl std::fmt::Display for SourceRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "source rejected: {}", self.diagnostics.join("; "))
    }
}
impl std::error::Error for SourceRejected {}

impl CheckedProgram {
    /// Parse and check `source`. Any parse or check diagnostic rejects it,
    /// as does a compiler that cannot be started.
    pub fn new(source: impl Into<String>) -> Result<Self, SourceRejected> {
        let source = source.into();
        let rejected = |message: String| SourceRejected {
            diagnostics: vec![message],
        };
        let connection = Connection::new(Host::start().map_err(rejected)?);
        crate::identity::prefetch_executable_sha256();
        let reply = connection
            .call(&json!({ "load": { "source": source } }))
            .map_err(rejected)?;
        if let Some(diagnostics) = reply.get("rejected").and_then(|d| d.as_array()) {
            return Err(SourceRejected {
                diagnostics: diagnostics
                    .iter()
                    .map(|d| d.as_str().unwrap_or_default().to_owned())
                    .collect(),
            });
        }
        let Some(facts) = reply.get("loaded") else {
            return Err(rejected(format!("unexpected sim-host load reply {reply}")));
        };
        let world = World::from_facts(facts.clone()).map_err(rejected)?;
        let digest = sha256_hex(source.as_bytes());
        Ok(Self {
            source,
            world,
            digest,
            connection,
        })
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn world(&self) -> &World {
        &self.world
    }
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }
    /// A fresh machine for this program, with the evaluator's defaults.
    pub fn runtime(&self) -> crate::machine::Runtime<'_> {
        crate::machine::Runtime::new(&self.connection)
    }
    /// SHA-256 of the exact source bytes that were checked.
    pub fn source_sha256(&self) -> &str {
        &self.digest
    }
}

#[cfg(feature = "vesper-port")]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    crate::port::sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(not(feature = "vesper-port"))]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
