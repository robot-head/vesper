//! Serving a [`vesper_prism`] router over HTTP/3.
//!
//! [`answer`] is the single dispatch path: decode the target once
//! ([`ParsedRequest`]), run the middleware chain and the router
//! ([`Service::dispatch`]), and call the application [`Handler`]. The
//! HTTP/1.1 path (`vesper_prism::connection::Connection`) and the HTTP/3
//! path ([`serve`]) both call it with a [`vesper_prism::Request`], so one
//! handler answers both protocols with the same response.

use vesper_prism::connection::Response as PrismResponse;
use vesper_prism::router::RouteMatch;
use vesper_prism::service::{Dispatch, ParsedRequest, Service, method_not_allowed};
use vesper_prism::target::TargetLimits;
use vesper_prism::{Header, Request as PrismRequest, RequestHead as PrismHead};

use crate::connection::{Event, Request, Response, ResponseSent, SendError, ServerConnection};
use crate::error::Code;
use crate::message::RequestHead;
use crate::qpack::Field;
use crate::transport::Transport;

/// The application: answers a routed request.
pub trait Handler<T> {
    fn handle(&self, request: &ParsedRequest, route: RouteMatch<'_, T>) -> PrismResponse;
}

impl<T, F> Handler<T> for F
where
    F: Fn(&ParsedRequest, RouteMatch<'_, T>) -> PrismResponse,
{
    fn handle(&self, request: &ParsedRequest, route: RouteMatch<'_, T>) -> PrismResponse {
        self(request, route)
    }
}

/// The protocol-independent dispatch: target decoding, middleware, routing,
/// handler. Errors become the same responses on every protocol.
pub fn answer<T>(
    service: &Service<T>,
    handler: &impl Handler<T>,
    request: PrismRequest,
    limits: TargetLimits,
) -> PrismResponse {
    let parsed = match ParsedRequest::new(request, limits) {
        Ok(p) => p,
        Err(_) => return PrismResponse::new(400, Vec::new()),
    };
    match service.dispatch(&parsed) {
        Ok(Dispatch::Middleware { response, .. }) => response,
        Ok(Dispatch::Route(found)) => handler.handle(&parsed, found),
        Ok(Dispatch::MethodNotAllowed(allowed)) => method_not_allowed(&allowed),
        Ok(Dispatch::NotFound) => PrismResponse::new(404, Vec::new()),
        Err(_) => PrismResponse::new(500, Vec::new()),
    }
}

/// Why an HTTP/3 request cannot become a Prism request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeError {
    /// A non-UTF-8 field name (cannot happen after validation).
    FieldName,
}

/// The Prism view of an HTTP/3 request: `:path` is the target and
/// `:authority` becomes the `host` field (first), as HTTP/1.1 carries it.
pub fn to_prism(head: &RequestHead, body: Vec<u8>) -> Result<PrismRequest, BridgeError> {
    let mut headers = Vec::with_capacity(head.fields.len() + 1);
    if let Some(a) = &head.authority {
        headers.push(Header {
            name: "host".into(),
            value: a.as_bytes().to_vec(),
        });
    }
    for f in &head.fields {
        if f.name == b"host" {
            continue;
        }
        let name = String::from_utf8(f.name.clone()).map_err(|_| BridgeError::FieldName)?;
        headers.push(Header {
            name,
            value: f.value.clone(),
        });
    }
    Ok(PrismRequest {
        head: PrismHead {
            method: head.method.clone(),
            target: head.path.clone(),
            headers,
            content_length: body.len(),
        },
        body,
    })
}

/// The HTTP/3 form of a Prism response. Field names are lowercased;
/// HTTP/1.1 connection-management fields (and content-length, which the
/// HTTP/3 layer computes) are dropped. `close` is reported separately: on
/// HTTP/3 it starts a graceful shutdown (GOAWAY) instead.
pub fn from_prism(response: PrismResponse) -> (Response, bool) {
    let fields = response
        .headers
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.to_ascii_lowercase();
            match name.as_str() {
                "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding"
                | "upgrade" | "content-length" => None,
                _ => Some(Field::new(name, value)),
            }
        })
        .collect();
    (
        Response {
            status: response.status,
            fields,
            body: response.body,
        },
        response.close,
    )
}

/// What one [`serve`] call did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServeReport {
    pub answered: u64,
    pub failed_requests: u64,
    /// Responses that could not be sent (the stream was reset instead).
    pub reset: u64,
    pub goaway: bool,
}

/// Polls the connection and answers every complete request through
/// [`answer`]. Each request's responder is discharged on every path: by
/// the response, or by a reset when the response cannot be sent.
pub fn serve<T, X: Transport>(
    conn: &mut ServerConnection,
    t: &mut X,
    service: &Service<T>,
    handler: &impl Handler<T>,
    limits: TargetLimits,
) -> ServeReport {
    let mut report = ServeReport::default();
    conn.poll(t);
    while let Some(event) = conn.poll_event() {
        match event {
            Event::Request(request) => {
                let Request {
                    head,
                    body,
                    responder,
                    ..
                } = *request;
                let reply = match to_prism(&head, body) {
                    Ok(r) => answer(service, handler, r, limits),
                    Err(_) => PrismResponse::new(400, Vec::new()),
                };
                let (response, close) = from_prism(reply);
                let sent: ResponseSent = match responder.respond(conn, t, response) {
                    Ok(sent) => sent,
                    Err((responder, SendError::InvalidResponse(_))) => {
                        report.reset += 1;
                        responder.reset(conn, t, Code::INTERNAL_ERROR)
                    }
                    Err((responder, _)) => {
                        report.reset += 1;
                        responder.reset(conn, t, Code::INTERNAL_ERROR)
                    }
                };
                if sent.reset.is_none() {
                    report.answered += 1;
                }
                if close {
                    conn.shutdown(t);
                    report.goaway = true;
                }
            }
            Event::RequestFailed { .. } => report.failed_requests += 1,
            Event::ResponseAbandoned { .. } | Event::GoAway(_) => {}
        }
    }
    conn.poll(t);
    report
}
