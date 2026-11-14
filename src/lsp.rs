//! Bounded stdio language server over open in-memory buffers.
//!
//! Three kinds of threads cooperate. One reader thread frames client input.
//! The dispatcher (the calling thread) owns the writer and the table of
//! pending requests. One session thread owns the editor state and handles
//! requests and notifications strictly in arrival order. Because the
//! dispatcher never waits for the session, `$/cancelRequest` is handled as
//! soon as it is read, for every request: a request still queued is answered
//! with `RequestCancelled` and never runs, a running one is answered at once
//! and its late result is dropped, and Lens traversals poll the request's
//! token so they stop early. Proof audits run on worker threads.
pub mod actions;
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
use std::{
    cell::RefCell,
    io::{BufRead, Write},
    sync::mpsc::{self, Receiver, Sender, SyncSender},
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
    /// The session answered a request.
    Reply {
        id: Id,
        token: CancelToken,
        payload: ResponsePayload,
    },
    /// Notifications (diagnostics, logs, proof status) from the session.
    Messages(Vec<Envelope>),
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
pub fn serve<R: BufRead + Send + 'static>(reader: R, writer: impl Write) -> Result<(), String> {
    serve_with(reader, writer, LensConfig::default())
}

/// [`serve`] with explicit server-side Lens configuration.
pub fn serve_with<R: BufRead + Send + 'static>(
    mut reader: R,
    mut writer: impl Write,
    config: LensConfig,
) -> Result<(), String> {
    let limits = ProtocolLimits::default();
    let mut pending = PendingRequests::new(MAX_PENDING).map_err(|e| e.to_string())?;
    let (events, inbox) = mpsc::channel::<Event>();
    let (credit, credits) = mpsc::sync_channel::<()>(READ_AHEAD);
    for _ in 0..READ_AHEAD {
        credit.try_send(()).map_err(|_| "read-ahead credits")?;
    }

    // The session lives on its own thread; it reports readiness first.
    let (work, queue) = mpsc::channel::<(Work, bool)>();
    let (ready_tx, ready) = mpsc::sync_channel(1);
    {
        let events = events.clone();
        let credit = credit.clone();
        thread::spawn(move || session_thread(config, queue, events, credit, ready_tx));
    }
    ready
        .recv()
        .map_err(|_| "Lens session thread failed to start".to_owned())??;

    let input = events.clone();
    // Detached: a blocked read must not keep `exit` from returning.
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
    let give_back = || {
        let _ = credit.try_send(());
    };
    let finish = |pending: &mut PendingRequests| {
        for id in pending.cancel_all() {
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
                finish(&mut pending);
                return error.map_or(Ok(()), Err);
            }
            Event::Reply { id, token, payload } => {
                // A cancelled request was already answered: drop its result.
                if pending.finish(&id, &token) {
                    send(
                        &mut writer,
                        Envelope::Response {
                            id: Some(id),
                            payload,
                        },
                        limits,
                    )?;
                }
                continue;
            }
            Event::Messages(messages) => {
                for message in messages {
                    send(&mut writer, message, limits)?;
                }
                continue;
            }
            Event::Exit { clean } => {
                finish(&mut pending);
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
                    Some(outcome) if pending.is_pending(&id, &token) => {
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
        let message = match parse_envelope(&body, limits) {
            Ok(message) => message,
            Err(error) => {
                let code = match error.kind {
                    ProtocolErrorKind::Utf8 | ProtocolErrorKind::Json => -32700,
                    _ => -32600,
                };
                send(
                    &mut writer,
                    Envelope::Response {
                        id: None,
                        payload: ResponsePayload::Error {
                            code,
                            message: error.to_string(),
                            data: None,
                        },
                    },
                    limits,
                )?;
                give_back();
                continue;
            }
        };
        match message {
            Envelope::Request { id, method, params } => match pending.begin(id.clone()) {
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
                    send(
                        &mut writer,
                        Envelope::Response {
                            id: Some(id),
                            payload: ResponsePayload::Error {
                                code: -32600,
                                message: error.to_string(),
                                data: None,
                            },
                        },
                        limits,
                    )?;
                    give_back();
                }
            },
            Envelope::Notification { method, params } if method == "$/cancelRequest" => {
                // An answered or unknown ID is ignored, as the protocol allows.
                // A queued request never runs; a running one stops polling.
                if let Ok(id) = cancel_params(params.as_ref())
                    && pending.cancel(&id)
                {
                    send(
                        &mut writer,
                        Envelope::Response {
                            id: Some(id),
                            payload: cancelled(),
                        },
                        limits,
                    )?;
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

fn session_thread(
    config: LensConfig,
    queue: Receiver<(Work, bool)>,
    events: Sender<Event>,
    credit: SyncSender<()>,
    ready: SyncSender<Result<(), String>>,
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
        let exit = handle_work(&mut session, item, &events);
        if frame {
            let _ = credit.try_send(());
        }
        if exit {
            break;
        }
    }
    session.stop_background();
}

/// Handle one unit of work. True when the server must exit.
fn handle_work(session: &mut session::Session, item: Work, events: &Sender<Event>) -> bool {
    match item {
        Work::Request {
            id,
            method,
            params,
            token,
        } => {
            // A request cancelled while it waited in the queue never runs.
            if token.is_cancelled() {
                return false;
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
                let _ = events.send(Event::Reply {
                    id,
                    token,
                    payload: ResponsePayload::Result(json!("finished")),
                });
                return false;
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
                Ok(None) => return false,
                Err(error) => ResponsePayload::Error {
                    code: error.code,
                    message: error.message,
                    data: error.data,
                },
            };
            let _ = events.send(Event::Reply { id, token, payload });
        }
        Work::Notification { method, params } => match session.notification(&method, params) {
            Ok(session::NotificationOutcome::Continue(messages)) => {
                if !messages.is_empty() {
                    let _ = events.send(Event::Messages(messages));
                }
                arm_timers(session, events);
            }
            Ok(session::NotificationOutcome::Exit { clean }) => {
                let _ = events.send(Event::Exit { clean });
                return true;
            }
            Err(error) => {
                // A notification never receives a JSON-RPC response.
                let message: String = error.message.chars().take(4096).collect();
                let _ = events.send(Event::Messages(vec![Envelope::Notification {
                    method: "window/logMessage".into(),
                    params: Some(json!({"type": 2, "message": message})),
                }]));
            }
        },
        Work::ProofDone {
            id,
            token,
            job,
            outcome,
        } => {
            let value = session.finish_proof(&job, outcome);
            let _ = events.send(Event::Reply {
                id,
                token,
                payload: ResponsePayload::Result(value),
            });
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
            return true;
        }
        Work::BackgroundDone {
            uri,
            generation,
            job,
            outcome,
        } => {
            let messages = session.finish_background(&uri, generation, &job, outcome);
            if !messages.is_empty() {
                let _ = events.send(Event::Messages(messages));
            }
            // A batch that left obligations behind arms the next one.
            arm_timers(session, events);
        }
    }
    false
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
