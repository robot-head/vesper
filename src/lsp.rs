//! Bounded synchronous stdio language server over open in-memory buffers.
pub mod actions;
pub mod macro_preview;
pub mod navigation;
mod search;
mod session;

use serde_json::json;
use std::io::{BufRead, Write};
use vesper_lsp::{
    Envelope, ProtocolErrorKind, ProtocolLimits, ResponsePayload, encode_frame, parse_envelope,
    read_frame,
};

/// Serve framed JSON-RPC without treating document URIs as filesystem access.
/// Framing errors terminate the stream; a rejected body has a known boundary
/// and receives a bounded error before the next frame is read.
pub fn serve(mut reader: impl BufRead, mut writer: impl Write) -> Result<(), String> {
    let limits = ProtocolLimits::default();
    let mut session = session::Session::new()?;
    while let Some(body) = read_frame(&mut reader, limits).map_err(|e| e.to_string())? {
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
            Envelope::Notification { method, params } => {
                match session.notification(&method, params) {
                    Ok(session::NotificationOutcome::Continue(messages)) => {
                        for message in messages {
                            send(&mut writer, message, limits)?;
                        }
                    }
                    Ok(session::NotificationOutcome::Exit { clean }) => {
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
    use vesper_lsp::{Id, read_message};

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
