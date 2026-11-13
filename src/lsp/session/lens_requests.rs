//! Lens requests on the session: poison notices, obligation-guided completion,
//! obligation hovers and inlay hints, and proof code lenses.
use super::*;
use crate::{
    lsp::{
        lens::{Binding, Unavailable, View},
        proof_lens,
    },
    tooling::frontend::AnalysisLimits,
};
use vesper_lsp::lens::{CompletionTier, ProofStatus};

pub(crate) const PROOF_COMMAND: &str = "vesper.proofAudit";

/// Server-side Lens configuration. Clients cannot choose executables.
#[derive(Clone, Debug)]
pub struct LensConfig {
    /// Lean launcher (`VESPER_LEAN`, else `lean` on PATH).
    pub lean: String,
    /// The `vesper` executable whose `proof-audit` command replays proofs.
    /// Defaults to the running executable when it is named `vesper`.
    pub auditor: Option<std::path::PathBuf>,
    /// Seconds allowed for each Lean process (1 to 300).
    pub proof_timeout: u64,
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
        }
    }
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
                Ok((syntax, _, _)) => View::recovered(syntax, AnalysisLimits::default()),
                Err(error) => return Err(error),
            },
            Err(AnalysisFailure::Semantic { .. }) => {
                View::semantic(document.text(), AnalysisLimits::default())
            }
            Err(_) => Err(Unavailable::Resource),
        }
        .map(Arc::new);
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
                    "span":byte_range(&r.span),"cut":r.cut})).collect::<Vec<_>>()}),
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
                    "scope": "lean_extraction_axiom_free"}}
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
        let uri = string(field(argument, "uri")?, "uri")?;
        let function = string(field(argument, "function")?, "function")?;
        if function.len() > 1024 {
            return Err(ServerError::params("function name is too long"));
        }
        let document = self.document(uri)?;
        if let Some(expected) = argument.get("version").map(version).transpose()?
            && expected != document.version()
        {
            return Err(ServerError::new(-32801, "Document version has changed"));
        }
        let snapshot = self.snapshot(uri)?;
        match proof_lens::prepare(
            &snapshot,
            function,
            &self.lens_config.lean,
            self.lens_config.auditor.as_deref(),
            self.lens_config.proof_timeout,
        ) {
            Ok(job) => Ok(CommandOutcome::Run(job)),
            Err(reason) => {
                let status = ProofStatus::Failed {
                    stage: "extract".into(),
                    reason,
                };
                let value = proof_value(snapshot.source_hash(), function, &status);
                self.proofs.record(snapshot.source_hash(), function, status);
                Ok(CommandOutcome::Done(value))
            }
        }
    }

    /// Record a finished proof job and render its command result.
    pub(crate) fn finish_proof(&mut self, job: &proof_lens::Job, status: ProofStatus) -> Value {
        let value = proof_value(job.source, &job.function, &status);
        self.proofs.record(job.source, &job.function, status);
        value
    }
}

fn proof_value(source: [u8; 32], function: &str, status: &ProofStatus) -> Value {
    let mut value = json!({"function": function, "status": status.as_str(),
        "scope": "lean_extraction_axiom_free",
        "source_blake3": blake3::Hash::from_bytes(source).to_hex().as_str()});
    match status {
        ProofStatus::Proved { detail } => value["detail"] = json!(detail),
        ProofStatus::Failed { stage, reason } => {
            value["stage"] = json!(stage);
            value["reason"] = json!(reason);
        }
        ProofStatus::NotAttempted => {}
    }
    value
}
