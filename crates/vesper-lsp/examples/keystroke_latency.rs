//! Keystroke-to-diagnostics latency through a real stdio language server.
//!
//! ```text
//! cargo run --release -p vesper-lsp --example keystroke_latency -- \
//!     target/release/vesper FIXTURE.ves [FIXTURE.ves ...] [--iterations N]
//! ```
//!
//! For each fixture the client opens the buffer, then repeats two kinds of
//! single-change edits inside the last function body, each followed by the
//! edit that restores the original text:
//!
//! * `valid`: insert one space (the file stays syntax- and type-correct);
//! * `damaging`: insert `let ` before the last body line, which makes the
//!   function unparsable and exercises recovery, the repaired prefix and
//!   PoisonedRecovery.
//!
//! Latency is the wall time from writing the `didChange` frame to reading the
//! `publishDiagnostics` notification carrying that version. After every
//! valid edit the client also times a whole-document `textDocument/inlayHint`
//! request (`lens`), which builds the Lens view of the new text under Lens's
//! own analysis profile, and a `textDocument/hover` at the edited position
//! (`hover`), which needs the checked snapshot of the new text. It includes pipe
//! I/O and JSON framing on both sides; it is not a measurement of any single
//! compiler layer. Nothing is reported that was not measured.
use serde_json::{Value, json};
use std::{
    io::{BufReader, Write},
    process::{Command, Stdio},
    time::Instant,
};
use vesper_lsp::{Envelope, Id, ProtocolLimits, encode_frame, read_message};

fn frame(message: Value) -> Vec<u8> {
    let envelope = match (message.get("id"), message.get("method")) {
        (Some(id), Some(method)) => Envelope::Request {
            id: Id::Integer(id.as_i64().unwrap() as i32),
            method: method.as_str().unwrap().into(),
            params: message.get("params").cloned(),
        },
        (None, Some(method)) => Envelope::Notification {
            method: method.as_str().unwrap().into(),
            params: message.get("params").cloned(),
        },
        _ => unreachable!("client sends requests and notifications only"),
    };
    encode_frame(&envelope, ProtocolLimits::default()).expect("client frame")
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn summary(name: &str, samples: &mut [f64]) -> String {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    format!(
        "{name:<9} n={:<4} min={:.3} median={:.3} p95={:.3} p99={:.3} max={:.3} ms",
        samples.len(),
        samples[0],
        percentile(samples, 0.5),
        percentile(samples, 0.95),
        percentile(samples, 0.99),
        samples[samples.len() - 1]
    )
}

/// Byte offset and (line, UTF-16 column) of the first non-blank character on
/// the last non-empty line before the final closing brace.
fn last_body_line(text: &str) -> Option<(usize, u32, u32)> {
    let close = text.rfind('}')?;
    let before = &text[..close];
    let mut line_start = before.trim_end().rfind('\n')? + 1;
    let line_text = &text[line_start..close];
    let indent = line_text.len() - line_text.trim_start().len();
    line_start += indent;
    let line = text[..line_start].matches('\n').count() as u32;
    let column = text[text[..line_start].rfind('\n').map_or(0, |i| i + 1)..line_start]
        .encode_utf16()
        .count() as u32;
    Some((line_start, line, column))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let server = args
        .next()
        .expect("usage: keystroke_latency SERVER FIXTURE.ves...");
    let mut fixtures = Vec::new();
    let mut iterations = 50usize;
    while let Some(arg) = args.next() {
        if arg == "--iterations" {
            iterations = args
                .next()
                .and_then(|n| n.parse().ok())
                .expect("--iterations N");
        } else {
            fixtures.push(arg);
        }
    }
    let mut child = Command::new(&server)
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start server");
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let limits = ProtocolLimits::default();
    let mut wait = |predicate: &dyn Fn(&Envelope) -> bool| loop {
        let message = read_message(&mut output, limits)
            .expect("server frame")
            .expect("server open");
        if predicate(&message) {
            return message;
        }
    };
    input
        .write_all(&frame(
            json!({"id":1,"method":"initialize","params":{"capabilities":{}}}),
        ))
        .unwrap();
    wait(&|m| matches!(m, Envelope::Response { .. }));
    input
        .write_all(&frame(json!({"method":"initialized","params":{}})))
        .unwrap();
    println!("server: {server}; iterations per edit kind: {iterations}");
    for (n, fixture) in fixtures.iter().enumerate() {
        let text = std::fs::read_to_string(fixture).expect("read fixture");
        let uri = format!("untitled:bench{n}.ves");
        let Some((_, line, column)) = last_body_line(&text) else {
            println!("{fixture}: no function body to edit; skipped");
            continue;
        };
        input
            .write_all(&frame(json!({"method":"textDocument/didOpen","params":{
                "textDocument":{"uri":uri,"languageId":"vesper","version":1,"text":text}}})))
            .unwrap();
        let mut status = String::new();
        let opened = wait(&|m| {
            matches!(m, Envelope::Notification { method, params: Some(p) }
            if method == "textDocument/publishDiagnostics" && p["uri"] == uri.as_str())
        });
        if let Envelope::Notification {
            params: Some(p), ..
        } = opened
        {
            status = p["vesper"]["status"].as_str().unwrap_or("").to_owned();
        }
        let mut version = 1;
        let mut valid = Vec::new();
        let mut damaging = Vec::new();
        let mut damaged_status = String::new();
        let mut lens = Vec::new();
        let mut lens_scope = String::new();
        let mut hover = Vec::new();
        let mut hover_status = String::new();
        let mut request_id = 1000;
        for _ in 0..iterations {
            for (insert, samples) in [(" ", &mut valid), ("let ", &mut damaging)] {
                let at = json!({"line":line,"character":column});
                let end = json!({"line":line,"character":column + insert.len() as u32});
                for change in [
                    json!({"range":{"start":at,"end":at},"text":insert}),
                    json!({"range":{"start":at,"end":end},"text":""}),
                ] {
                    version += 1;
                    let started = Instant::now();
                    input
                        .write_all(&frame(json!({"method":"textDocument/didChange","params":{
                            "textDocument":{"uri":uri,"version":version},"contentChanges":[change]}})))
                        .unwrap();
                    let published = wait(&|m| {
                        matches!(m, Envelope::Notification { method, params: Some(p) }
                        if method == "textDocument/publishDiagnostics" && p["uri"] == uri.as_str()
                            && p["version"] == version)
                    });
                    samples.push(started.elapsed().as_secs_f64() * 1e3);
                    if insert == " " {
                        request_id += 1;
                        let started = Instant::now();
                        input
                            .write_all(&frame(
                                json!({"id":request_id,"method":"textDocument/inlayHint",
                                "params":{"textDocument":{"uri":uri},"range":{
                                    "start":{"line":0,"character":0},
                                    "end":{"line":100000,"character":0}}}}),
                            ))
                            .unwrap();
                        let answer = wait(&|m| {
                            matches!(m, Envelope::Response { id: Some(Id::Integer(id)), .. }
                                if *id == request_id)
                        });
                        lens.push(started.elapsed().as_secs_f64() * 1e3);
                        request_id += 1;
                        let started = Instant::now();
                        input
                            .write_all(&frame(
                                json!({"id":request_id,"method":"textDocument/hover",
                                "params":{"textDocument":{"uri":uri},
                                    "position":{"line":line,"character":column}}}),
                            ))
                            .unwrap();
                        let hovered = wait(&|m| {
                            matches!(m, Envelope::Response { id: Some(Id::Integer(id)), .. }
                                if *id == request_id)
                        });
                        hover.push(started.elapsed().as_secs_f64() * 1e3);
                        if hover_status.is_empty() {
                            hover_status = match hovered {
                                Envelope::Response {
                                    payload: vesper_lsp::ResponsePayload::Result(Value::Null),
                                    ..
                                } => "no hover at the edit".into(),
                                Envelope::Response {
                                    payload: vesper_lsp::ResponsePayload::Result(_),
                                    ..
                                } => "hover".into(),
                                Envelope::Response {
                                    payload: vesper_lsp::ResponsePayload::Error { code, .. },
                                    ..
                                } => format!("error {code}"),
                                _ => "?".into(),
                            };
                        }
                        if lens_scope.is_empty() {
                            lens_scope = match answer {
                                Envelope::Response {
                                    payload:
                                        vesper_lsp::ResponsePayload::Result(Value::Array(hints)),
                                    ..
                                } => format!(
                                    "{} hints, scope {}",
                                    hints.len(),
                                    hints
                                        .first()
                                        .and_then(|h| h["data"]["vesper"]["scope"].as_str())
                                        .unwrap_or("none")
                                ),
                                Envelope::Response {
                                    payload: vesper_lsp::ResponsePayload::Error { data, .. },
                                    ..
                                } => format!(
                                    "unavailable: {}",
                                    data.as_ref()
                                        .and_then(|d| d["reason"].as_str())
                                        .unwrap_or("?")
                                ),
                                _ => "?".into(),
                            };
                        }
                    }
                    if insert == "let "
                        && let Envelope::Notification {
                            params: Some(p), ..
                        } = &published
                        && damaged_status.is_empty()
                    {
                        let lens = &p["vesper"]["recovery"]["lens"];
                        damaged_status = format!(
                            "{} ({} diagnostics, lens {}{})",
                            p["vesper"]["status"].as_str().unwrap_or(""),
                            p["diagnostics"].as_array().map_or(0, Vec::len),
                            lens["status"].as_str().unwrap_or("n/a"),
                            lens["reason"]
                                .as_str()
                                .map(|r| format!(": {r}"))
                                .unwrap_or_default()
                        );
                    }
                }
            }
        }
        println!(
            "{fixture}: {} bytes, {} lines, open status {status}; damaged status {damaged_status}",
            text.len(),
            text.lines().count()
        );
        println!("  {}", summary("valid", &mut valid));
        println!("  {}", summary("damaging", &mut damaging));
        println!("  {} ({lens_scope})", summary("lens", &mut lens));
        println!("  {} ({hover_status})", summary("hover", &mut hover));
        input
            .write_all(&frame(json!({"method":"textDocument/didClose","params":{
                "textDocument":{"uri":uri}}})))
            .unwrap();
    }
    input
        .write_all(&frame(json!({"id":2,"method":"shutdown"})))
        .unwrap();
    wait(&|m| matches!(m, Envelope::Response { .. }));
    input.write_all(&frame(json!({"method":"exit"}))).unwrap();
    child.wait().unwrap();
}
