//! Parsed and checked source owned independently of every session borrowing it.
use sha2::{Digest, Sha256};
use vesper::{check, check::World, parser};

/// A source module accepted by the parser and the full checker. Sessions borrow
/// this program, so its `World` outlives every runtime and live `Suspension`.
pub struct CheckedProgram {
    source: String,
    world: World,
    digest: String,
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
    /// Parse and check `source`. Any parse or check diagnostic rejects it.
    pub fn new(source: impl Into<String>) -> Result<Self, SourceRejected> {
        let source = source.into();
        let parsed = parser::parse(&source);
        let render = |d: &vesper::ast::Diagnostic| format!("{}: {}", d.code, d.message);
        if !parsed.diagnostics.is_empty() {
            return Err(SourceRejected {
                diagnostics: parsed.diagnostics.iter().map(render).collect(),
            });
        }
        let checked = check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            return Err(SourceRejected {
                diagnostics: checked.diagnostics.iter().map(render).collect(),
            });
        }
        let digest = sha256_hex(source.as_bytes());
        Ok(Self {
            source,
            world: checked.world,
            digest,
        })
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn world(&self) -> &World {
        &self.world
    }
    /// SHA-256 of the exact source bytes that were checked.
    pub fn source_sha256(&self) -> &str {
        &self.digest
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
