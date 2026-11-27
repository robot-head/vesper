//! Bounded stdio language server over open in-memory buffers.
//!
//! Three kinds of threads cooperate. One reader thread frames client input.
//! The dispatcher (the calling thread) parses frames and answers
//! cancellations. One session thread owns the editor state, handles requests
//! and notifications strictly in arrival order and writes its own responses
//! and notifications. The writer and the table of pending requests are
//! shared behind locks; a response is written only while its request is
//! still pending, under the table's lock, so every request is answered
//! exactly once. Because the dispatcher never waits for the session,
//! `$/cancelRequest` is handled as soon as it is read, for every request: a
//! request still queued is answered with `RequestCancelled` and never runs, a
//! running one is answered at once and its late result is dropped, and Lens
//! traversals poll the request's token so they stop early. Proof audits run
//! on worker threads.
pub mod actions;
pub mod anvil_actions;
mod lens;
#[cfg(test)]
mod lens_tests;
pub mod macro_preview;
pub mod navigation;
mod proof_lens;
mod proof_model;
mod search;
mod session;

use serde_json::json;
pub use session::LensConfig;
#[doc(hidden)]
pub use session::PortCheck;

/// Every proof-lens model of a checked source, for the differential test of
/// its Vesper port (`tests/self_host_lsp_features.rs`): per function, the
/// exact and bit-vector renderings and the model key as hex, or the reason
/// there is no model.
#[doc(hidden)]
pub fn proof_models_for_port(
    snapshot: &crate::tooling::frontend::CheckedSource,
) -> Vec<(String, Result<[String; 3], String>)> {
    proof_model::all(snapshot.checked())
        .into_iter()
        .map(|(name, model)| {
            let model = model.map(|m| [m.exact, m.bitvec, proof_model::hex(&m.key)]);
            (name, model)
        })
        .collect()
}
/// The ownership facts behind Lens views (`lens::View::checked`) as text,
/// for the Vesper port's differential test.
#[doc(hidden)]
pub fn lens_facts_for_port(
    snapshot: std::sync::Arc<crate::tooling::frontend::CheckedSource>,
    cursors: &[usize],
) -> String {
    lens::facts_dump(snapshot, cursors)
}
/// The repaired text and regions of a syntax-damaged document
/// (`lens::repair`) as text, for the Vesper port's differential test.
#[doc(hidden)]
pub fn lens_repair_for_port(
    syntax: &vesper_syntax::recovery::RecoverySyntax,
    limits: crate::tooling::frontend::AnalysisLimits,
) -> String {
    lens::repair_dump(syntax, limits)
}
/// A Lens view of `text` (semantic errors, or a repaired prefix whose
/// regions are `repair_dump`'s `G` rows in `regions`) and its answers to
/// the queries, as text, for the Vesper port's differential test.
#[doc(hidden)]
pub fn lens_view_for_port(
    text: &str,
    regions: &str,
    limits: crate::tooling::frontend::AnalysisLimits,
    cursors: &[usize],
    bindings: &[(String, usize)],
    members: &[(String, usize)],
    ceiling: Option<&std::collections::BTreeSet<String>>,
) -> String {
    lens::view_dump(
        text,
        lens::regions_from_dump(regions),
        limits,
        &lens::ViewQueries {
            cursors,
            bindings,
            members,
            ceiling,
        },
    )
}
/// A prepared proof-lens job (`proof_lens::prepare`), for the Vesper
/// port's differential test of the runner.
#[doc(hidden)]
pub struct ProofJobForPort(proof_lens::Job);
impl ProofJobForPort {
    /// The job as the Vesper runner reads it (`proof_lens::job_words`).
    pub fn words(&self) -> Vec<u8> {
        proof_lens::job_words(&self.0)
    }
    /// Each model's function and Lean module name, in job order.
    pub fn modules(&self) -> Vec<(String, String)> {
        self.0
            .models
            .iter()
            .map(|m| (m.function.clone(), m.module_name()))
            .collect()
    }
    /// Run the job with a token nobody cancels; its outcome as text
    /// (`proof_lens::outcome_rows`).
    pub fn run(&self) -> String {
        let cancel = vesper_lsp::lens::CancelToken::new();
        proof_lens::outcome_rows(proof_lens::run(&self.0, &cancel).as_ref())
    }
}

/// Prepare the proof-lens job for `source` (checked under the default
/// analysis limits): the extraction obligations of `extraction` and the
/// semantic models of `models` (functions without a model are skipped),
/// with the given toolchain.
#[doc(hidden)]
pub fn proof_job_for_port(
    source: &str,
    extraction: &[String],
    models: &[String],
    launcher: &str,
    auditor: Option<&std::path::Path>,
    timeout: u64,
    cache: Option<&std::path::Path>,
) -> Result<ProofJobForPort, String> {
    let snapshot = crate::tooling::frontend::check_source(source, Default::default())
        .map_err(|e| e.to_string())?;
    let all = proof_model::all(snapshot.checked());
    let chosen = models
        .iter()
        .filter_map(|name| all.get(name)?.as_ref().ok().cloned())
        .collect();
    let toolchain = proof_lens::Toolchain {
        launcher: launcher.to_owned(),
        auditor: auditor.map(std::path::Path::to_path_buf),
        timeout,
        cache: cache.map(std::path::Path::to_path_buf),
    };
    proof_lens::prepare(&snapshot, extraction, chosen, &toolchain).map(ProofJobForPort)
}

/// The Lens request layer (poison notices, member completion, obligation
/// hovers, inlay hints) on `document` at `uri`, against the view of
/// `view_text`: its checked snapshot when `checked`, else its semantic or
/// repaired view (regions as for `lens_view_for_port`), answered `rounds`
/// times on one view, as text, for the Vesper port's differential test.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn lens_requests_for_port(
    uri: &str,
    document: &str,
    view_text: &str,
    regions: &str,
    checked: bool,
    limits: crate::tooling::frontend::AnalysisLimits,
    index_limits: vesper_span::Limits,
    completions: &[vesper_span::Position],
    hovers: &[vesper_span::Position],
    inlays: &[vesper_span::Range],
    ceiling: Option<&std::collections::BTreeSet<String>>,
    rounds: u64,
) -> String {
    let view = if checked {
        match crate::tooling::frontend::check_source(view_text, limits) {
            Ok(snapshot) => lens::View::checked(std::sync::Arc::new(snapshot)),
            Err(_) => Err(lens::Unavailable::Guard),
        }
    } else {
        lens::Prepared::new(view_text, lens::regions_from_dump(regions), limits)
            .and_then(|prepared| lens::View::from_prepared(&prepared))
    };
    session::requests_dump(
        uri,
        document,
        index_limits,
        view.map(std::sync::Arc::new),
        &session::PortRequests {
            completions,
            hovers,
            inlays,
            ceiling,
        },
        rounds,
    )
}
/// `textDocument/formatting` (`actions::format_document`) on `source` at
/// `uri` and `version`, against `expected` (a version and BLAKE3 digest;
/// the document's own when `None`), run `rounds` times, as text for the
/// Vesper port's differential test: `E <error>`, or the edit array, the
/// versioned workspace edit and the provenance, one JSON value per line.
#[doc(hidden)]
pub fn formatting_for_port(
    uri: &str,
    version: i32,
    source: &str,
    expected: Option<(i32, [u8; 32])>,
    index_limits: vesper_span::Limits,
    rounds: u64,
) -> String {
    let index = match vesper_span::LineIndex::new(std::sync::Arc::from(source), index_limits) {
        Ok(index) => index,
        Err(e) => return format!("E index {e}"),
    };
    let mut out = String::new();
    for _ in 0..rounds.max(1) {
        out = match actions::ActionDocument::new(uri, version, source, &index) {
            Err(e) => format!("E {e}"),
            Ok(document) => {
                let stamp = expected.map_or(document.stamp(), |(version, source_hash)| {
                    actions::SourceStamp {
                        version,
                        source_hash,
                    }
                });
                match actions::format_document(&document, stamp) {
                    Err(e) => format!("E {e}"),
                    Ok(formatting) => format!(
                        "{}\n{}\n{}",
                        formatting.to_json(),
                        formatting.workspace_edit_json(),
                        formatting.provenance
                    ),
                }
            }
        };
        std::hint::black_box(&out);
    }
    out
}
/// The session's parameter checks (`validate_input`, `$/cancelRequest`
/// parameters, the `vesper.proofAudit` arguments against the documents
/// `open`), each run `rounds` times, as text, for the Vesper port's
/// differential test.
#[doc(hidden)]
pub fn command_checks_for_port(
    checks: &[(PortCheck, String, Option<serde_json::Value>)],
    open: &[(String, i32)],
    rounds: u64,
) -> String {
    session::command_checks_dump(checks, open, rounds)
}
/// A proof outcome for the Vesper port's proof-lens test: 0 not attempted,
/// 1 proved (detail), 2 checked (detail), 3 refuted (counterexample),
/// 4 failed (stage, reason).
#[doc(hidden)]
pub type PortProofStatus = (u8, String, String);
/// One step of a proof-lens session for the Vesper port's test: `L` renders
/// the code lenses of `source`, `P` records a finished `vesper.proofAudit`
/// job and renders its result, `B` records a finished background run and
/// renders its `vesper/proofStatus` parameters (nothing when the document
/// now hashes to `current`, not `source`).
#[doc(hidden)]
pub struct ProofLensOp {
    pub kind: u8,
    pub source: [u8; 32],
    pub current: [u8; 32],
    pub generation: u64,
    pub requested: Option<(String, bool)>,
    pub extraction: Vec<(String, PortProofStatus)>,
    pub semantic: Vec<(String, [u8; 32], PortProofStatus)>,
    pub reused: usize,
    pub compiled: usize,
    pub elapsed_ms: u64,
}
/// The proof-lens functions (name, span start) and model keys (or the
/// reason there is no model) of a checked source, as the session's code
/// lenses read them.
#[doc(hidden)]
#[allow(clippy::type_complexity)]
pub fn proof_lens_inputs_for_port(
    snapshot: &crate::tooling::frontend::CheckedSource,
) -> (
    Vec<(String, usize)>,
    Vec<(String, Result<[u8; 32], String>)>,
) {
    let models = proof_model::all(snapshot.checked())
        .into_iter()
        .map(|(name, model)| (name, model.map(|m| m.key)))
        .collect();
    (session::lens_functions(snapshot), models)
}
fn port_status((code, a, b): &PortProofStatus) -> vesper_lsp::lens::ProofStatus {
    use vesper_lsp::lens::ProofStatus;
    match code {
        1 => ProofStatus::Proved { detail: a.clone() },
        2 => ProofStatus::Checked { detail: a.clone() },
        3 => ProofStatus::Refuted {
            counterexample: a.clone(),
        },
        4 => ProofStatus::Failed {
            stage: a.clone(),
            reason: b.clone(),
        },
        _ => ProofStatus::NotAttempted,
    }
}
/// The session's proof-lens answers over `ops`, from empty ledgers of the
/// given capacities, `rounds` times (the last answers, or "" for no
/// rounds): an `L` row per code-lens answer, a `P` row per command result
/// and a `B` row per background notification, as JSON.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn proof_lenses_for_port(
    uri: &str,
    version: i32,
    text: &str,
    index_limits: vesper_span::Limits,
    functions: &[(String, usize)],
    models: &[(String, Result<[u8; 32], String>)],
    capacities: (usize, usize),
    ops: &[ProofLensOp],
    rounds: u64,
) -> String {
    use std::fmt::Write as _;
    let index = match vesper_span::LineIndex::new(std::sync::Arc::from(text), index_limits) {
        Ok(index) => index,
        Err(error) => return format!("E\t{error}\n"),
    };
    let models: std::collections::BTreeMap<&str, Result<[u8; 32], &str>> = models
        .iter()
        .map(|(name, m)| {
            (
                name.as_str(),
                m.as_ref().map(|k| *k).map_err(String::as_str),
            )
        })
        .collect();
    let outcomes: Vec<proof_lens::Outcome> = ops
        .iter()
        .map(|op| proof_lens::Outcome {
            extraction: op
                .extraction
                .iter()
                .map(|(f, s)| (f.clone(), port_status(s)))
                .collect(),
            semantic: op
                .semantic
                .iter()
                .map(|(f, k, s)| (f.clone(), *k, port_status(s)))
                .collect(),
            reused: op.reused,
            compiled: op.compiled,
            elapsed: std::time::Duration::from_millis(op.elapsed_ms),
        })
        .collect();
    let mut out = String::new();
    for _ in 0..rounds {
        out.clear();
        let ledger = |n| vesper_lsp::lens::ProofLedger::new(n).expect("ledger capacity");
        let (mut proofs, mut model_proofs) = (ledger(capacities.0), ledger(capacities.1));
        for (op, outcome) in ops.iter().zip(&outcomes) {
            match op.kind {
                b'L' => {
                    let answer = session::code_lenses_of(
                        uri,
                        version,
                        &index,
                        op.source,
                        functions,
                        |name| models.get(name).copied(),
                        &proofs,
                        &model_proofs,
                    );
                    let answer = answer.unwrap_or_else(
                        |e| json!({"error":{"code":e.code,"message":e.message,"data":e.data}}),
                    );
                    let _ = writeln!(out, "L\t{answer}");
                }
                b'P' => {
                    session::record_of(&mut proofs, &mut model_proofs, op.source, outcome);
                    let answer =
                        session::proof_result_of(op.source, op.requested.as_ref(), outcome);
                    let _ = writeln!(out, "P\t{answer}");
                }
                _ => {
                    session::record_of(&mut proofs, &mut model_proofs, op.source, outcome);
                    if op.current != op.source {
                        let _ = writeln!(out, "B\t-");
                    } else {
                        let answer = session::proof_status_of(
                            uri,
                            version,
                            op.generation,
                            op.source,
                            outcome,
                        );
                        let _ = writeln!(out, "B\t{answer}");
                    }
                }
            }
        }
    }
    out
}
/// `lens_view_for_port` with the guards, parse and check done once and
/// the view and its answers built `rounds` times (the last answer, or ""
/// for no rounds), for the Vesper port's speed comparison.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn lens_view_rounds_for_port(
    text: &str,
    regions: &str,
    limits: crate::tooling::frontend::AnalysisLimits,
    cursors: &[usize],
    bindings: &[(String, usize)],
    members: &[(String, usize)],
    ceiling: Option<&std::collections::BTreeSet<String>>,
    rounds: u64,
) -> String {
    let prepared = match lens::Prepared::new(text, lens::regions_from_dump(regions), limits) {
        Ok(prepared) => prepared,
        Err(unavailable) => return format!("U\t{}\n", unavailable.as_str()),
    };
    let queries = lens::ViewQueries {
        cursors,
        bindings,
        members,
        ceiling,
    };
    let mut out = String::new();
    for _ in 0..rounds {
        out = lens::prepared_dump(&prepared, &queries);
    }
    out
}
use std::{
    cell::RefCell,
    io::{BufRead, Write},
    sync::{
        Mutex, MutexGuard,
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread,
    time::Duration,
};
use vesper_lsp::{
    Envelope, Id, ProtocolErrorKind, ProtocolLimits, ResponsePayload, encode_frame,
    lens::{CancelToken, PendingRequests, cancel_params},
    parse_envelope, read_frame,
};

const REQUEST_CANCELLED: i32 = -32800;
/// Frames read but not yet handled. The reader takes one credit per frame and
/// a credit returns once the frame has been handled, so a stalled session
/// stops the reader after this many frames (bounded memory), while a cancel
/// arriving behind one long request is still read and answered at once.
const READ_AHEAD: usize = 32;
const MAX_PENDING: usize = 256;

thread_local! {
    /// The token of the request the session thread is handling now.
    static ACTIVE: RefCell<Option<CancelToken>> = const { RefCell::new(None) };
}

/// True when the request being handled on this thread has been cancelled.
/// Long Lens traversals poll this and stop with a typed cancellation.
pub(crate) fn request_cancelled() -> bool {
    ACTIVE.with(|active| {
        active
            .borrow()
            .as_ref()
            .is_some_and(CancelToken::is_cancelled)
    })
}

enum Event {
    Frame(Vec<u8>),
    Fatal(String),
    Eof,
    /// The session could not write to the client.
    WriteFailed(String),
    Exit {
        clean: bool,
    },
    /// The session handled everything queued before end of input.
    Drained(Option<String>),
    /// A requested proof job finished (`None`: it was cancelled).
    Proof {
        id: Id,
        token: CancelToken,
        job: Box<proof_lens::Job>,
        outcome: Option<proof_lens::Outcome>,
    },
    /// A debounce timer fired.
    Timer {
        uri: String,
    },
    /// A debounced background proof run finished.
    Background {
        uri: String,
        generation: u64,
        job: Box<proof_lens::Job>,
        outcome: Option<proof_lens::Outcome>,
    },
}

/// Work for the session thread, in arrival order.
enum Work {
    Request {
        id: Id,
        method: String,
        params: Option<serde_json::Value>,
        token: CancelToken,
    },
    Notification {
        method: String,
        params: Option<serde_json::Value>,
    },
    ProofDone {
        id: Id,
        token: CancelToken,
        job: Box<proof_lens::Job>,
        outcome: proof_lens::Outcome,
    },
    Timer {
        uri: String,
    },
    BackgroundDone {
        uri: String,
        generation: u64,
        job: Box<proof_lens::Job>,
        outcome: proof_lens::Outcome,
    },
    /// End of input (or a fatal framing error): answer once all earlier
    /// work has been handled.
    Drain(Option<String>),
}

/// Serve framed JSON-RPC without treating document URIs as filesystem access.
/// Framing errors terminate the stream; a rejected body has a known boundary
/// and receives a bounded error before the next frame is read.
pub fn serve<R: BufRead + Send + 'static>(
    reader: R,
    writer: impl Write + Send,
) -> Result<(), String> {
    serve_with(reader, writer, LensConfig::default())
}

/// The writer and the pending-request table, shared by the dispatcher and
/// the session thread. Lock order: `pending` before `writer`.
struct Shared<W> {
    pending: Mutex<PendingRequests>,
    writer: Mutex<W>,
    limits: ProtocolLimits,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking holder leaves consistent data: frames are written whole.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
impl<W: Write> Shared<W> {
    fn write(&self, message: Envelope) -> Result<(), String> {
        send(&mut *lock(&self.writer), message, self.limits)
    }
    /// Answer a request unless it was cancelled (and so already answered).
    fn answer(&self, id: Id, token: &CancelToken, payload: ResponsePayload) -> Result<(), String> {
        let mut pending = lock(&self.pending);
        if pending.finish(&id, token) {
            self.write(Envelope::Response {
                id: Some(id),
                payload,
            })?;
        }
        Ok(())
    }
}

/// [`serve`] with explicit server-side Lens configuration.
pub fn serve_with<R: BufRead + Send + 'static>(
    mut reader: R,
    writer: impl Write + Send,
    config: LensConfig,
) -> Result<(), String> {
    let limits = ProtocolLimits::default();
    let shared = Shared {
        pending: Mutex::new(PendingRequests::new(MAX_PENDING).map_err(|e| e.to_string())?),
        writer: Mutex::new(writer),
        limits,
    };
    let (events, inbox) = mpsc::channel::<Event>();
    let (credit, credits) = mpsc::sync_channel::<()>(READ_AHEAD);
    for _ in 0..READ_AHEAD {
        credit.try_send(()).map_err(|_| "read-ahead credits")?;
    }
    let input = events.clone();
    // Detached: a blocked read must not keep `exit` from returning.
    let start_reader = move || {
        thread::spawn(move || {
            loop {
                if credits.recv().is_err() {
                    return;
                }
                let event = match read_frame(&mut reader, limits) {
                    Ok(Some(body)) => Event::Frame(body),
                    Ok(None) => Event::Eof,
                    Err(error) => Event::Fatal(error.to_string()),
                };
                let last = !matches!(event, Event::Frame(_));
                if input.send(event).is_err() || last {
                    return;
                }
            }
        });
    };
    // The session borrows the writer, so it runs on a scoped thread; the
    // scope ends once the session has stopped (its queue closes when the
    // dispatcher returns).
    thread::scope(|scope| {
        let (work, queue) = mpsc::channel::<(Work, bool)>();
        let (ready_tx, ready) = mpsc::sync_channel(1);
        {
            let events = events.clone();
            let credit = credit.clone();
            let shared = &shared;
            scope.spawn(move || session_thread(config, queue, events, credit, ready_tx, shared));
        }
        ready
            .recv()
            .map_err(|_| "Lens session thread failed to start".to_owned())??;
        start_reader();
        dispatch(&shared, &inbox, &work, &credit)
    })
}

/// The dispatcher loop: frames, cancellations and worker events.
fn dispatch<W: Write>(
    shared: &Shared<W>,
    inbox: &Receiver<Event>,
    work: &Sender<(Work, bool)>,
    credit: &SyncSender<()>,
) -> Result<(), String> {
    let give_back = || {
        let _ = credit.try_send(());
    };
    let finish = || {
        for id in lock(&shared.pending).cancel_all() {
            drop(id);
        }
    };
    while let Ok(event) = inbox.recv() {
        let body = match event {
            Event::Frame(body) => body,
            // Input ended: everything already read is still handled, in
            // order, before the server returns.
            Event::Eof => {
                let _ = work.send((Work::Drain(None), false));
                continue;
            }
            Event::Fatal(error) => {
                let _ = work.send((Work::Drain(Some(error)), false));
                continue;
            }
            Event::Drained(error) => {
                finish();
                return error.map_or(Ok(()), Err);
            }
            Event::WriteFailed(error) => {
                finish();
                return Err(error);
            }
            Event::Exit { clean } => {
                finish();
                return if clean {
                    Ok(())
                } else {
                    Err("LSP exit before shutdown".into())
                };
            }
            Event::Proof {
                id,
                token,
                job,
                outcome,
            } => {
                match outcome {
                    // The session records the outcome and renders the answer.
                    Some(outcome) if lock(&shared.pending).is_pending(&id, &token) => {
                        let _ = work.send((
                            Work::ProofDone {
                                id,
                                token,
                                job,
                                outcome,
                            },
                            false,
                        ));
                    }
                    // Cancelled jobs were answered when they were cancelled.
                    _ => {}
                }
                continue;
            }
            Event::Timer { uri } => {
                let _ = work.send((Work::Timer { uri }, false));
                continue;
            }
            Event::Background {
                uri,
                generation,
                job,
                outcome,
            } => {
                if let Some(outcome) = outcome {
                    let _ = work.send((
                        Work::BackgroundDone {
                            uri,
                            generation,
                            job,
                            outcome,
                        },
                        false,
                    ));
                }
                continue;
            }
        };
        let message = match parse_envelope(&body, shared.limits) {
            Ok(message) => message,
            Err(error) => {
                let code = match error.kind {
                    ProtocolErrorKind::Utf8 | ProtocolErrorKind::Json => -32700,
                    _ => -32600,
                };
                shared.write(Envelope::Response {
                    id: None,
                    payload: ResponsePayload::Error {
                        code,
                        message: error.to_string(),
                        data: None,
                    },
                })?;
                give_back();
                continue;
            }
        };
        match message {
            Envelope::Request { id, method, params } => {
                let begun = lock(&shared.pending).begin(id.clone());
                match begun {
                    Ok(token) => {
                        if work
                            .send((
                                Work::Request {
                                    id,
                                    method,
                                    params,
                                    token,
                                },
                                true,
                            ))
                            .is_err()
                        {
                            return Err("Lens session thread stopped".into());
                        }
                    }
                    Err(error) => {
                        shared.write(Envelope::Response {
                            id: Some(id),
                            payload: ResponsePayload::Error {
                                code: -32600,
                                message: error.to_string(),
                                data: None,
                            },
                        })?;
                        give_back();
                    }
                }
            }
            Envelope::Notification { method, params } if method == "$/cancelRequest" => {
                // An answered or unknown ID is ignored, as the protocol allows.
                // A queued request never runs; a running one stops polling.
                if let Ok(id) = cancel_params(params.as_ref()) {
                    let mut pending = lock(&shared.pending);
                    if pending.cancel(&id) {
                        shared.write(Envelope::Response {
                            id: Some(id),
                            payload: cancelled(),
                        })?;
                    }
                }
                give_back();
            }
            Envelope::Notification { method, params } => {
                if work
                    .send((Work::Notification { method, params }, true))
                    .is_err()
                {
                    return Err("Lens session thread stopped".into());
                }
            }
            // This server's own requests (code lens refresh) need no answer,
            // so responses carry no authority.
            Envelope::Response { .. } => give_back(),
        }
    }
    Ok(())
}

fn session_thread<W: Write>(
    config: LensConfig,
    queue: Receiver<(Work, bool)>,
    events: Sender<Event>,
    credit: SyncSender<()>,
    ready: SyncSender<Result<(), String>>,
    shared: &Shared<W>,
) {
    let mut session = match session::Session::with_config(config) {
        Ok(session) => {
            let _ = ready.send(Ok(()));
            session
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    while let Ok((item, frame)) = queue.recv() {
        let exit = match handle_work(&mut session, item, &events, shared) {
            Ok(exit) => exit,
            Err(error) => {
                let _ = events.send(Event::WriteFailed(error));
                true
            }
        };
        if frame {
            let _ = credit.try_send(());
        }
        if exit {
            break;
        }
    }
    session.stop_background();
}

/// Handle one unit of work, writing its answers directly. True when the
/// server must exit.
fn handle_work<W: Write>(
    session: &mut session::Session,
    item: Work,
    events: &Sender<Event>,
    shared: &Shared<W>,
) -> Result<bool, String> {
    match item {
        Work::Request {
            id,
            method,
            params,
            token,
        } => {
            // A request cancelled while it waited in the queue never runs.
            if token.is_cancelled() {
                return Ok(false);
            }
            ACTIVE.with(|active| *active.borrow_mut() = Some(token.clone()));
            #[cfg(test)]
            if method == "vesper/testBlock" {
                // Test hook: a request that runs until it is cancelled, so the
                // protocol tests can cancel a long non-proof request.
                let started = std::time::Instant::now();
                while !request_cancelled() && started.elapsed() < Duration::from_secs(30) {
                    thread::sleep(Duration::from_millis(5));
                }
                ACTIVE.with(|active| *active.borrow_mut() = None);
                shared.answer(id, &token, ResponsePayload::Result(json!("finished")))?;
                return Ok(false);
            }
            let result = if method == "workspace/executeCommand" {
                match session.execute_command(params) {
                    Ok(session::CommandOutcome::Done(value)) => Ok(Some(value)),
                    Ok(session::CommandOutcome::Run(job)) => {
                        let events = events.clone();
                        let token = token.clone();
                        let id = id.clone();
                        thread::spawn(move || {
                            let outcome = proof_lens::run(&job, &token);
                            let _ = events.send(Event::Proof {
                                id,
                                token,
                                job: Box::new(job),
                                outcome,
                            });
                        });
                        Ok(None)
                    }
                    Err(error) => Err(error),
                }
            } else {
                session.request(&method, params).map(Some)
            };
            ACTIVE.with(|active| *active.borrow_mut() = None);
            let payload = match result {
                Ok(Some(value)) => ResponsePayload::Result(value),
                Ok(None) => return Ok(false),
                Err(error) => ResponsePayload::Error {
                    code: error.code,
                    message: error.message,
                    data: error.data,
                },
            };
            shared.answer(id, &token, payload)?;
        }
        Work::Notification { method, params } => match session.notification(&method, params) {
            Ok(session::NotificationOutcome::Continue(messages)) => {
                for message in messages {
                    shared.write(message)?;
                }
                arm_timers(session, events);
            }
            Ok(session::NotificationOutcome::Exit { clean }) => {
                let _ = events.send(Event::Exit { clean });
                return Ok(true);
            }
            Err(error) => {
                // A notification never receives a JSON-RPC response.
                let message: String = error.message.chars().take(4096).collect();
                shared.write(Envelope::Notification {
                    method: "window/logMessage".into(),
                    params: Some(json!({"type": 2, "message": message})),
                })?;
            }
        },
        Work::ProofDone {
            id,
            token,
            job,
            outcome,
        } => {
            let value = session.finish_proof(&job, outcome);
            shared.answer(id, &token, ResponsePayload::Result(value))?;
        }
        Work::Timer { uri } => match session.timer_fired(&uri) {
            session::TimerOutcome::Rearm(delay) => arm(events, uri, delay),
            session::TimerOutcome::Start {
                generation,
                job,
                token,
            } => {
                let events = events.clone();
                thread::spawn(move || {
                    let outcome = proof_lens::run(&job, &token);
                    let _ = events.send(Event::Background {
                        uri,
                        generation,
                        job,
                        outcome,
                    });
                });
            }
            session::TimerOutcome::Idle => {}
        },
        Work::Drain(error) => {
            let _ = events.send(Event::Drained(error));
            return Ok(true);
        }
        Work::BackgroundDone {
            uri,
            generation,
            job,
            outcome,
        } => {
            let messages = session.finish_background(&uri, generation, &job, outcome);
            for message in messages {
                shared.write(message)?;
            }
            // A batch that left obligations behind arms the next one.
            arm_timers(session, events);
        }
    }
    Ok(false)
}

fn arm_timers(session: &mut session::Session, events: &Sender<Event>) {
    for (uri, delay) in session.take_timers() {
        arm(events, uri, delay);
    }
}

fn arm(events: &Sender<Event>, uri: String, delay: Duration) {
    let events = events.clone();
    thread::spawn(move || {
        thread::sleep(delay);
        let _ = events.send(Event::Timer { uri });
    });
}

fn cancelled() -> ResponsePayload {
    ResponsePayload::Error {
        code: REQUEST_CANCELLED,
        message: "Request cancelled".into(),
        data: None,
    }
}

fn send(writer: &mut impl Write, message: Envelope, limits: ProtocolLimits) -> Result<(), String> {
    let bytes = match encode_frame(&message, limits) {
        Ok(bytes) => bytes,
        Err(error) => {
            // Encoding completes before writing. A refused feature result must
            // not leave a partial frame or silently lose its request ID.
            let Envelope::Response { id, .. } = message else {
                return Err(error.to_string());
            };
            encode_frame(
                &Envelope::Response {
                    id,
                    payload: ResponsePayload::Error {
                        code: -32004,
                        message: "Editor response exceeds protocol output limits".into(),
                        data: Some(json!({"status": "resource_unavailable"})),
                    },
                },
                limits,
            )
            .map_err(|e| e.to_string())?
        }
    };
    writer.write_all(&bytes).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

/// A macro preview snapshot of `snapshot_text` in its own database, for the
/// Vesper port's differential test (macro_preview.ves): Rust's rendering of
/// a document against it, and the words the port reads for the same call.
#[doc(hidden)]
pub struct MacroPreviewForPort {
    db: crate::database::Database,
    file: crate::database::SourceFile,
}

#[doc(hidden)]
impl MacroPreviewForPort {
    pub fn new(snapshot_text: &str) -> Self {
        let db = crate::database::Database::default();
        let file = crate::database::SourceFile::new(&db, snapshot_text.to_owned());
        let _ = crate::database::macro_preview::preview(&db, file);
        Self { db, file }
    }

    /// `E <error>`, or the rendered value as JSON, after `rounds` renders.
    pub fn render(
        &self,
        uri: &str,
        version: i32,
        text: &str,
        limits: macro_preview::Limits,
        rounds: u64,
    ) -> String {
        let snapshot = crate::database::macro_preview::preview(&self.db, self.file);
        let mut out = String::new();
        for _ in 0..rounds {
            out = match macro_preview::Document::new(uri, version, text, snapshot)
                .and_then(|d| macro_preview::render(&d, limits))
            {
                Ok(value) => serde_json::to_string(&value).unwrap_or_default(),
                Err(e) => format!("E {e}"),
            };
        }
        out
    }

    pub fn words(
        &self,
        uri: &str,
        version: i32,
        text: &str,
        limits: macro_preview::Limits,
        host_digest: bool,
        rounds: u64,
    ) -> Vec<u8> {
        let snapshot = crate::database::macro_preview::preview(&self.db, self.file);
        let digest = host_digest.then(|| *blake3::hash(text.as_bytes()).as_bytes());
        macro_preview::port_words(uri, version, text, snapshot, limits, digest, rounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use vesper_lsp::read_message;

    #[test]
    fn oversized_result_returns_one_bounded_error_with_original_id() {
        let limits = ProtocolLimits {
            max_reply_bytes: 256,
            ..Default::default()
        };
        let mut wire = Vec::new();
        send(
            &mut wire,
            Envelope::Response {
                id: Some(Id::String("request".into())),
                payload: ResponsePayload::Result(json!("x".repeat(2048))),
            },
            limits,
        )
        .unwrap();
        let mut wire = Cursor::new(wire);
        let response = read_message(&mut wire, ProtocolLimits::default())
            .unwrap()
            .unwrap();
        assert!(matches!(response, Envelope::Response {
            id: Some(Id::String(id)),
            payload: ResponsePayload::Error { code: -32004, .. }
        } if id == "request"));
        assert!(
            read_message(&mut wire, ProtocolLimits::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn refused_notification_writes_nothing() {
        let limits = ProtocolLimits {
            max_reply_bytes: 256,
            ..Default::default()
        };
        let mut wire = Vec::new();
        assert!(
            send(
                &mut wire,
                Envelope::Notification {
                    method: "window/logMessage".into(),
                    params: Some(json!({"message": "x".repeat(2048)}))
                },
                limits
            )
            .is_err()
        );
        assert!(wire.is_empty());
    }
}
