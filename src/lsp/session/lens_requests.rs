//! Lens requests on the session: poison notices, obligation-guided completion,
//! obligation hovers and inlay hints, and proof code lenses.
use super::*;
use crate::{
    lsp::{
        lens::{Binding, Unavailable, View},
        proof_lens, proof_model,
    },
    tooling::frontend::AnalysisLimits,
};
use std::time::{Duration, Instant};
use vesper_lsp::lens::{CancelToken, CompletionTier, Debouncer, ProofStatus, TimerAction};

pub(crate) const PROOF_COMMAND: &str = "vesper.proofAudit";

/// Server-side Lens configuration. Clients cannot choose executables or
/// directories; they may only switch debounced background runs on or off.
#[derive(Clone, Debug)]
pub struct LensConfig {
    /// Lean launcher (`VESPER_LEAN`, else `lean` on PATH).
    pub lean: String,
    /// The `vesper` executable whose `proof-audit` command replays proofs.
    /// Defaults to the running executable when it is named `vesper`.
    pub auditor: Option<std::path::PathBuf>,
    /// Seconds allowed for each Lean process (1 to 300).
    pub proof_timeout: u64,
    /// Private `.olean` cache reused across runs (`VESPER_LENS_CACHE`, else
    /// `$XDG_CACHE_HOME/vesper/lens-proofs` or `~/.cache/vesper/lens-proofs`).
    /// `None` compiles every run from scratch.
    pub cache: Option<std::path::PathBuf>,
    /// Debounce for background proof runs after an edit
    /// (`VESPER_LENS_BACKGROUND_MS`). `None` leaves them off unless the
    /// client asks for them in `initializationOptions`.
    pub background: Option<Duration>,
    /// Analysis budgets for Lens's own views (repaired prefixes, obligation
    /// facts). They are raised above the editor profile so larger buffers get
    /// Lens views; the editor profile that guards diagnostics, navigation and
    /// code actions is unchanged.
    pub limits: AnalysisLimits,
}
/// The spec's Layer 5 idle time before a background proof run (§21.3).
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(150);

/// Lens's own analysis profile: the editor profile with larger node and
/// type budgets (still inside the frontend's hard caps).
pub fn lens_limits() -> AnalysisLimits {
    AnalysisLimits {
        max_ast_nodes: 8192,
        max_type_nodes: 16_384,
        max_depth: 64,
        max_diagnostics: 256,
        max_diagnostic_bytes: 262_144,
        max_metadata_bytes: 2_097_152,
        ..AnalysisLimits::default()
    }
}

fn default_cache() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("VESPER_LENS_CACHE") {
        return (!dir.is_empty()).then(|| dir.into());
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".cache"))
        })?;
    Some(base.join("vesper").join("lens-proofs"))
}

impl Default for LensConfig {
    fn default() -> Self {
        let auditor = std::env::current_exe()
            .ok()
            .filter(|exe| exe.file_stem().is_some_and(|stem| stem == "vesper"));
        Self {
            lean: std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into()),
            auditor,
            proof_timeout: 60,
            cache: default_cache(),
            background: std::env::var("VESPER_LENS_BACKGROUND_MS")
                .ok()
                .and_then(|ms| ms.parse::<u64>().ok())
                .map(Duration::from_millis),
            limits: lens_limits(),
        }
    }
}

/// What a fired debounce timer asks the server to do.
pub(crate) enum TimerOutcome {
    Rearm(Duration),
    Start {
        generation: u64,
        job: Box<proof_lens::Job>,
        token: CancelToken,
    },
    Idle,
}

pub(crate) enum CommandOutcome {
    Done(Value),
    Run(proof_lens::Job),
}

fn lens_unavailable(uri: &str, reason: &str) -> ServerError {
    ServerError {
        code: ANALYSIS_UNAVAILABLE,
        message: "Lens analysis is unavailable".into(),
        data: Some(
            json!({"status":"analysis_unavailable","kind":"lens","reason":reason,"uri":uri}),
        ),
    }
}

fn ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// `receiver.partial|` immediately before `offset`: the receiver's span.
fn member_receiver(text: &str, offset: usize) -> Option<std::ops::Range<usize>> {
    let bytes = text.as_bytes();
    let mut i = offset;
    while i > 0 && ident(bytes[i - 1]) {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'.' {
        return None;
    }
    let end = i - 1;
    let mut start = end;
    while start > 0 && ident(bytes[start - 1]) {
        start -= 1;
    }
    let first = *bytes.get(start)?;
    if start == end || !(first.is_ascii_lowercase() || first == b'_') {
        return None;
    }
    // Projected receivers (`a.b.`) are outside the supported slice.
    if start > 0 && bytes[start - 1] == b'.' {
        return None;
    }
    Some(start..end)
}

impl Session {
    pub(super) fn set_lens_config(&mut self, config: LensConfig) {
        self.lens_config = config;
    }

    /// The Lens view of the current text, cached by exact source hash.
    pub(super) fn lens_view(&self, uri: &str) -> Result<Arc<View>, ServerError> {
        let document = self.document(uri)?;
        let hash = *blake3::hash(document.text().as_bytes()).as_bytes();
        if let Some((cached, view)) = self.lens_views.borrow().get(uri)
            && *cached == hash
        {
            return view
                .clone()
                .map_err(|reason| lens_unavailable(uri, reason.as_str()));
        }
        let file = *self
            .files
            .get(uri)
            .ok_or_else(|| ServerError::internal("open source mirror is missing"))?;
        let analysis = database::guarded_analysis(&self.db, file);
        let view = match analysis.checked_arc() {
            Ok(snapshot) => View::checked(snapshot),
            Err(AnalysisFailure::Parse { .. }) => match self.recovery_snapshot(uri) {
                Ok((syntax, _, _)) => View::recovered(syntax, self.lens_config.limits),
                Err(error) => return Err(error),
            },
            // Semantic errors, and buffers over the editor profile's budgets,
            // get a view under Lens's own (raised) profile.
            Err(AnalysisFailure::Semantic { .. } | AnalysisFailure::Resource { .. }) => {
                View::semantic(document.text(), self.lens_config.limits)
            }
            Err(_) => Err(Unavailable::Guard),
        }
        .map(Arc::new);
        if view.as_ref().err() == Some(&Unavailable::Cancelled) {
            // Never cache a traversal the client abandoned.
            return Err(ServerError::new(-32800, "Request cancelled"));
        }
        self.lens_views
            .borrow_mut()
            .insert(uri.to_owned(), (hash, view.clone()));
        view.map_err(|reason| lens_unavailable(uri, reason.as_str()))
    }

    /// One informational notice per poisoned linear binding, and metadata.
    pub(super) fn poison_payload(&self, uri: &str) -> (Vec<Value>, Value) {
        let Ok(document) = self.document(uri) else {
            return (Vec::new(), json!({"status":"unavailable"}));
        };
        let view = match self.lens_view(uri) {
            Ok(view) => view,
            Err(error) => {
                let reason = error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("reason"))
                    .cloned()
                    .unwrap_or_else(|| json!(error.message));
                return (Vec::new(), json!({"status":"unavailable","reason":reason}));
            }
        };
        let index = document.index();
        let range_of = |span: &std::ops::Range<usize>| {
            index
                .range(vesper_span::ByteSpan {
                    start: span.start,
                    end: span.end,
                })
                .ok()
                .map(range_json)
        };
        let mut diagnostics = Vec::new();
        let mut poisoned = Vec::new();
        for (region, bindings) in view.poisoned() {
            let Some(damage) = range_of(&region.damage) else {
                continue;
            };
            let line = index
                .position(region.damage.start)
                .map(|p| p.line + 1)
                .unwrap_or(0);
            for binding in bindings {
                let Some(origin) = range_of(&binding.origin) else {
                    continue;
                };
                diagnostics.push(json!({
                    "range": origin,
                    "severity": 3,
                    "source": "vesper",
                    "code": "VESPER_POISONED_RECOVERY",
                    "message": format!(
                        "linear binding '{}' is poisoned by unfinished code on line {line}; \
                         its scope-exit leak checks are suppressed until '{}' parses again",
                        binding.name, region.name
                    ),
                    "relatedInformation": [{
                        "location": {"uri": uri, "range": damage},
                        "message": "unfinished code (PoisonedRecovery region)"
                    }],
                    "data": {"status": "poisoned_recovery", "binding": binding.name,
                        "type": binding.ty.to_string(), "function": region.name}
                }));
                poisoned.push(json!({"binding": binding.name, "function": region.name,
                    "origin": byte_range(&binding.origin), "region": byte_range(&region.damage),
                    "cut": region.cut}));
            }
        }
        (
            diagnostics,
            json!({"status":"available","scope":view.scope(),"poisoned":poisoned,
                "absorbed_scope_exit_diagnostics":view.absorbed(),
                "regions":view.regions().iter().map(|r| json!({"function":r.name,
                    "kind":r.kind.as_str(),"depth":r.depth,
                    "span":byte_range(&r.span),"scope":byte_range(&r.scope),
                    "cut":r.cut})).collect::<Vec<_>>()}),
        )
    }

    pub(super) fn completion(
        &self,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value, ServerError> {
        let uri = document_uri(params)?;
        let at = position(field(params, "position")?)?;
        let document = self.document(uri)?;
        let offset = document
            .index()
            .offset(at)
            .map_err(|e| ServerError::params(e.to_string()))?;
        let empty = json!({"isIncomplete": false, "items": []});
        let Some(receiver) = member_receiver(document.text(), offset) else {
            return Ok(empty);
        };
        let view = self.lens_view(uri)?;
        let name = &document.text()[receiver.clone()];
        let Some((binding, ranked)) = view.complete_member(name, receiver.start) else {
            return Ok(empty);
        };
        let live = view
            .outstanding(receiver.start)
            .iter()
            .any(|b| b.origin == binding.origin);
        let items: Vec<Value> = ranked
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let mut value = json!({
                    "label": item.label,
                    "kind": 2,
                    "detail": item.detail,
                    "sortText": format!("{}{i:04}", item.tier as u8),
                    "data": {"vesper": {"tier": item.tier.as_str(), "receiver": name,
                        "receiver_type": binding.ty.to_string(),
                        "receiver_live_linear": live, "scope": view.scope()}}
                });
                if let Some(badge) = &item.badge {
                    value["labelDetails"] = json!({"description": badge});
                }
                if item.tier == CompletionTier::BlockedByFrame {
                    value["tags"] = json!([1]);
                }
                value
            })
            .collect();
        Ok(json!({"isIncomplete": false, "items": items}))
    }

    /// Consuming calls for a binding's type, rendered for hover text.
    fn consumers(view: &View, binding: &Binding) -> Vec<String> {
        let ty = binding.ty.name();
        let mut out = Vec::new();
        for (name, f) in &view.world().functions {
            if let Some(method) = name.strip_prefix(ty).and_then(|n| n.strip_prefix("::")) {
                if f.params
                    .first()
                    .is_some_and(|p| p.name == "self" && p.pass == crate::ast::Pass::Sink)
                {
                    out.push(format!("{}.{method}()", binding.name));
                }
            } else if !name.contains("::")
                && f.params
                    .iter()
                    .any(|p| p.pass == crate::ast::Pass::Sink && p.ty.name() == ty)
            {
                out.push(format!("{name}(..)"));
            }
            if out.len() >= 8 {
                break;
            }
        }
        out
    }

    fn obligation_text(&self, uri: &str, offset: usize) -> Option<String> {
        let view = self.lens_view(uri).ok()?;
        let index = self.document(uri).ok()?.index();
        let live = view.outstanding(offset);
        if live.is_empty() {
            return None;
        }
        let mut text = format!("outstanding linear obligations here ({}):", view.scope());
        for binding in live {
            let line = index
                .position(binding.origin.start)
                .map(|p| p.line + 1)
                .unwrap_or(0);
            let consumers = Self::consumers(&view, binding);
            text.push_str(&format!(
                "\n- {}: {} (introduced on line {line}){}",
                binding.name,
                binding.ty,
                if consumers.is_empty() {
                    String::new()
                } else {
                    format!("; discharge with {}", consumers.join(", "))
                }
            ));
        }
        Some(text)
    }

    /// Append live obligations to the navigation hover. When checked
    /// navigation is unavailable, its error stands and carries them as data.
    pub(super) fn hover_with_obligations(
        &self,
        uri: &str,
        at: Position,
        base: Result<Value, ServerError>,
    ) -> Result<Value, ServerError> {
        // Navigation's own answer, including its error precedence, stands
        // unless there are live obligations to add.
        let Ok(document) = self.document(uri) else {
            return base;
        };
        let Ok(offset) = document.index().offset(at) else {
            return base;
        };
        let Some(obligations) = self.obligation_text(uri, offset) else {
            return base;
        };
        match base {
            Ok(Value::Object(mut hover)) => {
                if let Some(value) = hover
                    .get_mut("contents")
                    .and_then(|c| c.get_mut("value"))
                    .and_then(|v| v.as_str().map(str::to_owned))
                {
                    hover["contents"]["value"] = json!(format!("{value}\n\n{obligations}"));
                }
                Ok(Value::Object(hover))
            }
            Ok(_) => Ok(json!({"contents":{"kind":"plaintext","value":obligations}})),
            // Damaged or ill-typed buffers keep navigation's typed refusal;
            // the obligations ride along in its data for clients that look.
            Err(mut error) if error.code == ANALYSIS_UNAVAILABLE => {
                match error.data.as_mut() {
                    Some(Value::Object(data)) => {
                        data.insert("lens_obligations".into(), json!(obligations));
                    }
                    _ => error.data = Some(json!({"lens_obligations": obligations})),
                }
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn inlay_hints(
        &self,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value, ServerError> {
        let uri = document_uri(params)?;
        let selection = range(field(params, "range")?)?;
        let document = self.document(uri)?;
        let index = document.index();
        let start = index
            .offset(selection.start)
            .map_err(|e| ServerError::params(e.to_string()))?;
        // Editors commonly request hints through a range past the last line.
        let end = index.offset(selection.end).unwrap_or(document.text().len());
        let view = self.lens_view(uri)?;
        let mut hints = Vec::new();
        let poisoned: Vec<_> = view
            .poisoned()
            .into_iter()
            .flat_map(|(_, bindings)| bindings.into_iter().map(|b| b.origin.clone()))
            .collect();
        for binding in view.linear_bindings() {
            if binding.origin.start < start || binding.origin.end > end {
                continue;
            }
            let label = if poisoned.contains(&binding.origin) {
                "linear: poisoned by unfinished code".to_owned()
            } else if let Some(span) = binding.first_consumption_after(binding.origin.end) {
                let line = index.position(span.start).map(|p| p.line + 1).unwrap_or(0);
                format!("linear: consumed on line {line}")
            } else {
                "linear: outstanding".to_owned()
            };
            let at = index
                .position(binding.origin.end)
                .map_err(|_| ServerError::internal("binding span is invalid"))?;
            hints.push(json!({"position":{"line":at.line,"character":at.character},
                "label":label,"paddingLeft":true,
                "data":{"vesper":{"binding":binding.name,"type":binding.ty.to_string(),
                    "scope":view.scope()}}}));
            if hints.len() >= 512 {
                break;
            }
        }
        Ok(Value::Array(hints))
    }

    fn toolchain(&self) -> proof_lens::Toolchain {
        proof_lens::Toolchain {
            launcher: self.lens_config.lean.clone(),
            auditor: self.lens_config.auditor.clone(),
            timeout: self.lens_config.proof_timeout,
            cache: self.lens_config.cache.clone(),
        }
    }

    /// Semantic models of every function of the checked source, cached by
    /// exact source hash.
    fn models(
        &self,
        uri: &str,
        snapshot: &CheckedSource,
    ) -> Arc<BTreeMap<String, Result<proof_model::Model, String>>> {
        let hash = snapshot.source_hash();
        if let Some((cached, models)) = self.lens_models.borrow().get(uri)
            && *cached == hash
        {
            return models.clone();
        }
        let models = Arc::new(proof_model::all(snapshot.checked()));
        self.lens_models
            .borrow_mut()
            .insert(uri.to_owned(), (hash, models.clone()));
        models
    }

    fn semantic_status(&self, model: &Result<proof_model::Model, String>) -> ProofStatus {
        match model {
            Ok(model) => self.model_proofs.status(model.key, &model.function),
            Err(reason) => ProofStatus::Failed {
                stage: "model".into(),
                reason: reason.clone(),
            },
        }
    }

    pub(super) fn code_lenses(
        &self,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value, ServerError> {
        let uri = document_uri(params)?;
        let document = self.document(uri)?;
        // Proofs need checked source. Damaged or ill-typed buffers get none.
        let Ok(snapshot) = self.snapshot(uri) else {
            return Ok(Value::Array(Vec::new()));
        };
        let functions = match proof_lens::candidates(&snapshot) {
            Ok((_, functions)) => functions,
            Err(_) => {
                let mut all: Vec<_> = snapshot
                    .world()
                    .functions
                    .values()
                    .filter(|f| f.body.is_some())
                    .map(|f| (f.name.clone(), f.span.clone()))
                    .collect();
                all.sort_by_key(|(_, span)| span.start);
                all
            }
        };
        let models = self.models(uri, &snapshot);
        let hash = snapshot.source_hash();
        let mut lenses = Vec::new();
        for (name, span) in functions.iter().take(256) {
            let status = self.proofs.status(hash, name);
            let title = match &status {
                ProofStatus::NotAttempted => "Lean: not attempted (run proof audit)".to_owned(),
                ProofStatus::Proved { .. } => {
                    "Lean: proved (extraction kernel-checked, no axioms)".to_owned()
                }
                ProofStatus::Failed { stage, .. } => format!("Lean: failed at {stage} (rerun)"),
                ProofStatus::Checked { .. } | ProofStatus::Refuted { .. } => {
                    format!("Lean: {}", status.as_str())
                }
            };
            let at = document
                .index()
                .position(span.start)
                .map_err(|_| ServerError::internal("function span is invalid"))?;
            let position = json!({"line":at.line,"character":at.character});
            lenses.push(json!({
                "range": {"start": position, "end": position},
                "command": {"title": title, "command": PROOF_COMMAND,
                    "arguments": [{"uri": uri, "version": document.version(), "function": name}]},
                "data": {"vesper": {"status": status.as_str(), "function": name,
                    "scope": proof_lens::EXTRACTION_SCOPE}}
            }));
            let Some(model) = models.get(name) else {
                continue;
            };
            let status = self.semantic_status(model);
            let title = match (&status, model) {
                (_, Err(reason)) => format!(
                    "Lean model: unavailable ({})",
                    reason.chars().take(80).collect::<String>()
                ),
                (ProofStatus::NotAttempted, _) => {
                    "Lean model: not attempted (check arithmetic faults)".to_owned()
                }
                (ProofStatus::Proved { .. }, _) => {
                    "Lean model: no arithmetic fault (proved, kernel-replayed)".to_owned()
                }
                (ProofStatus::Checked { .. }, _) => {
                    "Lean model: no counterexample (bv_decide, not kernel-replayed)".to_owned()
                }
                (ProofStatus::Refuted { counterexample }, _) => {
                    format!("Lean model: arithmetic fault when {counterexample}")
                }
                (ProofStatus::Failed { stage, .. }, _) => {
                    format!("Lean model: failed at {stage} (rerun)")
                }
            };
            let mut data = json!({"status": status.as_str(), "function": name,
                "scope": proof_lens::SEMANTIC_SCOPE});
            match (&status, model) {
                (_, Err(reason)) => {
                    data["status"] = json!("unavailable");
                    data["reason"] = json!(reason);
                }
                (ProofStatus::Refuted { counterexample }, Ok(model)) => {
                    data["counterexample"] = json!(counterexample);
                    data["model_blake3"] = json!(proof_model::hex(&model.key));
                }
                (_, Ok(model)) => data["model_blake3"] = json!(proof_model::hex(&model.key)),
            }
            lenses.push(json!({
                "range": {"start": position, "end": position},
                "command": {"title": title, "command": PROOF_COMMAND,
                    "arguments": [{"uri": uri, "version": document.version(), "function": name,
                        "obligation": "semantic"}]},
                "data": {"vesper": data}
            }));
        }
        Ok(Value::Array(lenses))
    }

    /// Validate `workspace/executeCommand` and prepare the proof job.
    pub(crate) fn execute_command(
        &mut self,
        params: Option<Value>,
    ) -> Result<CommandOutcome, ServerError> {
        self.running()?;
        validate_input("workspace/executeCommand", params.as_ref())?;
        let params = object(params.as_ref())?;
        if string(field(params, "command")?, "command")? != PROOF_COMMAND {
            return Err(ServerError::params("unknown command"));
        }
        let arguments = field(params, "arguments")?
            .as_array()
            .filter(|args| args.len() == 1)
            .ok_or_else(|| ServerError::params("vesper.proofAudit takes one argument object"))?;
        let argument = object(arguments.first())?;
        if argument
            .keys()
            .any(|k| !matches!(k.as_str(), "uri" | "version" | "function" | "obligation"))
        {
            return Err(ServerError::params("unknown vesper.proofAudit argument"));
        }
        let uri = string(field(argument, "uri")?, "uri")?;
        let function = string(field(argument, "function")?, "function")?;
        if function.len() > 1024 {
            return Err(ServerError::params("function name is too long"));
        }
        let semantic = match argument.get("obligation").map(|v| string(v, "obligation")) {
            None => false,
            Some(Ok("extraction")) => false,
            Some(Ok("semantic")) => true,
            Some(Ok(_)) => {
                return Err(ServerError::params(
                    "obligation must be \"extraction\" or \"semantic\"",
                ));
            }
            Some(Err(error)) => return Err(error),
        };
        let document = self.document(uri)?;
        if let Some(expected) = argument.get("version").map(version).transpose()?
            && expected != document.version()
        {
            return Err(ServerError::new(-32801, "Document version has changed"));
        }
        let snapshot = self.snapshot(uri)?;
        let fail = |stage: &str, reason: String| ProofStatus::Failed {
            stage: stage.into(),
            reason,
        };
        if semantic {
            let models = self.models(uri, &snapshot);
            let model = match models.get(function) {
                Some(Ok(model)) => model.clone(),
                Some(Err(reason)) => {
                    let status = fail("model", reason.clone());
                    return Ok(CommandOutcome::Done(semantic_value(
                        function, None, &status,
                    )));
                }
                None => {
                    let status = fail("model", format!("function '{function}' is not checked"));
                    return Ok(CommandOutcome::Done(semantic_value(
                        function, None, &status,
                    )));
                }
            };
            let mut job = proof_lens::prepare(&snapshot, &[], vec![model], &self.toolchain())
                .map_err(ServerError::internal)?;
            job.requested = Some((function.to_owned(), true));
            return Ok(CommandOutcome::Run(job));
        }
        match proof_lens::prepare(
            &snapshot,
            &[function.to_owned()],
            Vec::new(),
            &self.toolchain(),
        ) {
            Ok(mut job) => {
                job.requested = Some((function.to_owned(), false));
                Ok(CommandOutcome::Run(job))
            }
            Err(reason) => {
                let status = fail("extract", reason);
                let value = proof_value(snapshot.source_hash(), function, &status);
                self.proofs.record(snapshot.source_hash(), function, status);
                Ok(CommandOutcome::Done(value))
            }
        }
    }

    fn record(&mut self, job: &proof_lens::Job, outcome: &proof_lens::Outcome) {
        for (function, status) in &outcome.extraction {
            self.proofs.record(job.source, function, status.clone());
        }
        for (function, key, status) in &outcome.semantic {
            // Toolchain or I/O failures say nothing about the model.
            if matches!(status, ProofStatus::Failed { stage, .. } if stage != "prove" && stage != "kernel")
            {
                continue;
            }
            self.model_proofs.record(*key, function, status.clone());
        }
    }

    /// Record a finished proof job and render its command result.
    pub(crate) fn finish_proof(
        &mut self,
        job: &proof_lens::Job,
        outcome: proof_lens::Outcome,
    ) -> Value {
        self.record(job, &outcome);
        let Some((function, semantic)) = &job.requested else {
            return json!(null);
        };
        let mut value = if *semantic {
            let found = outcome
                .semantic
                .iter()
                .find(|(f, _, _)| f == function)
                .map(|(_, key, status)| (key, status.clone()));
            let (key, status) = match found {
                Some((key, status)) => (Some(key), status),
                None => (None, ProofStatus::NotAttempted),
            };
            semantic_value(function, key, &status)
        } else {
            let status = outcome
                .extraction
                .iter()
                .find(|(f, _)| f == function)
                .map(|(_, status)| status.clone())
                .unwrap_or(ProofStatus::NotAttempted);
            proof_value(job.source, function, &status)
        };
        value["olean_reused"] = json!(outcome.reused);
        value["olean_compiled"] = json!(outcome.compiled);
        value["elapsed_ms"] = json!(outcome.elapsed.as_millis() as u64);
        value
    }

    /// Read the client's background-run choice and refresh support.
    pub(super) fn configure_background(&mut self, initialize: &serde_json::Map<String, Value>) {
        self.code_lens_refresh = initialize
            .get("capabilities")
            .and_then(|c| c.pointer("/workspace/codeLens/refreshSupport"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let options = initialize
            .get("initializationOptions")
            .and_then(|o| o.get("vesperLens"));
        let requested = options
            .and_then(|o| o.get("backgroundProofs"))
            .and_then(Value::as_bool);
        let debounce = options
            .and_then(|o| o.get("debounceMs"))
            .and_then(Value::as_u64)
            .filter(|ms| *ms <= 60_000)
            .map(Duration::from_millis);
        let delay = match (requested, self.lens_config.background) {
            (Some(false), _) => None,
            (Some(true), configured) => Some(debounce.or(configured).unwrap_or(DEFAULT_DEBOUNCE)),
            (None, configured) => configured.map(|d| debounce.unwrap_or(d)),
        };
        // Background runs need the auditor; without it they stay off.
        self.background = delay
            .filter(|_| self.lens_config.auditor.is_some())
            .and_then(|delay| Debouncer::new(delay, 256).ok());
    }

    /// An accepted edit restarts the debounce for `uri`.
    pub(super) fn touch_background(&mut self, uri: &str) {
        if let Some(background) = &mut self.background
            && let TimerAction::Arm(delay) = background.touch(uri, Instant::now())
        {
            self.timers.push((uri.to_owned(), delay));
        }
    }
    pub(super) fn forget_background(&mut self, uri: &str) {
        if let Some(background) = &mut self.background {
            background.forget(uri);
        }
    }
    pub(crate) fn stop_background(&mut self) {
        if let Some(background) = &mut self.background {
            background.cancel_all();
        }
    }
    pub(crate) fn take_timers(&mut self) -> Vec<(String, Duration)> {
        std::mem::take(&mut self.timers)
    }

    /// A debounce timer fired: start a background run of every obligation
    /// that has no outcome for the current source yet.
    pub(crate) fn timer_fired(&mut self, uri: &str) -> TimerOutcome {
        let Some(background) = &mut self.background else {
            return TimerOutcome::Idle;
        };
        let generation = match background.timer(uri, Instant::now()) {
            TimerAction::Arm(delay) => return TimerOutcome::Rearm(delay),
            TimerAction::Idle => return TimerOutcome::Idle,
            TimerAction::Fire { generation } => generation,
        };
        let Ok(snapshot) = self.snapshot(uri) else {
            return TimerOutcome::Idle;
        };
        let hash = snapshot.source_hash();
        let extraction: Vec<String> = proof_lens::candidates(&snapshot)
            .map(|(_, functions)| functions)
            .unwrap_or_default()
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| self.proofs.status(hash, name) == ProofStatus::NotAttempted)
            .take(64)
            .collect();
        let models: Vec<_> = self
            .models(uri, &snapshot)
            .values()
            .filter_map(|m| m.as_ref().ok())
            .filter(|m| self.model_proofs.status(m.key, &m.function) == ProofStatus::NotAttempted)
            .take(64)
            .cloned()
            .collect();
        let Ok(job) = proof_lens::prepare(&snapshot, &extraction, models, &self.toolchain()) else {
            return TimerOutcome::Idle;
        };
        if job.is_empty() {
            return TimerOutcome::Idle;
        }
        let token = CancelToken::new();
        if let Some(background) = &mut self.background {
            background.started(uri, generation, token.clone());
        }
        TimerOutcome::Start {
            generation,
            job: Box::new(job),
            token,
        }
    }

    /// Record a finished background run and tell the client.
    pub(crate) fn finish_background(
        &mut self,
        uri: &str,
        generation: u64,
        job: &proof_lens::Job,
        outcome: proof_lens::Outcome,
    ) -> Vec<Envelope> {
        if let Some(background) = &mut self.background {
            background.finished(uri, generation);
        }
        self.record(job, &outcome);
        let Ok(document) = self.document(uri) else {
            return Vec::new();
        };
        if *blake3::hash(document.text().as_bytes()).as_bytes() != job.source {
            return Vec::new();
        }
        let mut results: Vec<Value> = outcome
            .extraction
            .iter()
            .map(|(f, status)| {
                let mut v = proof_value(job.source, f, status);
                v["obligation"] = json!("extraction");
                v
            })
            .collect();
        results.extend(outcome.semantic.iter().map(|(f, key, status)| {
            let mut v = semantic_value(f, Some(key), status);
            v["obligation"] = json!("semantic");
            v
        }));
        let mut messages = vec![notification(
            "vesper/proofStatus",
            json!({"uri": uri, "version": document.version(), "generation": generation,
                "source_blake3": blake3::Hash::from_bytes(job.source).to_hex().as_str(),
                "results": results, "olean_reused": outcome.reused,
                "olean_compiled": outcome.compiled,
                "elapsed_ms": outcome.elapsed.as_millis() as u64}),
        )];
        if self.code_lens_refresh {
            self.refreshes += 1;
            messages.push(Envelope::Request {
                id: vesper_lsp::Id::String(format!("vesper-lens-refresh-{}", self.refreshes)),
                method: "workspace/codeLens/refresh".into(),
                params: None,
            });
        }
        messages
    }
}

fn status_fields(value: &mut Value, status: &ProofStatus) {
    match status {
        ProofStatus::Proved { detail } | ProofStatus::Checked { detail } => {
            value["detail"] = json!(detail)
        }
        ProofStatus::Refuted { counterexample } => value["counterexample"] = json!(counterexample),
        ProofStatus::Failed { stage, reason } => {
            value["stage"] = json!(stage);
            value["reason"] = json!(reason);
        }
        ProofStatus::NotAttempted => {}
    }
}

fn proof_value(source: [u8; 32], function: &str, status: &ProofStatus) -> Value {
    let mut value = json!({"function": function, "status": status.as_str(),
        "scope": proof_lens::EXTRACTION_SCOPE,
        "source_blake3": blake3::Hash::from_bytes(source).to_hex().as_str()});
    status_fields(&mut value, status);
    value
}

fn semantic_value(function: &str, key: Option<&[u8; 32]>, status: &ProofStatus) -> Value {
    let mut value = json!({"function": function, "status": status.as_str(),
        "scope": proof_lens::SEMANTIC_SCOPE,
        "theorem": "no arithmetic fault (Fail[ArithOverflow], DivisionByZero) for any input in range, under vesper.arith.checked/1, about Lens's scalar model of the source function"});
    if let Some(key) = key {
        value["model_blake3"] = json!(proof_model::hex(key));
    }
    status_fields(&mut value, status);
    value
}
