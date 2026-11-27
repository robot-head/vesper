//! Explicit pure Tier1 expansion previews, associated with current editor text.
//!
//! This codec exposes the compiler's expanded-source artifact, never an original
//! source semantic overlay. URIs are labels; neither construction nor rendering
//! performs I/O or executes generated code, components, or proofs.
use crate::{
    ast::Diagnostic,
    database::macro_preview::MacroSnapshot,
    forge_frontend::{ExpandedChecked, InvocationAudit, SourceMapSegment, Stage},
};
use serde::Serialize;
use serde_json::Value;
use std::{fmt, io::Write};

pub const SCHEMA: &str = "vesper.editor.macro-preview/1";
const MAX_SOURCE: usize = 128 * 1024;
const MAX_URI: usize = 4096;
const MAX_OUTPUT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_uri_bytes: usize,
    pub max_output_bytes: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: MAX_SOURCE,
            max_uri_bytes: MAX_URI,
            max_output_bytes: MAX_OUTPUT,
            max_work: 16 * 1024 * 1024,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<(), Error> {
        if self.max_source_bytes == 0
            || self.max_source_bytes > MAX_SOURCE
            || self.max_uri_bytes == 0
            || self.max_uri_bytes > MAX_URI
            || self.max_output_bytes == 0
            || self.max_output_bytes > MAX_OUTPUT
            || self.max_work == 0
            || self.max_work > 32 * 1024 * 1024
        {
            Err(Error::InvalidLimits)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    InvalidDocument(&'static str),
    SourceMismatch,
    Resource(&'static str),
    Serialization,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("invalid macro preview limits"),
            Self::InvalidDocument(reason) => write!(f, "invalid macro preview document: {reason}"),
            Self::SourceMismatch => f.write_str("macro preview source association differs"),
            Self::Resource(reason) => write!(f, "macro preview resource limit: {reason}"),
            Self::Serialization => f.write_str("macro preview serialization failed"),
        }
    }
}
impl std::error::Error for Error {}

/// Borrowed association with a real complete compiler snapshot. Reconstruct it
/// after each accepted change; no caller-supplied approval or World is accepted.
pub struct Document<'a> {
    uri: &'a str,
    version: i32,
    source: &'a str,
    hash: [u8; 32],
    snapshot: &'a MacroSnapshot,
}
impl<'a> Document<'a> {
    pub fn new(
        uri: &'a str,
        version: i32,
        source: &'a str,
        snapshot: &'a MacroSnapshot,
    ) -> Result<Self, Error> {
        // Bound all comparisons/hashing before observing potentially oversized
        // legacy inputs. At most four source-sized traversals (<1 Mi units).
        if uri.is_empty() || uri.len() > MAX_URI {
            return Err(Error::InvalidDocument("URI byte limit"));
        }
        if source.len() > MAX_SOURCE {
            return Err(Error::Resource("source bytes"));
        }
        if snapshot.source() != Some(source)
            || snapshot.source_identity().byte_len() != source.len()
        {
            return Err(Error::SourceMismatch);
        }
        let hash = *blake3::hash(source.as_bytes()).as_bytes();
        if snapshot.source_identity().blake3() != Some(hash) {
            return Err(Error::SourceMismatch);
        }
        if let Ok(expanded) = snapshot.expanded()
            && (expanded.original_source() != source || expanded.original_hash() != hash)
        {
            return Err(Error::SourceMismatch);
        }
        Ok(Self {
            uri,
            version,
            source,
            hash,
            snapshot,
        })
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    schema: &'static str,
    scope: &'static str,
    tier: u8,
    uri: &'a str,
    version: i32,
    source_blake3: String,
    range_unit: &'static str,
}
impl<'a> From<&Document<'a>> for Envelope<'a> {
    fn from(document: &Document<'a>) -> Self {
        Self {
            schema: SCHEMA,
            scope: "expanded_source_only",
            tier: 1,
            uri: document.uri,
            version: document.version,
            source_blake3: blake3::Hash::from_bytes(document.hash).to_hex().to_string(),
            range_unit: "utf8_bytes",
        }
    }
}
#[derive(Serialize)]
struct Success<'a> {
    #[serde(flatten)]
    envelope: Envelope<'a>,
    status: &'static str,
    expanded_blake3: String,
    package: &'a str,
    source_bytes: usize,
    expanded_bytes: usize,
    macro_definitions: usize,
    invocations: usize,
    expanded_source: &'a str,
    source_map: &'a [SourceMapSegment],
    audits: &'a [InvocationAudit],
}
impl<'a> Success<'a> {
    fn new(document: &Document<'a>, expanded: &'a ExpandedChecked) -> Self {
        Self {
            envelope: document.into(),
            status: "expanded_checked",
            expanded_blake3: blake3::Hash::from_bytes(expanded.expanded_hash())
                .to_hex()
                .to_string(),
            package: expanded.package(),
            source_bytes: document.source.len(),
            expanded_bytes: expanded.expanded_source().len(),
            macro_definitions: expanded.macro_definitions(),
            invocations: expanded.invocations(),
            expanded_source: expanded.expanded_source(),
            source_map: expanded.source_map(),
            audits: expanded.audits(),
        }
    }
}
#[derive(Serialize)]
struct Rejected<'a> {
    #[serde(flatten)]
    envelope: Envelope<'a>,
    status: &'static str,
    stage: Stage,
    message: &'a str,
    expanded_source: Option<&'a str>,
    diagnostics: &'a [Diagnostic],
    diagnostic_source: &'static str,
    source_map: [(); 0],
    audits: [(); 0],
}

struct Output {
    bytes: Vec<u8>,
    cap: usize,
    work: usize,
    refused: Option<&'static str>,
}
impl Output {
    fn refuse(&mut self, reason: &'static str) -> std::io::Error {
        self.refused = Some(reason);
        std::io::Error::other("macro preview output refused")
    }
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(work) = bytes.len().checked_add(1) else {
            return Err(self.refuse("serialization work"));
        };
        let Some(remaining) = self.work.checked_sub(work) else {
            return Err(self.refuse("serialization work"));
        };
        self.work = remaining;
        let Some(needed) = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= self.cap)
        else {
            return Err(self.refuse("output bytes"));
        };
        if needed > self.bytes.capacity() {
            // Geometric growth bounds cumulative copying; never reserve an
            // uncapped compiler payload or materialize a JSON Value first.
            let capacity = needed
                .max(self.bytes.capacity().saturating_mul(2))
                .max(256)
                .min(self.cap);
            if self
                .bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .is_err()
            {
                return Err(self.refuse("output allocation"));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(value: &impl Serialize, output: &mut Output) -> Result<Value, Error> {
    if serde_json::to_writer(&mut *output, value).is_err() {
        return Err(output.refused.map_or(Error::Serialization, Error::Resource));
    }
    // Only the completely serialized, bounded payload reaches this allocation.
    // Its parse and payload-copy envelope was reserved before serialization.
    serde_json::from_slice(&output.bytes).map_err(|_| Error::Serialization)
}

/// Render only after a complete association and bounded serialization succeeds.
pub fn render(document: &Document<'_>, limits: Limits) -> Result<Value, Error> {
    limits.validate()?;
    if document.source.len() > limits.max_source_bytes {
        return Err(Error::Resource("source bytes"));
    }
    if document.uri.len() > limits.max_uri_bytes {
        return Err(Error::Resource("URI bytes"));
    }
    // Prepay parsing, payload copies, geometric buffer copies, and fixed DTO
    // construction from one allowance. Serializer writes additionally charge
    // actual bytes plus calls, bounding traversals even on a rejected payload.
    let reserved = limits
        .max_output_bytes
        .checked_mul(8)
        .and_then(|n| n.checked_add(document.source.len().checked_mul(4)?))
        .and_then(|n| n.checked_add(document.uri.len()))
        .and_then(|n| n.checked_add(4096))
        .ok_or(Error::Resource("work"))?;
    let work = limits
        .max_work
        .checked_sub(reserved)
        .ok_or(Error::Resource("work"))?;
    let mut output = Output {
        bytes: Vec::new(),
        cap: limits.max_output_bytes,
        work,
        refused: None,
    };
    match document.snapshot.expanded() {
        Ok(expanded) => encode(&Success::new(document, expanded), &mut output),
        Err(failure) => {
            let diagnostic_source = if failure.stage() == Stage::Parse {
                "original"
            } else if failure.expanded_source().is_some() {
                "expanded"
            } else {
                "none"
            };
            encode(
                &Rejected {
                    envelope: document.into(),
                    status: "rejected",
                    stage: failure.stage(),
                    message: failure.message(),
                    expanded_source: failure.expanded_source(),
                    diagnostics: failure.diagnostics(),
                    diagnostic_source,
                    source_map: [],
                    audits: [],
                },
                &mut output,
            )
        }
    }
}

/// The words the Vesper port (`@vesper/lsp-features`, macro_preview.ves)
/// reads for one preview: `rounds`, the version, the four limits, the URI
/// and document text, the document's BLAKE3 digest when the host computes
/// it (the port hashes with the portable BLAKE3 otherwise), the snapshot's
/// association and its compiler outcome.
/// Each text is its byte length then its bytes; each list its length then
/// its entries.
#[doc(hidden)]
pub fn port_words(
    uri: &str,
    version: i32,
    source: &str,
    snapshot: &MacroSnapshot,
    limits: Limits,
    host_digest: Option<[u8; 32]>,
    rounds: u64,
) -> Vec<u8> {
    let mut w = PortWords(Vec::new());
    w.word(rounds);
    w.word(i64::from(version) as u64);
    for n in [
        limits.max_source_bytes,
        limits.max_uri_bytes,
        limits.max_output_bytes,
        limits.max_work,
    ] {
        w.word(n as u64);
    }
    w.text(uri);
    w.text(source);
    w.flag(host_digest.is_some());
    w.0.extend_from_slice(&host_digest.unwrap_or([0; 32]));
    w.flag(snapshot.source().is_some());
    w.text(snapshot.source().unwrap_or(""));
    w.word(snapshot.source_identity().byte_len() as u64);
    let identity = snapshot.source_identity().blake3();
    w.flag(identity.is_some());
    w.0.extend_from_slice(&identity.unwrap_or([0; 32]));
    match snapshot.expanded() {
        Ok(expanded) => {
            w.flag(true);
            w.text(expanded.original_source());
            w.0.extend_from_slice(&expanded.original_hash());
            w.0.extend_from_slice(&expanded.expanded_hash());
            w.text(expanded.package());
            w.text(expanded.expanded_source());
            w.word(expanded.macro_definitions() as u64);
            w.word(expanded.invocations() as u64);
            w.word(expanded.source_map().len() as u64);
            for s in expanded.source_map() {
                w.range(&s.expanded);
                w.range(&s.original);
                w.flag(s.kind == crate::forge_frontend::MapKind::Generated);
                w.option(s.r#macro.as_deref());
            }
            w.word(expanded.audits().len() as u64);
            for a in expanded.audits() {
                w.text(&a.r#macro);
                w.range(&a.invocation);
                w.text(a.scope);
                w.audit(&a.audit);
            }
        }
        Err(failure) => {
            w.flag(false);
            w.word(failure.stage() as u64);
            w.text(failure.message());
            w.option(failure.expanded_source());
            w.word(failure.diagnostics().len() as u64);
            for d in failure.diagnostics() {
                w.text(&d.code);
                w.text(&d.message);
                w.span(&d.span);
                w.word(d.related.len() as u64);
                for label in &d.related {
                    w.span(&label.span);
                    w.text(&label.message);
                }
                w.option(d.help.as_deref());
            }
        }
    }
    w.0
}

// The `port_words` tags of `OperationKind` and `AuthorityArg` variants
// (macro_preview.ves reads the same values).
const OP_USE: u64 = 0;
const OP_HANDLER: u64 = 1;
const OP_INDIRECT_CALL: u64 = 2;
const ARG_DEF: u64 = 0;
const ARG_TEXT: u64 = 1;
const ARG_INT: u64 = 2;

struct PortWords(Vec<u8>);
impl PortWords {
    fn word(&mut self, x: u64) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn flag(&mut self, b: bool) {
        self.word(u64::from(b));
    }
    fn text(&mut self, s: &str) {
        self.word(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn option(&mut self, s: Option<&str>) {
        self.flag(s.is_some());
        self.text(s.unwrap_or(""));
    }
    fn range(&mut self, r: &crate::forge_frontend::ByteRange) {
        self.word(r.start as u64);
        self.word(r.end as u64);
    }
    fn span(&mut self, r: &std::ops::Range<usize>) {
        self.word(r.start as u64);
        self.word(r.end as u64);
    }
    fn def(&mut self, d: &vesper_forge::DefId) {
        self.word(u64::from(d.package.0));
        self.word(u64::from(d.item));
    }
    fn expansion(&mut self, e: vesper_forge::ExpansionId) {
        self.word(e.session().0);
        self.word(u64::from(e.index()));
    }
    fn operation(&mut self, op: &vesper_forge::OperationKind) {
        use vesper_forge::OperationKind;
        match op {
            OperationKind::Use { def } => {
                self.word(OP_USE);
                self.def(def);
            }
            OperationKind::Handler { effect, handler } => {
                self.word(OP_HANDLER);
                self.def(effect);
                self.def(handler);
            }
            OperationKind::IndirectCall => self.word(OP_INDIRECT_CALL),
        }
    }
    fn authority(&mut self, a: &vesper_forge::Authority) {
        use vesper_forge::AuthorityArg;
        self.word(u64::from(a.effect.package.0));
        self.text(&a.effect.name);
        self.word(a.args.len() as u64);
        for arg in &a.args {
            match arg {
                AuthorityArg::Def(d) => {
                    self.word(ARG_DEF);
                    self.def(d);
                }
                AuthorityArg::Text(s) => {
                    self.word(ARG_TEXT);
                    self.text(s);
                }
                AuthorityArg::Int(n) => {
                    self.word(ARG_INT);
                    self.word(*n);
                }
            }
        }
    }
    fn audit(&mut self, a: &vesper_forge::AuthorityAudit) {
        self.word(u64::from(a.schema_version));
        self.word(a.session.0);
        self.text(&a.graph_version);
        self.word(a.policies.len() as u64);
        for p in &a.policies {
            self.expansion(p.expansion);
            self.def(&p.macro_def);
            self.word(p.macro_policy_version);
            self.word(u64::from(p.caller.0));
            self.word(p.caller_ceiling_version);
        }
        self.word(a.operations.len() as u64);
        for op in &a.operations {
            self.word(op.token as u64);
            self.operation(&op.operation);
            self.word(op.expansions.len() as u64);
            for e in &op.expansions {
                self.expansion(*e);
            }
            self.word(op.requirements.len() as u64);
            for r in &op.requirements {
                self.authority(r);
            }
        }
        self.word(a.source_operations.len() as u64);
        for op in &a.source_operations {
            self.operation(op);
        }
    }
}
