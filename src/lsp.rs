//! Bounded stdio language server over open in-memory buffers.
//!
//! One reader thread frames client input; the session itself stays on the
//! calling thread, so requests are handled in arrival order. Proof audits run
//! on worker threads and answer later; `$/cancelRequest` answers a still
//! pending request with `RequestCancelled` exactly once and stops its job.
pub mod actions;
mod lens;
#[cfg(test)]
mod lens_tests;
pub mod macro_preview;
pub mod navigation;
mod proof_lens;
mod search;
mod session;

use serde_json::json;
pub use session::LensConfig;
use std::{
    io::{BufRead, Write},
    sync::mpsc,
    thread,
};
use vesper_lsp::{
    Envelope, Id, ProtocolErrorKind, ProtocolLimits, ResponsePayload, encode_frame,
    lens::{PendingRequests, ProofStatus, cancel_params},
    parse_envelope, read_frame,
};

const REQUEST_CANCELLED: i32 = -32800;

enum Event {
    Frame(Vec<u8>),
    Fatal(String),
    Eof,
    Proof {
        id: Id,
        job: Box<proof_lens::Job>,
        status: Option<ProofStatus>,
    },
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
    let mut session = session::Session::with_config(config)?;
    let mut pending = PendingRequests::new(64).map_err(|e| e.to_string())?;
    let (events, inbox) = mpsc::channel();
    let input = events.clone();
    // Detached: a blocked read must not keep `exit` from returning.
    thread::spawn(move || {
        loop {
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
    let finish = |pending: &mut PendingRequests| {
        for id in pending.cancel_all() {
            drop(id);
        }
    };
    while let Ok(event) = inbox.recv() {
        let body = match event {
            Event::Frame(body) => body,
            Event::Eof => {
                finish(&mut pending);
                return Ok(());
            }
            Event::Fatal(error) => {
                finish(&mut pending);
                return Err(error);
            }
            Event::Proof { id, job, status } => {
                if pending.finish(&id) {
                    let payload = match status {
                        Some(status) => ResponsePayload::Result(session.finish_proof(&job, status)),
                        None => cancelled(),
                    };
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
                continue;
            }
        };
        match message {
            Envelope::Request { id, method, params } if method == "workspace/executeCommand" => {
                let payload = match session.execute_command(params) {
                    Ok(session::CommandOutcome::Done(value)) => ResponsePayload::Result(value),
                    Ok(session::CommandOutcome::Run(job)) => match pending.begin(id.clone()) {
                        Ok(token) => {
                            let events = events.clone();
                            thread::spawn(move || {
                                let status = proof_lens::run(&job, &token);
                                let _ = events.send(Event::Proof {
                                    id,
                                    job: Box::new(job),
                                    status,
                                });
                            });
                            continue;
                        }
                        Err(error) => ResponsePayload::Error {
                            code: -32600,
                            message: error.to_string(),
                            data: None,
                        },
                    },
                    Err(error) => ResponsePayload::Error {
                        code: error.code,
                        message: error.message,
                        data: error.data,
                    },
                };
                send(
                    &mut writer,
                    Envelope::Response {
                        id: Some(id),
                        payload,
                    },
                    limits,
                )?;
            }
            Envelope::Request { id, method, params } => {
                let payload = match session.request(&method, params) {
                    Ok(value) => ResponsePayload::Result(value),
                    Err(error) => ResponsePayload::Error {
                        code: error.code,
                        message: error.message,
                        data: error.data,
                    },
                };
                send(
                    &mut writer,
                    Envelope::Response {
                        id: Some(id),
                        payload,
                    },
                    limits,
                )?;
            }
            Envelope::Notification { method, params } if method == "$/cancelRequest" => {
                // An answered or unknown ID is ignored, as the protocol allows.
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
            }
            Envelope::Notification { method, params } => {
                match session.notification(&method, params) {
                    Ok(session::NotificationOutcome::Continue(messages)) => {
                        for message in messages {
                            send(&mut writer, message, limits)?;
                        }
                    }
                    Ok(session::NotificationOutcome::Exit { clean }) => {
                        finish(&mut pending);
                        return if clean {
                            Ok(())
                        } else {
                            Err("LSP exit before shutdown".into())
                        };
                    }
                    Err(error) => {
                        // A notification never receives a JSON-RPC response.
                        let message: String = error.message.chars().take(4096).collect();
                        send(
                            &mut writer,
                            Envelope::Notification {
                                method: "window/logMessage".into(),
                                params: Some(json!({"type": 2, "message": message})),
                            },
                            limits,
                        )?;
                    }
                }
            }
            // This server issues no client requests, so responses carry no authority.
            Envelope::Response { .. } => (),
        }
    }
    Ok(())
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
