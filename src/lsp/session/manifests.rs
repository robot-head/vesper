//! Open `Loom.toml` buffers and the capability ceilings they declare.
//!
//! The server never reads the filesystem: a ceiling applies only while the
//! client has the manifest open, and only to Vesper documents whose URI lies
//! under the manifest's directory (the deepest such manifest wins). URIs are
//! compared as strings; nothing here resolves paths.
use super::{ServerError, notification};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use vesper_lsp::{ContentChange, DocumentStore, Envelope, StoreLimits};

/// Open manifests are few; anything beyond this is refused, not evicted.
const MAX_MANIFESTS: usize = 64;

pub(super) struct Manifests {
    store: DocumentStore,
    ceilings: BTreeMap<String, Result<BTreeSet<String>, String>>,
}

/// A URI whose last path segment is `Loom.toml`.
pub(super) fn is_manifest(uri: &str) -> bool {
    uri.rsplit('/').next() == Some("Loom.toml") && uri.contains('/')
}

impl Manifests {
    pub(super) fn new() -> Result<Self, String> {
        Ok(Self {
            store: DocumentStore::new(StoreLimits {
                max_documents: MAX_MANIFESTS,
                ..StoreLimits::default()
            })
            .map_err(|e| e.to_string())?,
            ceilings: BTreeMap::new(),
        })
    }
    pub(super) fn open(
        &mut self,
        uri: &str,
        version: i32,
        text: &str,
    ) -> Result<Envelope, ServerError> {
        self.store
            .open(uri, version, text)
            .map_err(|e| ServerError::params(e.to_string()))?;
        Ok(self.parse(uri))
    }
    pub(super) fn change(
        &mut self,
        uri: &str,
        version: i32,
        changes: &[ContentChange],
    ) -> Result<Envelope, ServerError> {
        self.store
            .change(uri, version, changes)
            .map_err(|e| ServerError::params(e.to_string()))?;
        Ok(self.parse(uri))
    }
    pub(super) fn close(&mut self, uri: &str) -> Result<Envelope, ServerError> {
        self.store
            .close(uri)
            .map_err(|e| ServerError::params(e.to_string()))?;
        self.ceilings.remove(uri);
        Ok(notification(
            "textDocument/publishDiagnostics",
            json!({"uri": uri, "diagnostics": []}),
        ))
    }
    /// Parse the stored text with Loom's own manifest validation. An invalid
    /// manifest grants nothing and constrains nothing: completion falls back
    /// to unconstrained ranking, and the manifest shows why.
    fn parse(&mut self, uri: &str) -> Envelope {
        let (text, version) = self
            .store
            .get(uri)
            .map_or(("", 0), |d| (d.text(), d.version()));
        let parsed = vesper_loom_calculus::parse_manifest(text)
            .map(|m| m.package.capability_ceiling)
            .map_err(|errors| {
                errors
                    .iter()
                    .take(8)
                    .map(|e| e.message.chars().take(512).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("; ")
            });
        let diagnostics = match &parsed {
            Ok(_) => vec![],
            Err(message) => vec![json!({
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                "severity": 2,
                "source": "vesper-lens",
                "code": "VESPER_MANIFEST_UNAVAILABLE",
                "message": format!("Loom.toml capability ceiling unavailable: {message}"),
            })],
        };
        self.ceilings.insert(uri.to_owned(), parsed);
        notification(
            "textDocument/publishDiagnostics",
            json!({"uri": uri, "version": version, "diagnostics": diagnostics}),
        )
    }
    /// The ceiling of the deepest open, valid manifest above `uri`. An invalid
    /// deeper manifest hides shallower ones: its package's ceiling is unknown.
    pub(super) fn ceiling_for(&self, uri: &str) -> Option<&BTreeSet<String>> {
        self.ceilings
            .iter()
            .filter(|(manifest, _)| {
                let dir = &manifest[..manifest.len() - "Loom.toml".len()];
                uri.starts_with(dir)
            })
            .max_by_key(|(manifest, _)| manifest.len())
            .and_then(|(_, ceiling)| ceiling.as_ref().ok())
    }
}
