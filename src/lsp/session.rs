//! In-memory bounded editor lifecycle. URIs are labels, never filesystem paths.
use super::{actions, macro_preview, navigation, search};
mod lens_requests;
use crate::{
    database::{self, Database, RecoveryFile, SourceFile, SyntaxSnapshot},
    tooling::{
        frontend::{AnalysisFailure, CheckedSource},
        lint::ConfigProvenance,
    },
};
pub use lens_requests::LensConfig;
pub(crate) use lens_requests::{CommandOutcome, TimerOutcome};
use salsa::Setter;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};
use vesper_lint::policy::Policy;
use vesper_lsp::{ContentChange, DocumentStore, Envelope, Position, Range, StoreLimits};
use vesper_syntax::{
    incremental::{SyntaxDocument, UpdateKind},
    recovery::{self, RecoverySyntax},
};

const RESET_REVISIONS: usize = 64;
const ANALYSIS_UNAVAILABLE: i32 = -32003;

#[derive(Debug)]
pub(crate) struct ServerError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}
impl ServerError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    fn params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }
    fn internal(message: impl Into<String>) -> Self {
        Self::new(-32603, message)
    }
}
#[derive(Debug)]
pub(crate) enum NotificationOutcome {
    Continue(Vec<Envelope>),
    Exit { clean: bool },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    Uninitialized,
    Running,
    Shutdown,
    Exited,
}

/// Syntax and database inputs always advance together after a successful store
/// transaction. A failed syntax update drops the old mirror for that new text.
enum RecoveryMirror {
    Ready {
        document: SyntaxDocument,
        file: RecoveryFile,
        update_kind: &'static str,
    },
    Unavailable {
        reason: String,
    },
}

pub(crate) struct Session {
    state: Lifecycle,
    documents: DocumentStore,
    db: Database,
    files: BTreeMap<String, SourceFile>,
    recovery: BTreeMap<String, RecoveryMirror>,
    accepted_revisions: usize,
    policy: Policy,
    hierarchical_symbols: bool,
    code_actions_supported: bool,
    diagnostic_related: bool,
    diagnostic_help_data: bool,
    lens_views: RefCell<LensViews>,
    proofs: vesper_lsp::lens::ProofLedger,
    /// Semantic proof outcomes keyed by exact model identity, so a function
    /// whose model did not change keeps its outcome across edits.
    model_proofs: vesper_lsp::lens::ProofLedger,
    lens_models: RefCell<LensModels>,
    lens_config: LensConfig,
    background: Option<vesper_lsp::lens::Debouncer>,
    timers: Vec<(String, std::time::Duration)>,
    code_lens_refresh: bool,
    refreshes: u64,
}
type LensModels = BTreeMap<
    String,
    (
        [u8; 32],
        Arc<BTreeMap<String, Result<super::proof_model::Model, String>>>,
    ),
>;
type LensViews = BTreeMap<
    String,
    (
        [u8; 32],
        Result<Arc<super::lens::View>, super::lens::Unavailable>,
    ),
>;
impl Session {
    pub(crate) fn new() -> Result<Self, String> {
        Ok(Self {
            state: Lifecycle::Uninitialized,
            documents: DocumentStore::new(StoreLimits::default()).map_err(|e| e.to_string())?,
            db: Database::default(),
            files: BTreeMap::new(),
            recovery: BTreeMap::new(),
            accepted_revisions: 0,
            policy: Policy::default(),
            hierarchical_symbols: false,
            code_actions_supported: false,
            diagnostic_related: false,
            diagnostic_help_data: false,
            lens_views: RefCell::new(BTreeMap::new()),
            proofs: vesper_lsp::lens::ProofLedger::new(1024).map_err(|e| e.to_string())?,
            model_proofs: vesper_lsp::lens::ProofLedger::new(4096).map_err(|e| e.to_string())?,
            lens_models: RefCell::new(BTreeMap::new()),
            lens_config: LensConfig::default(),
            background: None,
            timers: Vec::new(),
            code_lens_refresh: false,
            refreshes: 0,
        })
    }
    pub(crate) fn with_config(config: LensConfig) -> Result<Self, String> {
        let mut session = Self::new()?;
        session.set_lens_config(config);
        Ok(session)
    }
    fn running(&self) -> Result<(), ServerError> {
        match self.state {
            Lifecycle::Running => Ok(()),
            Lifecycle::Uninitialized => Err(ServerError::new(-32002, "Server not initialized")),
            Lifecycle::Shutdown | Lifecycle::Exited => {
                Err(ServerError::new(-32600, "Server has shut down"))
            }
        }
    }
    pub(crate) fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ServerError> {
        validate_input(method, params.as_ref())?;
        if method == "initialize" {
            if self.state != Lifecycle::Uninitialized {
                return Err(ServerError::new(
                    -32600,
                    "initialize may be requested only once",
                ));
            }
            let initialize = object(params.as_ref())?;
            let publishes = initialize
                .get("capabilities")
                .and_then(|value| value.pointer("/textDocument/publishDiagnostics"));
            self.diagnostic_related = publishes
                .and_then(|value| value.get("relatedInformation"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            self.diagnostic_help_data = publishes
                .and_then(|value| value.get("dataSupport"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            self.hierarchical_symbols = initialize
                .get("capabilities")
                .and_then(|value| {
                    value.pointer("/textDocument/documentSymbol/hierarchicalDocumentSymbolSupport")
                })
                .and_then(Value::as_bool)
                .unwrap_or(false);
            self.code_actions_supported =
                initialize.get("capabilities").is_some_and(|capabilities| {
                    capabilities
                        .pointer("/workspace/workspaceEdit/documentChanges")
                        .and_then(Value::as_bool)
                        == Some(true)
                        && capabilities
                            .pointer("/textDocument/codeAction/codeActionLiteralSupport")
                            .is_some_and(Value::is_object)
                });
            self.configure_background(initialize);
            let code_actions = if self.code_actions_supported {
                json!({"codeActionKinds":["quickfix","source.fixAll.vesper"],"resolveProvider":false})
            } else {
                Value::Bool(false)
            };
            self.state = Lifecycle::Running;
            return Ok(json!({"capabilities":{
                "positionEncoding":"utf-16",
                "textDocumentSync":{"openClose":true,"change":2},
                "hoverProvider":true,"definitionProvider":true,
                "documentSymbolProvider":true,"workspaceSymbolProvider":true,
                "documentFormattingProvider":true,
                "codeActionProvider":code_actions,
                "completionProvider":{"triggerCharacters":["."],"resolveProvider":false},
                "inlayHintProvider":true,
                "codeLensProvider":{"resolveProvider":false},
                "executeCommandProvider":{"commands":[lens_requests::PROOF_COMMAND]},
                "experimental":{
                    "vesperLens":{"version":1,"poisonedRecovery":"VESPER_POISONED_RECOVERY",
                        "completion":"obligation_ranked_members",
                        "proofLenses":[super::proof_lens::EXTRACTION_SCOPE,
                            super::proof_lens::SEMANTIC_SCOPE],
                        "backgroundProofs":self.background.as_ref()
                            .map(|b| json!({"debounceMs": b.delay().as_millis() as u64}))
                            .unwrap_or(Value::Null),
                        "asyncCancellation":"all_requests"},
                    "vesperRecoveryStatus":{"scope":"syntax_only","version":1},
                    "vesperMacroPreview":{"version":1,"tier":1,"scope":"expanded_source_only"}
                }
            },"serverInfo":{"name":"vesper","version":env!("CARGO_PKG_VERSION")}}));
        }
        self.running()?;
        if method == "shutdown" {
            if params.as_ref().is_some_and(|value| {
                !value.is_null() && !value.as_object().is_some_and(|object| object.is_empty())
            }) {
                return Err(ServerError::params("shutdown takes no parameters"));
            }
            self.state = Lifecycle::Shutdown;
            return Ok(Value::Null);
        }
        if !matches!(
            method,
            "textDocument/hover"
                | "textDocument/definition"
                | "textDocument/documentSymbol"
                | "workspace/symbol"
                | "vesper/signatureSearch"
                | "vesper/recoveryStatus"
                | "vesper/macroPreview"
                | "textDocument/formatting"
                | "textDocument/codeAction"
                | "textDocument/completion"
                | "textDocument/inlayHint"
                | "textDocument/codeLens"
        ) {
            return Err(ServerError::new(-32601, "Method not found"));
        }
        let params = object(params.as_ref())?;
        match method {
            "vesper/macroPreview" => self.macro_preview(params),
            "vesper/recoveryStatus" => self.recovery_status(document_uri(params)?),
            "textDocument/completion" => self.completion(params),
            "textDocument/inlayHint" => self.inlay_hints(params),
            "textDocument/codeLens" => self.code_lenses(params),
            "textDocument/hover" => {
                let uri = document_uri(params)?;
                let at = position(field(params, "position")?)?;
                let base = self.snapshot(uri).and_then(|snapshot| {
                    navigation::hover(
                        &snapshot,
                        self.document(uri)?.index(),
                        at,
                        navigation::Limits::default(),
                    )
                    .map(|value| value.unwrap_or(Value::Null))
                    .map_err(nav_error)
                });
                self.hover_with_obligations(uri, at, base)
            }
            "textDocument/definition" => {
                let uri = document_uri(params)?;
                let at = position(field(params, "position")?)?;
                let snapshot = self.snapshot(uri)?;
                let document = self.document(uri)?;
                let limits = navigation::Limits::default();
                navigation::definition(uri, &snapshot, document.index(), at, limits)
                    .map(|value| value.unwrap_or(Value::Null))
                    .map_err(nav_error)
            }
            "textDocument/documentSymbol" => {
                let uri = document_uri(params)?;
                let snapshot = self.snapshot(uri)?;
                let symbols = navigation::document_symbols(
                    &snapshot,
                    self.document(uri)?.index(),
                    navigation::Limits::default(),
                )
                .map_err(nav_error)?;
                Ok(Value::Array(if self.hierarchical_symbols {
                    symbols
                } else {
                    symbols
                        .into_iter()
                        .map(|symbol| {
                            json!({"name":symbol["name"],"kind":symbol["kind"],
                        "location":{"uri":uri,"range":symbol["range"]}})
                        })
                        .collect()
                }))
            }
            "workspace/symbol" => {
                let query = string(field(params, "query")?, "query")?;
                if query.len() > navigation::Limits::default().max_query_bytes {
                    return Err(ServerError::params("workspace symbol query is too long"));
                }
                let snapshots = self.open_snapshots(None)?;
                let documents: Vec<_> = snapshots
                    .iter()
                    .map(|(uri, snapshot)| navigation::OpenDocument {
                        uri,
                        snapshot,
                        index: self
                            .documents
                            .get(uri)
                            .expect("validated open document")
                            .index(),
                    })
                    .collect();
                navigation::workspace_symbols(&documents, query, navigation::Limits::default())
                    .map(Value::Array)
                    .map_err(nav_error)
            }
            "vesper/signatureSearch" => {
                let query = field(params, "query")?;
                if !query.is_object() {
                    return Err(ServerError::params("query must be an object"));
                }
                // Validate the structural query even when no buffer is open,
                // and before a source-specific failure can obscure bad params.
                search::signature_search(&[], query).map_err(search_error)?;
                let uri = params
                    .get("uri")
                    .map(|value| string(value, "uri"))
                    .transpose()?;
                let snapshots = self.open_snapshots(uri)?;
                let documents: Vec<_> = snapshots
                    .iter()
                    .map(|(uri, snapshot)| (*uri, snapshot.as_ref()))
                    .collect();
                search::signature_search(&documents, query).map_err(search_error)
            }
            "textDocument/formatting" => {
                let uri = document_uri(params)?;
                let document = self.document(uri)?;
                let action = actions::ActionDocument::new(
                    uri,
                    document.version(),
                    document.text(),
                    document.index(),
                )
                .map_err(action_error)?;
                actions::format_document(&action, action.stamp())
                    .map(|formatted| formatted.to_json())
                    .map_err(action_error)
            }
            "textDocument/codeAction" => self.code_actions(params),
            _ => Err(ServerError::new(-32601, "Method not found")),
        }
    }

    pub(crate) fn notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<NotificationOutcome, ServerError> {
        validate_input(method, params.as_ref())?;
        if method == "exit" {
            let clean = self.state == Lifecycle::Shutdown;
            self.state = Lifecycle::Exited;
            return Ok(NotificationOutcome::Exit { clean });
        }
        self.running()?;
        let output = match method {
            "textDocument/didOpen" => {
                let params = object(params.as_ref())?;
                let doc = object(Some(field(params, "textDocument")?))?;
                let uri = string(field(doc, "uri")?, "uri")?;
                let version = version(field(doc, "version")?)?;
                let text = string(field(doc, "text")?, "text")?;
                self.documents
                    .open(uri, version, text)
                    .map_err(|e| ServerError::params(e.to_string()))?;
                self.accepted(uri);
                self.touch_background(uri);
                vec![self.diagnostics(uri)?]
            }
            "textDocument/didChange" => {
                let params = object(params.as_ref())?;
                let doc = object(Some(field(params, "textDocument")?))?;
                let uri = string(field(doc, "uri")?, "uri")?;
                let version = version(field(doc, "version")?)?;
                let changes = field(params, "contentChanges")?
                    .as_array()
                    .ok_or_else(|| ServerError::params("contentChanges must be an array"))?;
                if changes.len() > StoreLimits::default().max_changes {
                    return Err(ServerError::params("too many content changes"));
                }
                let changes: Vec<_> = changes
                    .iter()
                    .map(|change| {
                        let change = object(Some(change))?;
                        let range = change
                            .get("range")
                            .filter(|value| !value.is_null())
                            .map(range)
                            .transpose()?;
                        let range_length = change
                            .get("rangeLength")
                            .map(|value| unsigned(value, "rangeLength"))
                            .transpose()?;
                        Ok(ContentChange {
                            range,
                            range_length,
                            text: string(field(change, "text")?, "text")?,
                        })
                    })
                    .collect::<Result<_, ServerError>>()?;
                self.documents
                    .change(uri, version, &changes)
                    .map_err(|e| ServerError::params(e.to_string()))?;
                self.accepted(uri);
                self.touch_background(uri);
                vec![self.diagnostics(uri)?]
            }
            "textDocument/didClose" => {
                let params = object(params.as_ref())?;
                let uri = document_uri(params)?;
                self.documents
                    .close(uri)
                    .map_err(|e| ServerError::params(e.to_string()))?;
                self.forget_background(uri);
                self.reset_database();
                vec![notification(
                    "textDocument/publishDiagnostics",
                    json!({"uri":uri,"diagnostics":[]}),
                )]
            }
            // No background execution, cancellation task, trace output or file
            // read is associated with these or unknown notifications.
            _ => Vec::new(),
        };
        Ok(NotificationOutcome::Continue(output))
    }

    fn document(&self, uri: &str) -> Result<&vesper_lsp::Document, ServerError> {
        self.documents
            .get(uri)
            .ok_or_else(|| ServerError::params("document URI is not open"))
    }
    fn macro_preview(&self, params: &serde_json::Map<String, Value>) -> Result<Value, ServerError> {
        if params.len() != 1 || !params.contains_key("textDocument") {
            return Err(ServerError::params("macro preview takes only textDocument"));
        }
        let text_document = object(Some(field(params, "textDocument")?))?;
        if text_document
            .keys()
            .any(|key| key != "uri" && key != "version")
        {
            return Err(ServerError::params("unknown macro preview document field"));
        }
        let uri = string(field(text_document, "uri")?, "uri")?;
        let expected = text_document.get("version").map(version).transpose()?;
        let current = self.document(uri)?;
        if expected.is_some_and(|expected| expected != current.version()) {
            return Err(ServerError::new(-32801, "Document version has changed"));
        }
        let unavailable = || ServerError {
            code: ANALYSIS_UNAVAILABLE,
            message: "Macro preview is unavailable".into(),
            data: Some(json!({"status":"preview_unavailable","scope":"expanded_source_only"})),
        };
        let limits = macro_preview::Limits::default();
        // Bound the exact mirror comparison before traversing either source.
        // URI labels never cause filesystem access or execution.
        if current.text().len() > limits.max_source_bytes || uri.len() > limits.max_uri_bytes {
            return Err(unavailable());
        }
        let file = *self.files.get(uri).ok_or_else(unavailable)?;
        if file.text(&self.db) != current.text() {
            return Err(unavailable());
        }
        let snapshot = database::macro_preview::preview(&self.db, file);
        let document =
            macro_preview::Document::new(uri, current.version(), current.text(), snapshot)
                .map_err(|_| unavailable())?;
        // This result never replaces guarded_analysis or changes diagnostics.
        macro_preview::render(&document, limits).map_err(|_| unavailable())
    }
    fn snapshot(&self, uri: &str) -> Result<Arc<CheckedSource>, ServerError> {
        let document = self.document(uri)?;
        let file = *self
            .files
            .get(uri)
            .ok_or_else(|| ServerError::internal("open source mirror is missing"))?;
        let analysis = database::guarded_analysis(&self.db, file);
        analysis.checked_arc().map_err(|failure| ServerError {
            code: ANALYSIS_UNAVAILABLE,
            message: "Checked source analysis is unavailable".into(),
            data: Some(
                json!({"status":"analysis_unavailable","kind":failure_kind(failure),
                "reason":failure.to_string(),"uri":uri,"version":document.version(),
                "source_blake3":hash_text(analysis.source_identity().blake3()),
                "diagnostics":failure.diagnostics()}),
            ),
        })
    }
    fn open_snapshots(
        &self,
        only: Option<&str>,
    ) -> Result<Vec<(&str, Arc<CheckedSource>)>, ServerError> {
        if let Some(uri) = only {
            let document = self.document(uri)?;
            return Ok(vec![(document.uri(), self.snapshot(uri)?)]);
        }
        self.documents
            .iter()
            .map(|document| Ok((document.uri(), self.snapshot(document.uri())?)))
            .collect()
    }
    fn accepted(&mut self, uri: &str) {
        self.accepted_revisions += 1;
        if self.accepted_revisions >= RESET_REVISIONS {
            self.reset_database();
            return;
        }
        // The store transaction has already checked all byte/document limits.
        let document = self
            .documents
            .get(uri)
            .expect("accepted transaction retained document");
        if let Some(file) = self.files.get(uri) {
            // The store already bounded both texts. Preserve the actual input
            // and its memoized results for byte-identical version-only changes.
            if file.text(&self.db) != document.text() {
                file.set_text(&mut self.db).to(document.text().to_owned());
            }
        } else {
            self.files.insert(
                uri.to_owned(),
                SourceFile::new(&self.db, document.text().to_owned()),
            );
        }
        let text = document.text();
        let mirror = match self.recovery.remove(uri) {
            Some(RecoveryMirror::Ready {
                mut document, file, ..
            }) => match document.update(text) {
                Ok(kind) => {
                    file.set_snapshot(&mut self.db)
                        .to(SyntaxSnapshot::new(Arc::clone(document.snapshot())));
                    RecoveryMirror::Ready {
                        document,
                        file,
                        update_kind: update_label(kind),
                    }
                }
                Err(error) => RecoveryMirror::Unavailable {
                    reason: error.to_string(),
                },
            },
            _ => open_recovery(&self.db, text),
        };
        self.recovery.insert(uri.to_owned(), mirror);
    }
    fn reset_database(&mut self) {
        // Every old SourceFile is discarded with its database. No stale IDs
        // survive in the mirror, including IDs of still-open documents.
        self.files.clear();
        self.recovery.clear();
        // Cached Lens views hold whole checker states; keep only open buffers
        // reachable, and rebuild them lazily from the new database.
        self.lens_views.borrow_mut().clear();
        self.lens_models.borrow_mut().clear();
        self.db = Database::default();
        for document in self.documents.iter() {
            self.files.insert(
                document.uri().to_owned(),
                SourceFile::new(&self.db, document.text().to_owned()),
            );
            self.recovery.insert(
                document.uri().to_owned(),
                open_recovery(&self.db, document.text()),
            );
        }
        self.accepted_revisions = 0;
    }

    fn recovery_snapshot(
        &self,
        uri: &str,
    ) -> Result<(&RecoverySyntax, RecoveryFile, &'static str), ServerError> {
        let current = self.document(uri)?;
        let unavailable = |reason: &str| ServerError {
            code: ANALYSIS_UNAVAILABLE,
            message: "Recovery syntax is unavailable".into(),
            data: Some(json!({"status":"analysis_unavailable","kind":"recovery",
                "reason":reason,"uri":uri,"version":current.version(),
                "source_blake3":blake3::hash(current.text().as_bytes()).to_hex().as_str()})),
        };
        match self.recovery.get(uri) {
            Some(RecoveryMirror::Ready {
                document,
                file,
                update_kind,
            }) => {
                let syntax = document.snapshot().as_ref();
                if syntax.source() != current.text() {
                    return Err(unavailable("current source identity mismatch"));
                }
                Ok((syntax, *file, *update_kind))
            }
            Some(RecoveryMirror::Unavailable { reason }) => Err(unavailable(reason)),
            None => Err(unavailable("current recovery mirror is missing")),
        }
    }

    fn recovery_status(&self, uri: &str) -> Result<Value, ServerError> {
        let current = self.document(uri)?;
        let (syntax, _, update_kind) = self.recovery_snapshot(uri)?;
        let declarations: Vec<_> = syntax.declarations().iter().map(|declaration| {
            json!({"kind":declaration.kind.as_str(),
                "name":declaration.name_span.as_ref().map(|span| &syntax.source()[span.clone()]),
                "span":byte_range(&declaration.span),"header_span":byte_range(&declaration.header_span),
                "body_span":declaration.body_span.as_ref().map(byte_range),
                "status":declaration.status.as_str()})
        }).collect();
        let sites: Vec<_> = syntax.sites().iter().map(|site| {
            json!({"kind":site.kind.as_str(),"span":byte_range(&site.span),
                "declaration_index":site.owner,"receiver":site.receiver.as_ref().map(|receiver| {
                    json!({"root":byte_range(&receiver.root),
                        "projections":receiver.projections.iter().map(byte_range).collect::<Vec<_>>()})
                })})
        }).collect();
        Ok(
            json!({"scope":"syntax_only","uri":uri,"version":current.version(),
            "source_blake3":hash_text(Some(syntax.source_hash())),"byte_length":syntax.source().len(),
            "update_kind":update_kind,"declarations":declarations,"sites":sites}),
        )
    }
    fn diagnostics(&self, uri: &str) -> Result<Envelope, ServerError> {
        let document = self.document(uri)?;
        let file = *self
            .files
            .get(uri)
            .ok_or_else(|| ServerError::internal("source mirror missing"))?;
        let analysis = database::guarded_analysis(&self.db, file);
        let mut recovery = None;
        let mut lens_metadata = None;
        let (mut diagnostics, status, kind) = match analysis.checked() {
            Ok(_) => (Vec::new(), "checked", None),
            Err(failure @ AnalysisFailure::Parse { .. }) => match self.recovery_payload(uri) {
                Ok((mut payload, mut metadata)) => {
                    // PoisonedRecovery: one notice per live linear binding of
                    // each damaged function, never a cascade of leak errors.
                    let (poison, lens) = self.poison_payload(uri);
                    payload.0.extend(poison);
                    metadata["lens"] = lens;
                    recovery = Some(metadata);
                    payload
                }
                Err(error) => {
                    recovery = Some(json!({"scope":"unavailable",
                            "reason":error.data.as_ref().and_then(|value| value.get("reason"))
                                .cloned().unwrap_or_else(|| json!(error.message))}));
                    diagnostic_payload(uri, document.index(), failure)?
                }
            },
            Err(failure) => {
                // Over the editor budgets, Lens still builds its own view
                // under its raised profile; its facts stay presentation only.
                if matches!(failure, AnalysisFailure::Resource { .. }) {
                    lens_metadata = Some(self.poison_payload(uri).1);
                }
                diagnostic_payload(uri, document.index(), failure)?
            }
        };
        // Validation covers every supplied span before optional presentation
        // fields are filtered for the client's publication capabilities.
        for diagnostic in &mut diagnostics {
            if let Some(object) = diagnostic.as_object_mut() {
                if !self.diagnostic_related {
                    object.remove("relatedInformation");
                }
                if !self.diagnostic_help_data
                    && object
                        .get("data")
                        .is_some_and(|data| data.get("help").is_some())
                {
                    object.remove("data");
                }
            }
        }
        let mut metadata = json!({"status":status,"kind":kind,
            "source_blake3":hash_text(analysis.source_identity().blake3())});
        if let Some(recovery) = recovery {
            metadata["recovery"] = recovery;
        }
        if let Some(lens) = lens_metadata {
            metadata["lens"] = lens;
        }
        Ok(notification(
            "textDocument/publishDiagnostics",
            json!({"uri":uri,"version":document.version(),
            "diagnostics":diagnostics,"vesper":metadata}),
        ))
    }
    fn recovery_payload(&self, uri: &str) -> Result<(DiagnosticPayload, Value), ServerError> {
        let current = self.document(uri)?;
        let (syntax, file, _) = self.recovery_snapshot(uri)?;
        if syntax.diagnostics().is_empty() {
            // A disagreement between the editor and strict parser cannot make
            // the original parse failure disappear or become checked authority.
            return Err(ServerError::internal("strict and recovery syntax disagree"));
        }
        let scoped = database::recovery_diagnostics(&self.db, file);
        let (semantic, metadata) = match scoped {
            Ok(scoped) => {
                if scoped.source() != current.text() || scoped.source_hash() != syntax.source_hash()
                {
                    return Err(ServerError::internal(
                        "recovery diagnostics source identity mismatch",
                    ));
                }
                let analyzed: Vec<_> = scoped
                    .analyzed_functions()
                    .iter()
                    .map(|coverage| json!({"span":byte_range(&coverage.span),"name":coverage.name}))
                    .collect();
                let unavailable: Vec<_> = scoped.unavailable().iter().map(|region| {
                    json!({"span":byte_range(&region.span),"reason":region.reason.as_str()})
                }).collect();
                (
                    scoped.diagnostics(),
                    json!({"scope":"independent_functions",
                    "analyzed_functions":analyzed,"unavailable":unavailable}),
                )
            }
            Err(error) => (
                &[][..],
                json!({"scope":"unavailable","reason":error.to_string(),
                "analyzed_functions":[],"unavailable":[{"span":{"start":0,"end":current.text().len()},
                    "reason":"scoped_analysis_unavailable"}]}),
            ),
        };
        // Both producers have finite independent budgets. The shared renderer
        // then validates their combined spans, labels, text and output limits.
        let mut diagnostics = syntax.diagnostics().to_vec();
        diagnostics.extend_from_slice(semantic);
        let payload = diagnostic_payload(
            uri,
            current.index(),
            &AnalysisFailure::Parse { diagnostics },
        )?;
        if payload.1 == "analysis_unavailable" {
            return Ok((
                payload,
                json!({"scope":"unavailable","reason":"diagnostic_metadata",
                "analyzed_functions":[],"unavailable":[{"span":{"start":0,"end":current.text().len()},
                    "reason":"diagnostic_metadata"}]}),
            ));
        }
        Ok((payload, metadata))
    }
    fn code_actions(&self, params: &serde_json::Map<String, Value>) -> Result<Value, ServerError> {
        if !self.code_actions_supported {
            return Err(ServerError { code: ANALYSIS_UNAVAILABLE,
                message: "Code actions require literal action and versioned documentChanges client support".into(),
                data: Some(json!({"status":"analysis_unavailable","kind":"client_capability"})) });
        }
        let uri = document_uri(params)?;
        let selection = range(field(params, "range")?)?;
        let context = object(Some(field(params, "context")?))?;
        let (quickfix, fix_all) = match context.get("only") {
            None => (true, false),
            Some(only) => {
                let kinds = only
                    .as_array()
                    .ok_or_else(|| ServerError::params("context.only must be an array"))?;
                if kinds.len() > 16 {
                    return Err(ServerError::params("too many action kind filters"));
                }
                let mut quick = false;
                let mut all = false;
                for kind in kinds {
                    let kind = string(kind, "code action kind")?;
                    if kind.len() > 128 {
                        return Err(ServerError::params("action kind is too long"));
                    }
                    quick |= kind_matches("quickfix", kind);
                    all |= kind_matches("source.fixAll.vesper", kind);
                }
                (quick, all)
            }
        };
        let document = self.document(uri)?;
        // Validate even selections whose kind filter selects no supported action.
        document
            .index()
            .span(selection)
            .map_err(|e| ServerError::params(e.to_string()))?;
        if !quickfix && !fix_all {
            return Ok(Value::Array(vec![]));
        }
        self.snapshot(uri)?;
        let action = actions::ActionDocument::new(
            uri,
            document.version(),
            document.text(),
            document.index(),
        )
        .map_err(action_error)?;
        let mut output = Vec::new();
        for (selected, request) in [
            (quickfix, actions::ActionRequest::QuickFix),
            (fix_all, actions::ActionRequest::FixAllUnusedEffects),
        ] {
            if selected {
                output.extend(
                    actions::code_actions(
                        &action,
                        action.stamp(),
                        selection,
                        request,
                        &self.policy,
                        ConfigProvenance::Default,
                    )
                    .map_err(action_error)?
                    .iter()
                    .map(actions::CodeAction::to_json),
                );
            }
        }
        Ok(Value::Array(output))
    }
}

fn notification(method: &str, params: Value) -> Envelope {
    Envelope::Notification {
        method: method.into(),
        params: Some(params),
    }
}
fn update_label(kind: UpdateKind) -> &'static str {
    match kind {
        UpdateKind::Unchanged => "unchanged",
        UpdateKind::BodyReparse => "body_reparse",
        UpdateKind::FullReparse => "full_reparse",
    }
}
fn open_recovery(db: &Database, text: &str) -> RecoveryMirror {
    match SyntaxDocument::open(text, recovery::Limits::default()) {
        Ok(document) => {
            let file = RecoveryFile::new(db, SyntaxSnapshot::new(Arc::clone(document.snapshot())));
            RecoveryMirror::Ready {
                document,
                file,
                update_kind: "opened",
            }
        }
        Err(error) => RecoveryMirror::Unavailable {
            reason: error.to_string(),
        },
    }
}
fn byte_range(span: &std::ops::Range<usize>) -> Value {
    json!({"start":span.start,"end":span.end})
}
fn object(value: Option<&Value>) -> Result<&serde_json::Map<String, Value>, ServerError> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| ServerError::params("parameters must be an object"))
}
fn field<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a Value, ServerError> {
    object
        .get(name)
        .ok_or_else(|| ServerError::params(format!("missing {name}")))
}
fn string<'a>(value: &'a Value, name: &str) -> Result<&'a str, ServerError> {
    value
        .as_str()
        .ok_or_else(|| ServerError::params(format!("{name} must be a string")))
}
fn document_uri(params: &serde_json::Map<String, Value>) -> Result<&str, ServerError> {
    string(
        field(object(Some(field(params, "textDocument")?))?, "uri")?,
        "uri",
    )
}
fn unsigned(value: &Value, name: &str) -> Result<u32, ServerError> {
    value
        .as_u64()
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| ServerError::params(format!("{name} must be a u32")))
}
fn version(value: &Value) -> Result<i32, ServerError> {
    value
        .as_i64()
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| ServerError::params("version must be an i32"))
}
fn position(value: &Value) -> Result<Position, ServerError> {
    let value = object(Some(value))?;
    Ok(Position {
        line: unsigned(field(value, "line")?, "line")?,
        character: unsigned(field(value, "character")?, "character")?,
    })
}
fn range(value: &Value) -> Result<Range, ServerError> {
    let value = object(Some(value))?;
    Ok(Range {
        start: position(field(value, "start")?)?,
        end: position(field(value, "end")?)?,
    })
}
fn range_json(range: Range) -> Value {
    json!({"start":{"line":range.start.line,"character":range.start.character},
        "end":{"line":range.end.line,"character":range.end.character}})
}
fn failure_kind(failure: &AnalysisFailure) -> &'static str {
    match failure {
        AnalysisFailure::Parse { .. } => "parse",
        AnalysisFailure::Semantic { .. } => "semantic",
        AnalysisFailure::Resource { .. } => "resource",
        AnalysisFailure::Unsupported { .. } => "unsupported",
    }
}
fn kind_matches(offered: &str, requested: &str) -> bool {
    requested.is_empty()
        || offered == requested
        || offered
            .strip_prefix(requested)
            .is_some_and(|tail| tail.starts_with('.'))
}
type DiagnosticPayload = (Vec<Value>, &'static str, Option<&'static str>);
fn diagnostic_payload(
    uri: &str,
    index: &vesper_span::LineIndex,
    failure: &AnalysisFailure,
) -> Result<DiagnosticPayload, ServerError> {
    match failure {
        AnalysisFailure::Resource { reason } | AnalysisFailure::Unsupported { reason } => Ok((
            vec![unavailable_warning(index, failure_kind(failure), reason)?],
            "analysis_unavailable",
            Some(failure_kind(failure)),
        )),
        AnalysisFailure::Parse { .. } | AnalysisFailure::Semantic { .. } => {
            let rendered = vesper_diagnostics::prepare(
                index,
                failure.diagnostics(),
                vesper_diagnostics::Limits::default(),
            )
            .and_then(|prepared| prepared.to_lsp(uri));
            let diagnostics = match rendered {
                Ok(diagnostics) => diagnostics,
                Err(error) => {
                    let kind = if matches!(
                        &error,
                        vesper_diagnostics::Error::Span(
                            vesper_span::Error::ByteOutOfBounds
                                | vesper_span::Error::Utf8Boundary
                                | vesper_span::Error::LineEndingInterior
                                | vesper_span::Error::ReversedRange
                        )
                    ) {
                        "invalid_compiler_span"
                    } else {
                        "diagnostic_metadata"
                    };
                    return Ok((
                        vec![unavailable_warning(
                            index,
                            kind,
                            &format!("Analysis unavailable: compiler diagnostic metadata: {error}"),
                        )?],
                        "analysis_unavailable",
                        Some(kind),
                    ));
                }
            };
            Ok((
                diagnostics,
                if matches!(failure, AnalysisFailure::Parse { .. }) {
                    "parse_error"
                } else {
                    "semantic_error"
                },
                Some(failure_kind(failure)),
            ))
        }
    }
}
fn unavailable_warning(
    index: &vesper_span::LineIndex,
    kind: &str,
    reason: &str,
) -> Result<Value, ServerError> {
    let at = index
        .position(0)
        .map_err(|_| ServerError::internal("index origin is invalid"))?;
    Ok(
        json!({"range":range_json(Range{start:at,end:at}),"severity":2,
        "source":"vesper","code":"VESPER_ANALYSIS_UNAVAILABLE","message":reason,
        "data":{"status":"analysis_unavailable","kind":kind}}),
    )
}
fn hash_text(hash: Option<[u8; 32]>) -> Option<String> {
    hash.map(|hash| blake3::Hash::from_bytes(hash).to_hex().to_string())
}
fn nav_error(error: navigation::Error) -> ServerError {
    match error {
        navigation::Error::InvalidPosition(reason) => ServerError::params(reason),
        navigation::Error::SourceMismatch
        | navigation::Error::InvalidSpan
        | navigation::Error::InvalidLimits => ServerError::internal(error.to_string()),
        navigation::Error::Resource(_) => ServerError {
            code: ANALYSIS_UNAVAILABLE,
            message: error.to_string(),
            data: Some(json!({"status":"analysis_unavailable","kind":"resource"})),
        },
    }
}
fn action_error(error: actions::ActionError) -> ServerError {
    let code = match &error {
        actions::ActionError::InvalidRange(_)
        | actions::ActionError::InvalidDocument(_)
        | actions::ActionError::StaleDocument => -32602,
        actions::ActionError::Invariant(_) | actions::ActionError::InvalidPolicyProvenance(_) => {
            -32603
        }
        actions::ActionError::FormatterUnavailable(_)
        | actions::ActionError::LintUnavailable(_) => ANALYSIS_UNAVAILABLE,
    };
    ServerError {
        code,
        message: error.to_string(),
        data: (code == ANALYSIS_UNAVAILABLE).then(|| json!({"status":"analysis_unavailable"})),
    }
}
fn search_error(error: search::SearchError) -> ServerError {
    match error {
        search::SearchError::Query(reason) => ServerError::params(reason),
        search::SearchError::Unavailable { uri, reason } => ServerError {
            code: ANALYSIS_UNAVAILABLE,
            message: "Signature search is unavailable".into(),
            data: Some(
                json!({"status":"analysis_unavailable","kind":"search","uri":uri,"reason":reason}),
            ),
        },
    }
}

/// Defensive direct-DOM bounds before reading/copying arbitrary client metadata.
/// Serialized requests additionally pass the protocol's pre-DOM duplicate/depth
/// checks. The caller owns any allocations made before this private API.
fn validate_input(method: &str, params: Option<&Value>) -> Result<(), ServerError> {
    if method.is_empty() || method.len() > 256 || method.chars().any(char::is_control) {
        return Err(ServerError::new(-32600, "invalid method label"));
    }
    let mut pending = params
        .into_iter()
        .map(|value| (value, 0usize))
        .collect::<Vec<_>>();
    let mut enqueued = pending.len();
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        if depth > 64 {
            return Err(ServerError::params("parameter depth exceeds 64"));
        }
        let mut charge = |n: usize| -> Result<(), ServerError> {
            bytes = bytes
                .checked_add(n)
                .filter(|n| *n <= 2 * 1024 * 1024)
                .ok_or_else(|| ServerError::params("parameter string bytes exceed 2 MiB"))?;
            Ok(())
        };
        match value {
            Value::String(text) => charge(text.len())?,
            Value::Array(values) => {
                enqueued = enqueued
                    .checked_add(values.len())
                    .filter(|n| *n <= 65_536)
                    .ok_or_else(|| ServerError::params("parameter nodes exceed limit"))?;
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                enqueued = enqueued
                    .checked_add(values.len())
                    .filter(|n| *n <= 65_536)
                    .ok_or_else(|| ServerError::params("parameter nodes exceed limit"))?;
                for (key, value) in values {
                    charge(key.len())?;
                    pending.push((value, depth + 1));
                }
            }
            _ => (),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(hierarchical: bool) -> Session {
        let mut session = Session::new().unwrap();
        session
            .request(
                "initialize",
                Some(json!({
                    "capabilities": {
                        "workspace": {"workspaceEdit": {"documentChanges": true}},
                        "textDocument": {
                            "documentSymbol": {"hierarchicalDocumentSymbolSupport": hierarchical},
                            "codeAction": {"codeActionLiteralSupport": {}}
                        }
                    }
                })),
            )
            .unwrap();
        session
    }
    fn publish(outcome: NotificationOutcome) -> Value {
        let NotificationOutcome::Continue(mut notifications) = outcome else {
            panic!("unexpected exit");
        };
        assert_eq!(notifications.len(), 1);
        let Envelope::Notification { method, params } = notifications.pop().unwrap() else {
            panic!("not a notification");
        };
        assert_eq!(method, "textDocument/publishDiagnostics");
        params.unwrap()
    }
    fn open(session: &mut Session, uri: &str, text: &str) -> Value {
        publish(
            session
                .notification(
                    "textDocument/didOpen",
                    Some(json!({"textDocument":{
            "uri":uri,"version":1,"languageId":"vesper","text":text}})),
                )
                .unwrap(),
        )
    }
    fn change(session: &mut Session, uri: &str, version: i32, text: &str) -> Value {
        publish(
            session
                .notification(
                    "textDocument/didChange",
                    Some(json!({"textDocument":{
            "uri":uri,"version":version},"contentChanges":[{"text":text}]})),
                )
                .unwrap(),
        )
    }
    fn query() -> Value {
        json!({"parameters":[],"result":{"Named":["u64",[]]},"result_max_mode":"Copy",
            "effect_ceiling":{"instances":[],"tail":null}})
    }
    #[test]
    fn macro_preview_rejects_client_authority_and_stale_versions_before_compilation() {
        let mut session = running(false);
        open(&mut session, "untitled:preview", "fn main() -> i64 { 42 }");
        for params in [
            json!({"textDocument":{"uri":"untitled:preview"},"policy":{}}),
            json!({"textDocument":{"uri":"untitled:preview","approved":true}}),
            json!({"textDocument":{"uri":"untitled:preview","source":"forged"}}),
            json!({"textDocument":{"uri":"untitled:preview","version":null}}),
            json!({"textDocument":{"uri":"untitled:preview","version":2147483648_i64}}),
        ] {
            assert_eq!(
                session
                    .request("vesper/macroPreview", Some(params))
                    .unwrap_err()
                    .code,
                -32602
            );
        }
        let stale = session
            .request(
                "vesper/macroPreview",
                Some(json!({
                    "textDocument":{"uri":"untitled:preview","version":0}
                })),
            )
            .unwrap_err();
        assert_eq!(stale.code, -32801);
    }

    #[test]
    fn macro_preview_never_uses_a_stale_source_mirror() {
        let mut session = running(false);
        let uri = "untitled:preview";
        open(&mut session, uri, "fn main() -> i64 { 42 }");
        let file = session.files[uri];
        file.set_text(&mut session.db)
            .to("fn main() -> i64 { 7 }".into());
        let error = session
            .request(
                "vesper/macroPreview",
                Some(json!({"textDocument":{"uri":uri}})),
            )
            .unwrap_err();
        assert_eq!(error.code, ANALYSIS_UNAVAILABLE);
        assert_eq!(error.data.unwrap()["status"], "preview_unavailable");
        assert_eq!(
            session.document(uri).unwrap().text(),
            "fn main() -> i64 { 42 }"
        );
    }
    #[test]
    fn macro_preview_noop_reuses_actual_artifact_with_current_document_version() {
        let mut session = running(false);
        let uri = "untitled:macro";
        let source = include_str!("../../tests/fixtures/source_macros/keep.ves");
        open(&mut session, uri, source);
        let before = database::macro_preview::preview(&session.db, session.files[uri])
            .outcome()
            .unwrap();
        change(&mut session, uri, 2, source);
        let after = database::macro_preview::preview(&session.db, session.files[uri])
            .outcome()
            .unwrap();
        assert!(Arc::ptr_eq(&before, &after));
        let result = session
            .request(
                "vesper/macroPreview",
                Some(json!({"textDocument":{"uri":uri,"version":2}})),
            )
            .unwrap();
        assert_eq!(result["version"], 2);
        assert_eq!(result["status"], "expanded_checked");
        assert_eq!(result["expanded_source"], after.expanded_source());
    }
    #[test]
    fn code_actions_require_negotiated_literal_and_versioned_edit_support() {
        for capabilities in [
            json!({}),
            json!({"workspace":{"workspaceEdit":{"documentChanges":false}},"textDocument":{"codeAction":{"codeActionLiteralSupport":{}}}}),
            json!({"workspace":{"workspaceEdit":{"documentChanges":true}}}),
            json!({"textDocument":{"codeAction":{"codeActionLiteralSupport":{}}}}),
        ] {
            let mut session = Session::new().unwrap();
            let result = session
                .request("initialize", Some(json!({"capabilities":capabilities})))
                .unwrap();
            assert_eq!(result["capabilities"]["codeActionProvider"], false);
            let error = session
                .request("textDocument/codeAction", Some(json!({})))
                .unwrap_err();
            assert_eq!(error.code, ANALYSIS_UNAVAILABLE);
            assert_eq!(error.data.unwrap()["kind"], "client_capability");
        }
        assert!(running(true).code_actions_supported);
    }
    #[test]
    fn lifecycle_and_unknown_request_codes_are_strict() {
        let mut session = Session::new().unwrap();
        assert_eq!(session.request("unknown", None).unwrap_err().code, -32002);
        assert_eq!(
            session.notification("initialized", None).unwrap_err().code,
            -32002
        );
        let result = session.request("initialize", Some(json!({}))).unwrap();
        assert_eq!(result["capabilities"]["positionEncoding"], "utf-16");
        // Lens providers are advertised; proof commands are server-named only.
        assert_eq!(
            result["capabilities"]["executeCommandProvider"]["commands"],
            json!(["vesper.proofAudit"])
        );
        assert_eq!(
            result["capabilities"]["completionProvider"]["triggerCharacters"],
            json!(["."])
        );
        assert!(result["capabilities"]["codeLensProvider"].is_object());
        assert!(result["capabilities"].get("renameProvider").is_none());
        assert_eq!(
            session
                .request("initialize", Some(json!({})))
                .unwrap_err()
                .code,
            -32600
        );
        assert_eq!(session.request("unknown", None).unwrap_err().code, -32601);
        assert_eq!(
            session
                .request("shutdown", Some(json!({"bad":true})))
                .unwrap_err()
                .code,
            -32602
        );
        assert_eq!(
            session.request("shutdown", Some(json!({}))).unwrap(),
            Value::Null
        );
        assert_eq!(session.request("unknown", None).unwrap_err().code, -32600);
        assert!(matches!(
            session.notification("exit", None).unwrap(),
            NotificationOutcome::Exit { clean: true }
        ));
        assert!(matches!(
            Session::new().unwrap().notification("exit", None).unwrap(),
            NotificationOutcome::Exit { clean: false }
        ));
    }
    #[test]
    fn rejected_transactions_preserve_store_source_mirror_and_version() {
        let mut session = running(true);
        let source = "//😀\nfn value()->u64{1}";
        open(&mut session, "opaque:a", source);
        let original_hash = session.snapshot("opaque:a").unwrap().source_hash();
        let error = session.notification("textDocument/didChange", Some(json!({"textDocument":{"uri":"opaque:a","version":2},
            "contentChanges":[{"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":3}},"text":"x"}]}))).unwrap_err();
        assert_eq!(error.code, -32602);
        assert_eq!(session.document("opaque:a").unwrap().version(), 1);
        assert_eq!(
            session.snapshot("opaque:a").unwrap().source_hash(),
            original_hash
        );
        assert_eq!(session.accepted_revisions, 1);
        let rejected = session.notification("textDocument/didChange", Some(json!({"textDocument":{"uri":"opaque:a","version":2},
            "contentChanges":[{"text":"fn changed(){}"},{"range":{"start":{"line":99,"character":0},"end":{"line":99,"character":0}},"text":"bad"}]})));
        assert!(rejected.is_err());
        assert_eq!(session.document("opaque:a").unwrap().text(), source);
        assert_eq!(session.files["opaque:a"].text(&session.db), source);
        assert_eq!(
            change(&mut session, "opaque:a", 2, "fn value()->u64{2}")["version"],
            2
        );
        assert!(
            session
                .notification(
                    "textDocument/didChange",
                    Some(
                        json!({"textDocument":{"uri":"opaque:a","version":2},"contentChanges":[]})
                    )
                )
                .is_err()
        );
        assert_eq!(session.document("opaque:a").unwrap().version(), 2);
    }
    #[test]
    fn database_rebuilds_after_64_accepted_revisions_and_each_close() {
        let mut session = running(true);
        open(&mut session, "opaque:a", "fn a()->u64{1}");
        open(&mut session, "opaque:b", "//later\nfn b()->u64{2}");
        for version in 2..=63 {
            change(
                &mut session,
                "opaque:a",
                version,
                &format!("fn a()->u64{{{version}}}"),
            );
        }
        assert_eq!(session.accepted_revisions, 0);
        assert_eq!(session.files.len(), 2);
        assert_eq!(session.recovery.len(), 2);
        for document in session.documents.iter() {
            assert_eq!(
                session.files[document.uri()].text(&session.db),
                document.text()
            );
            assert_eq!(
                session.snapshot(document.uri()).unwrap().source(),
                document.text()
            );
            let (syntax, _, update_kind) = session.recovery_snapshot(document.uri()).unwrap();
            assert_eq!(syntax.source(), document.text());
            assert_eq!(update_kind, "opened");
        }
        let cleared = publish(
            session
                .notification(
                    "textDocument/didClose",
                    Some(json!({"textDocument":{"uri":"opaque:a"}})),
                )
                .unwrap(),
        );
        assert_eq!(cleared["diagnostics"], json!([]));
        assert_eq!(session.files.len(), 1);
        assert_eq!(session.recovery.len(), 1);
        assert!(!session.files.contains_key("opaque:a"));
        assert!(!session.recovery.contains_key("opaque:a"));
        assert_eq!(
            session.snapshot("opaque:b").unwrap().source(),
            "//later\nfn b()->u64{2}"
        );
        open(&mut session, "opaque:a", "fn fresh()->bool{true}");
        let snapshot = session.snapshot("opaque:a").unwrap();
        assert!(snapshot.world().functions.contains_key("fresh"));
        assert!(!snapshot.world().functions.contains_key("a"));
    }
    #[test]
    fn failed_recovery_update_discards_old_snapshot_for_accepted_new_source() {
        let mut session = running(true);
        open(&mut session, "opaque:a", "fn a()->u64{1}");
        assert_eq!(
            session.recovery_status("opaque:a").unwrap()["update_kind"],
            "opened"
        );
        // Accepted by the document byte budget, rejected by recovery's smaller
        // token budget. The prior complete tree must not answer this revision.
        let new_source = format!("fn a(){{{}}}", ";".repeat(20_000));
        change(&mut session, "opaque:a", 2, &new_source);
        assert_eq!(session.document("opaque:a").unwrap().text(), new_source);
        assert!(matches!(
            session.recovery["opaque:a"],
            RecoveryMirror::Unavailable { .. }
        ));
        let error = session.recovery_status("opaque:a").unwrap_err();
        assert_eq!(error.code, ANALYSIS_UNAVAILABLE);
        let data = error.data.unwrap();
        assert_eq!(data["version"], 2);
        assert_eq!(
            data["source_blake3"],
            blake3::hash(new_source.as_bytes()).to_hex().as_str()
        );
        change(&mut session, "opaque:a", 3, "fn restored(){}");
        let restored = session.recovery_status("opaque:a").unwrap();
        assert_eq!(restored["version"], 3);
        assert_eq!(restored["declarations"][0]["name"], "restored");
        assert_eq!(restored["update_kind"], "opened");
    }
    #[test]
    fn recovery_requests_reject_stale_mirror_and_non_open_uris() {
        let mut session = running(true);
        open(&mut session, "opaque:a", "fn a(){}");
        open(&mut session, "opaque:b", "fn b(){}");
        let mirror = session.recovery.remove("opaque:a").unwrap();
        session.recovery.insert("opaque:b".into(), mirror);
        assert_eq!(
            session.recovery_status("opaque:b").unwrap_err().code,
            ANALYSIS_UNAVAILABLE
        );
        let error = session
            .request(
                "vesper/recoveryStatus",
                Some(json!({"textDocument":{"uri":"file:///never/read.vsp"}})),
            )
            .unwrap_err();
        assert_eq!(error.code, -32602);
    }
    #[test]
    fn unsupported_and_invalid_spans_publish_explicit_unavailable_warnings() {
        let mut session = running(true);
        let unavailable = open(&mut session, "opaque:a", "fn main(){let x=Some(0);}");
        assert_eq!(unavailable["vesper"]["status"], "analysis_unavailable");
        assert_eq!(unavailable["diagnostics"][0]["severity"], 2);
        assert_eq!(
            unavailable["diagnostics"][0]["code"],
            "VESPER_ANALYSIS_UNAVAILABLE"
        );
        let semantic = change(&mut session, "opaque:a", 2, "fn main(){unknown}");
        assert_eq!(semantic["vesper"]["status"], "semantic_error");
        assert_eq!(semantic["diagnostics"][0]["severity"], 1);
        let checked = change(&mut session, "opaque:a", 3, "fn main(){}");
        assert_eq!(checked["vesper"]["status"], "checked");
        assert_eq!(checked["diagnostics"], json!([]));
        let index =
            vesper_span::LineIndex::new(Arc::from("😀\r\n"), vesper_span::Limits::default())
                .unwrap();
        for span in [1..2, 5..6, 99..100] {
            for related in [false, true] {
                let invalid = if related {
                    crate::ast::Diagnostic::new("test", "valid primary", 0..0)
                        .with_related(span.clone(), "invalid secondary location")
                } else {
                    crate::ast::Diagnostic::new("test", "invalid location", span.clone())
                };
                let failure = AnalysisFailure::Semantic {
                    diagnostics: vec![
                        crate::ast::Diagnostic::new("test", "valid prefix", 0..0),
                        invalid,
                    ],
                };
                let (diagnostics, status, kind) =
                    diagnostic_payload("opaque:test", &index, &failure).unwrap();
                assert_eq!(status, "analysis_unavailable");
                assert_eq!(kind, Some("invalid_compiler_span"));
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0]["severity"], 2);
            }
        }
    }
    #[test]
    fn symbols_negotiate_hierarchical_support_and_use_only_open_buffers() {
        for hierarchical in [false, true] {
            let mut session = running(hierarchical);
            open(
                &mut session,
                "file:///no-ambient-read.ves",
                "fn answer()->u64{42}",
            );
            let result = session
                .request(
                    "textDocument/documentSymbol",
                    Some(json!({"textDocument":{"uri":"file:///no-ambient-read.ves"}})),
                )
                .unwrap();
            assert_eq!(result[0]["name"], "answer");
            assert_eq!(result[0].get("selectionRange").is_some(), hierarchical);
            assert_eq!(result[0].get("location").is_some(), !hierarchical);
            assert_eq!(
                session
                    .request(
                        "textDocument/documentSymbol",
                        Some(json!({"textDocument":{"uri":"file:///closed.ves"}}))
                    )
                    .unwrap_err()
                    .code,
                -32602
            );
            let workspace = session
                .request("workspace/symbol", Some(json!({"query":"answer"})))
                .unwrap();
            assert_eq!(workspace.as_array().unwrap().len(), 1);
        }
    }
    #[test]
    fn signature_search_reports_source_specific_unavailability() {
        let mut session = running(true);
        open(&mut session, "opaque:valid", "fn answer()->u64{42}");
        open(&mut session, "opaque:invalid", "fn main(){missing}");
        let error = session
            .request("vesper/signatureSearch", Some(json!({"query":query()})))
            .unwrap_err();
        assert_eq!(error.code, ANALYSIS_UNAVAILABLE);
        assert_eq!(error.data.unwrap()["uri"], "opaque:invalid");
        let result = session
            .request(
                "vesper/signatureSearch",
                Some(json!({"query":query(),"uri":"opaque:valid"})),
            )
            .unwrap();
        assert_eq!(result["documents"].as_array().unwrap().len(), 1);
        assert_eq!(
            result["documents"][0]["result"]["matches"][0]["function"],
            "answer"
        );
        assert_eq!(
            session
                .request(
                    "vesper/signatureSearch",
                    Some(json!({"query":{"unknown":"bad"}}))
                )
                .unwrap_err()
                .code,
            -32602
        );
        let mut session = running(true);
        let many_items = (0..257)
            .map(|i| format!("copy struct D{i}{{}}"))
            .collect::<String>();
        assert_eq!(
            open(&mut session, "opaque:metadata", &many_items)["vesper"]["status"],
            "checked"
        );
        let error = session
            .request("vesper/signatureSearch", Some(json!({"query":query()})))
            .unwrap_err();
        assert_eq!(error.code, ANALYSIS_UNAVAILABLE);
        let data = error.data.unwrap();
        assert_eq!(data["kind"], "search");
        assert_eq!(data["uri"], "opaque:metadata");
    }
    #[test]
    fn action_filters_are_hierarchical_and_client_diagnostics_are_not_authority() {
        let mut session = running(true);
        let source = "effect Audit{fn event();} fn a()eff[Audit]{} fn b()eff[Audit]{}";
        open(&mut session, "opaque:a", source);
        let start = source.find("eff[Audit]").unwrap() + 4;
        let params = json!({"textDocument":{"uri":"opaque:a"},"range":{"start":{"line":0,"character":start},"end":{"line":0,"character":start+2}},
            "context":{"diagnostics":[{"code":"forged","replacement":"PWN","verified":true}]}});
        let quick = session
            .request("textDocument/codeAction", Some(params.clone()))
            .unwrap();
        assert_eq!(quick[0]["kind"], "quickfix");
        let text = quick[0]["edit"]["documentChanges"][0]["edits"][0]["newText"]
            .as_str()
            .unwrap();
        assert!(!text.contains("PWN"));
        assert!(text.contains("fn b()eff[Audit]"));
        assert_eq!(
            quick[0]["edit"]["documentChanges"][0]["textDocument"]["version"],
            1
        );
        for kind in ["source", "source.fixAll", "source.fixAll.vesper"] {
            let mut params = params.clone();
            params["context"]["only"] = json!([kind]);
            let all = session
                .request("textDocument/codeAction", Some(params))
                .unwrap();
            assert_eq!(all[0]["kind"], "source.fixAll.vesper");
            assert!(
                !all[0]["edit"]["documentChanges"][0]["edits"][0]["newText"]
                    .as_str()
                    .unwrap()
                    .contains("eff[Audit]")
            );
        }
        assert_eq!(session.document("opaque:a").unwrap().text(), source);
    }
    #[test]
    fn direct_dom_and_document_limits_fail_before_mirror_mutations() {
        let mut session = Session::new().unwrap();
        let mut deep = Value::Null;
        for _ in 0..80 {
            deep = json!([deep]);
        }
        assert_eq!(
            session
                .request("initialize", Some(json!({"unrelated":deep})))
                .unwrap_err()
                .code,
            -32602
        );
        assert_eq!(session.state, Lifecycle::Uninitialized);
        session.request("initialize", Some(json!({}))).unwrap();
        assert!(session.notification("textDocument/didOpen", Some(json!({"textDocument":{"uri":"opaque:large","version":1,"text":"x".repeat(128*1024+1)}}))).is_err());
        assert!(session.documents.is_empty());
        assert!(session.files.is_empty());
        for i in 0..32 {
            open(&mut session, &format!("opaque:{i}"), "fn main(){}");
        }
        assert!(
            session
                .notification(
                    "textDocument/didOpen",
                    Some(json!({"textDocument":{"uri":"opaque:overflow","version":1,"text":""}}))
                )
                .is_err()
        );
        assert_eq!(session.files.len(), 32);
        assert!(!session.files.contains_key("opaque:overflow"));
    }
}
