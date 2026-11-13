//! Stdio session tests for Lens: framed JSON-RPC through `serve_with` over
//! in-memory pipes, with the server on its own thread like a real process.
use super::{LensConfig, serve_with};
use serde_json::{Value, json};
use std::{
    io::{self, BufReader, Read, Write},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

struct PipeReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}
impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.buf.len() {
            match self.rx.recv() {
                Ok(bytes) => {
                    self.buf = bytes;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
struct PipeWriter(Sender<Vec<u8>>);
impl Write for PipeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .send(bytes.to_vec())
            .map_err(|_| io::Error::other("client closed"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A client connected to a server thread.
struct Client {
    to_server: Option<Sender<Vec<u8>>>,
    from_server: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    server: Option<JoinHandle<Result<(), String>>>,
    next_id: i64,
}
impl Client {
    fn start(config: LensConfig) -> Self {
        let (to_server, server_in) = mpsc::channel();
        let (server_out, from_server) = mpsc::channel();
        let server = thread::spawn(move || {
            serve_with(
                BufReader::new(PipeReader {
                    rx: server_in,
                    buf: Vec::new(),
                    pos: 0,
                }),
                PipeWriter(server_out),
                config,
            )
        });
        let mut client = Self {
            to_server: Some(to_server),
            from_server,
            pending: Vec::new(),
            server: Some(server),
            next_id: 100,
        };
        let id = client.request(
            "initialize",
            json!({"capabilities":{
            "textDocument":{"publishDiagnostics":{"relatedInformation":true}}}}),
        );
        let init = client.response(id);
        assert!(init["result"]["capabilities"]["completionProvider"].is_object());
        assert_eq!(init["result"]["capabilities"]["inlayHintProvider"], true);
        client.notify("initialized", json!({}));
        client
    }
    fn send(&mut self, message: Value) {
        let body = serde_json::to_vec(&message).unwrap();
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend(body);
        self.to_server.as_ref().unwrap().send(frame).unwrap();
    }
    fn request(&mut self, method: &str, params: Value) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        if params.is_null() {
            self.send(json!({"jsonrpc":"2.0","id":id,"method":method}));
        } else {
            self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        }
        id
    }
    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }
    /// Next server message, or None after `timeout`.
    fn next_within(&mut self, timeout: Duration) -> Option<Value> {
        loop {
            if let Some(end) = self.pending.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = std::str::from_utf8(&self.pending[..end]).unwrap();
                let length: usize = header
                    .strip_prefix("Content-Length: ")
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                if self.pending.len() >= end + 4 + length {
                    let body: Vec<u8> = self
                        .pending
                        .drain(..end + 4 + length)
                        .skip(end + 4)
                        .collect();
                    return Some(serde_json::from_slice(&body).unwrap());
                }
            }
            match self.from_server.recv_timeout(timeout) {
                Ok(bytes) => self.pending.extend(bytes),
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
    fn next(&mut self) -> Value {
        self.next_within(Duration::from_secs(60))
            .expect("server message within 60 seconds")
    }
    fn response(&mut self, id: i64) -> Value {
        loop {
            let message = self.next();
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
        }
    }
    fn diagnostics(&mut self, version: i32) -> Value {
        loop {
            let message = self.next();
            if message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["version"] == version
            {
                return message["params"].clone();
            }
        }
    }
    fn open(&mut self, uri: &str, text: &str) -> Value {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"vesper","version":1,"text":text}}),
        );
        self.diagnostics(1)
    }
    fn replace(&mut self, uri: &str, version: i32, text: &str) -> Value {
        self.notify(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":text}]}),
        );
        self.diagnostics(version)
    }
    fn at(&mut self, method: &str, uri: &str, line: u32, character: u32) -> Value {
        let id = self.request(
            method,
            json!({"textDocument":{"uri":uri},"position":{"line":line,"character":character}}),
        );
        self.response(id)
    }
    fn close(mut self) {
        let id = self.request("shutdown", Value::Null);
        self.response(id);
        self.send(json!({"jsonrpc":"2.0","method":"exit"}));
        self.to_server.take();
        let result = self.server.take().unwrap().join().unwrap();
        assert_eq!(result, Ok(()));
    }
}

fn position_of(text: &str, needle: &str) -> (u32, u32) {
    let offset = text.find(needle).expect("needle present");
    let line = text[..offset].matches('\n').count() as u32;
    let column = (offset - text[..offset].rfind('\n').map_or(0, |i| i + 1)) as u32;
    (line, column)
}

const PREFIX: &str = "effect Storage {
    fn write(slot: u64) -> Unit;
}

linear struct WriteTxn {
    wal_slot: u64
}

impl WriteTxn {
    fn commit(sink self) -> u64 {
        let WriteTxn {
            wal_slot
        }
        = self;
        wal_slot
    }
    fn abort(sink self) {
        let WriteTxn {
            wal_slot
        }
        = self;
    }
    fn slot(self) -> u64 {
        self.wal_slot
    }
    fn persist(self) -> u64 eff [Storage] {
        Storage::write(self.wal_slot);
        self.wal_slot
    }
}

fn open_txn(slot: u64) -> WriteTxn {
    WriteTxn {
        wal_slot: slot
    }
}
";

fn with_main(body: &str) -> String {
    format!("{PREFIX}\nfn main() -> u64 {{\n{body}}}\n")
}

fn codes(params: &Value) -> Vec<String> {
    params["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[test]
fn poisoned_region_reports_each_binding_once_without_cascading() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:poison.ves";
    let damaged = with_main(
        "    let tx = open_txn(1);\n    let other = open_txn(2);\n    let total = tx.slot();\n    tx.\n",
    );
    let published = client.open(uri, &damaged);
    let codes1 = codes(&published);
    assert!(codes1.contains(&"E0002".to_owned()), "{published}");
    assert!(
        !codes1.iter().any(|c| c == "E0302" || c == "E0402"),
        "{published}"
    );
    let poison: Vec<_> = published["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "VESPER_POISONED_RECOVERY")
        .collect();
    assert_eq!(poison.len(), 2, "{published}");
    let names: Vec<_> = poison
        .iter()
        .map(|d| d["data"]["binding"].clone())
        .collect();
    assert_eq!(names, [json!("tx"), json!("other")]);
    for (diagnostic, needle) in poison.iter().zip(["tx = ", "other = "]) {
        assert_eq!(diagnostic["severity"], 3);
        let (line, column) = position_of(&damaged, needle);
        assert_eq!(
            diagnostic["range"]["start"],
            json!({"line":line,"character":column})
        );
        let related = &diagnostic["relatedInformation"][0];
        let (line, _) = position_of(&damaged, "    tx.\n");
        assert_eq!(related["location"]["range"]["start"]["line"], line);
    }
    let lens = &published["vesper"]["recovery"]["lens"];
    assert_eq!(lens["scope"], "recovered_prefix");
    assert!(
        lens["absorbed_scope_exit_diagnostics"].as_u64().unwrap() >= 2,
        "{lens}"
    );

    // More unfinished code in the same region still reports each poison once.
    let worse = with_main(
        "    let tx = open_txn(1);\n    let other = open_txn(2);\n    let total = tx.slot();\n    tx.\n    other.\n    let\n",
    );
    let published = client.replace(uri, 2, &worse);
    let poison = codes(&published)
        .iter()
        .filter(|c| *c == "VESPER_POISONED_RECOVERY")
        .count();
    assert_eq!(poison, 2, "{published}");
    assert!(!codes(&published).iter().any(|c| c == "E0302"));

    // A binding consumed before the damage is not poisoned.
    let partial = with_main(
        "    let tx = open_txn(1);\n    let other = open_txn(2);\n    let total = tx.commit();\n    other.\n",
    );
    let published = client.replace(uri, 3, &partial);
    let poison: Vec<_> = published["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "VESPER_POISONED_RECOVERY")
        .map(|d| d["data"]["binding"].clone())
        .collect();
    assert_eq!(poison, [json!("other")], "{published}");

    // Finishing the code clears the poison and the checker is authoritative.
    let finished = with_main(
        "    let tx = open_txn(1);\n    let other = open_txn(2);\n    let total = tx.commit();\n    other.abort();\n    total\n",
    );
    let published = client.replace(uri, 4, &finished);
    assert_eq!(published["diagnostics"], json!([]), "{published}");
    assert_eq!(published["vesper"]["status"], "checked");

    // A real leak in finished code is still reported by the checker.
    let leak = with_main("    let tx = open_txn(1);\n    7\n");
    let published = client.replace(uri, 5, &leak);
    assert!(
        codes(&published).contains(&"E0302".to_owned()),
        "{published}"
    );
    assert!(!codes(&published).contains(&"VESPER_POISONED_RECOVERY".to_owned()));
    client.close();
}

#[test]
fn completion_lists_consuming_calls_first_for_a_live_linear_receiver() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:complete.ves";
    let text = with_main("    let tx = open_txn(1);\n    let other = open_txn(2);\n    tx.\n");
    client.open(uri, &text);
    let (line, column) = position_of(&text, "tx.\n");
    let response = client.at("textDocument/completion", uri, line, column + 3);
    let items = response["result"]["items"].as_array().unwrap();
    let labels: Vec<_> = items.iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["abort", "commit", "slot", "persist"], "{response}");
    let tiers: Vec<_> = items
        .iter()
        .map(|i| i["data"]["vesper"]["tier"].as_str().unwrap())
        .collect();
    assert_eq!(
        tiers,
        [
            "discharges_live_linear",
            "discharges_live_linear",
            "valid_frame_compatible",
            "blocked_by_frame"
        ]
    );
    assert_eq!(
        items[0]["labelDetails"]["description"],
        "discharges linear obligation"
    );
    assert_eq!(items[0]["detail"], "fn abort(sink self)");
    assert!(
        items[3]["labelDetails"]["description"]
            .as_str()
            .unwrap()
            .contains("abortable effect Storage")
    );
    assert_eq!(items[0]["data"]["vesper"]["scope"], "recovered_prefix");
    let mut sorted = items.clone();
    sorted.sort_by_key(|i| i["sortText"].as_str().unwrap().to_owned());
    assert_eq!(&sorted, items);

    // After the receiver is consumed it is no longer a live obligation.
    let text = with_main("    let tx = open_txn(1);\n    let n = tx.commit();\n    tx.\n");
    client.replace(uri, 2, &text);
    let (line, column) = position_of(&text, "tx.\n");
    let response = client.at("textDocument/completion", uri, line, column + 3);
    let items = response["result"]["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .all(|i| i["data"]["vesper"]["receiver_live_linear"] == false)
    );
    assert!(
        items
            .iter()
            .all(|i| i["data"]["vesper"]["tier"] != "discharges_live_linear")
    );

    // No receiver, or an unknown receiver, gives an empty list, never a guess.
    let response = client.at("textDocument/completion", uri, 0, 0);
    assert_eq!(response["result"]["items"], json!([]));
    let text = with_main("    let tx = open_txn(1);\n    nope.\n");
    client.replace(uri, 3, &text);
    let (line, column) = position_of(&text, "nope.");
    let response = client.at("textDocument/completion", uri, line, column + 5);
    assert_eq!(response["result"]["items"], json!([]));
    client.close();
}

#[test]
fn hover_and_inlay_hints_show_outstanding_obligations() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:hover.ves";
    let text = with_main(
        "    let tx = open_txn(1);\n    let other = open_txn(2);\n    other.abort();\n    tx.commit()\n",
    );
    let published = client.open(uri, &text);
    assert_eq!(published["vesper"]["status"], "checked", "{published}");
    let (line, column) = position_of(&text, "other.abort");
    let hover = client.at("textDocument/hover", uri, line, column);
    let value = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(
        value.contains("outstanding linear obligations here (checked)"),
        "{value}"
    );
    assert!(value.contains("- tx: WriteTxn"), "{value}");
    assert!(value.contains("- other: WriteTxn"), "{value}");
    assert!(value.contains("tx.abort(), tx.commit()"), "{value}");
    let (line, column) = position_of(&text, "tx.commit()");
    let hover = client.at("textDocument/hover", uri, line, column);
    let value = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(value.contains("- tx: WriteTxn"), "{value}");
    assert!(!value.contains("- other"), "{value}");

    let id = client.request(
        "textDocument/inlayHint",
        json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},
            "end":{"line":200,"character":0}}}),
    );
    let response = client.response(id);
    let hints: Vec<_> = response["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["data"]["vesper"]["binding"] != "self")
        .map(|h| (h["data"]["vesper"]["binding"].clone(), h["label"].clone()))
        .collect();
    let (commit_line, _) = position_of(&text, "tx.commit()");
    let (abort_line, _) = position_of(&text, "other.abort");
    assert_eq!(
        hints,
        [
            (
                json!("tx"),
                json!(format!("linear: consumed on line {}", commit_line + 1))
            ),
            (
                json!("other"),
                json!(format!("linear: consumed on line {}", abort_line + 1))
            )
        ],
        "{response}"
    );

    // In a damaged buffer navigation's typed refusal stands, and it carries
    // the repaired prefix's obligations as data.
    let text = with_main("    let tx = open_txn(1);\n    tx.\n");
    client.replace(uri, 2, &text);
    let (line, column) = position_of(&text, "tx.\n");
    let hover = client.at("textDocument/hover", uri, line, column);
    assert_eq!(hover["error"]["code"], -32003, "{hover}");
    let value = hover["error"]["data"]["lens_obligations"].as_str().unwrap();
    assert!(value.contains("(recovered_prefix)"), "{value}");
    assert!(value.contains("- tx: WriteTxn"), "{value}");
    client.close();
}

#[test]
fn closure_sink_parameters_and_function_value_calls_follow_checker_modes() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:closure.ves";
    let text = with_main(
        "    let f: fn @copy (sink WriteTxn) -> u64 = | sink t: WriteTxn | t.commit();\n    let tx = open_txn(1);\n    f(tx)\n",
    );
    let published = client.open(uri, &text);
    assert_eq!(published["vesper"]["status"], "checked", "{published}");
    // The closure's sink parameter is an owned obligation inside its body.
    let (line, column) = position_of(&text, "t.commit()");
    let hover = client.at("textDocument/hover", uri, line, column);
    let value = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(value.contains("- t: WriteTxn"), "{value}");
    // Passing `tx` to the function value's sink parameter consumes it.
    let id = client.request(
        "textDocument/inlayHint",
        json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},
            "end":{"line":200,"character":0}}}),
    );
    let response = client.response(id);
    let (call_line, _) = position_of(&text, "f(tx)");
    let labels: Vec<_> = response["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["data"]["vesper"]["binding"] == "tx" || h["data"]["vesper"]["binding"] == "t")
        .map(|h| (h["data"]["vesper"]["binding"].clone(), h["label"].clone()))
        .collect();
    assert_eq!(
        labels,
        [
            (
                json!("t"),
                json!(format!("linear: consumed on line {}", line + 1))
            ),
            (
                json!("tx"),
                json!(format!("linear: consumed on line {}", call_line + 1))
            )
        ],
        "{response}"
    );
    client.close();
}

#[cfg(unix)]
mod fake_lean {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    pub(super) struct Toolchain {
        pub dir: PathBuf,
    }
    impl Toolchain {
        /// A stand-in Lean whose compile step sleeps when `slow` is set, and
        /// an auditor that reports an accepted empty-axiom replay.
        pub(super) fn new(name: &str, slow: bool, accept: bool) -> Self {
            Self::with_compile(
                name,
                if slow { "exec /bin/sleep 30" } else { "exit 0" },
                accept,
            )
        }
        /// `compile` is the shell body run for a compile invocation; `$DIR`
        /// is the toolchain directory.
        pub(super) fn with_compile(name: &str, compile: &str, accept: bool) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("vesper-lens-fake-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("bin")).unwrap();
            let compile = compile.replace("$DIR", &dir.display().to_string());
            let lean = format!(
                "#!/bin/sh\ncase \"$1\" in\n--print-prefix) echo '{}' ;;\n--version) echo 'Lean (version 4.24.0, fake)' ;;\n*) {compile} ;;\nesac\n",
                dir.display()
            );
            let report = if accept {
                r#"{"accepted":true,"kernel_replayed":true,"forbidden_axioms":[],"checked_constants":3}"#
            } else {
                r#"{"accepted":false,"kernel_replayed":true,"forbidden_axioms":["sorryAx"],"checked_constants":3}"#
            };
            let auditor = format!("#!/bin/sh\necho '{report}'\n");
            for (path, text) in [(dir.join("bin/lean"), lean), (dir.join("auditor"), auditor)] {
                fs::write(&path, text).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self { dir }
        }
        pub(super) fn config(&self) -> LensConfig {
            LensConfig {
                lean: self.dir.join("bin/lean").display().to_string(),
                auditor: Some(self.dir.join("auditor")),
                proof_timeout: 60,
            }
        }
    }
    impl Drop for Toolchain {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

const PROVABLE: &str = "fn double(x: u64) -> u64 {
    x
}

fn main() -> u64 {
    double(21)
}
";

fn lens_status(client: &mut Client, uri: &str, function: &str) -> (String, String) {
    let id = client.request("textDocument/codeLens", json!({"textDocument":{"uri":uri}}));
    let response = client.response(id);
    let lens = response["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["data"]["vesper"]["function"] == function)
        .cloned()
        .unwrap_or_else(|| panic!("no lens for {function}: {response}"));
    (
        lens["data"]["vesper"]["status"]
            .as_str()
            .unwrap()
            .to_owned(),
        lens["command"]["title"].as_str().unwrap().to_owned(),
    )
}

fn audit_args(uri: &str, function: &str) -> Value {
    json!({"command":"vesper.proofAudit","arguments":[{"uri":uri,"version":1,"function":function}]})
}

#[cfg(unix)]
#[test]
fn proof_lens_cancellation_answers_once_and_keeps_not_attempted() {
    let toolchain = fake_lean::Toolchain::new("cancel", true, true);
    let mut client = Client::start(toolchain.config());
    let uri = "untitled:proof.ves";
    client.open(uri, PROVABLE);
    assert_eq!(lens_status(&mut client, uri, "double").0, "not_attempted");
    let id = client.request("workspace/executeCommand", audit_args(uri, "double"));
    // The server keeps answering while the proof job runs.
    let (status, _) = lens_status(&mut client, uri, "double");
    assert_eq!(status, "not_attempted");
    client.notify("$/cancelRequest", json!({"id": id}));
    let response = client.response(id);
    assert_eq!(response["error"]["code"], -32800, "{response}");
    // A second cancel and the killed job's completion send nothing more.
    client.notify("$/cancelRequest", json!({"id": id}));
    while let Some(message) = client.next_within(Duration::from_millis(500)) {
        assert_ne!(message.get("id"), Some(&json!(id)), "{message}");
    }
    assert_eq!(lens_status(&mut client, uri, "double").0, "not_attempted");
    // Unknown IDs are ignored without a response.
    client.notify("$/cancelRequest", json!({"id": 987654}));
    assert!(client.next_within(Duration::from_millis(200)).is_none());
    client.close();
}

#[cfg(unix)]
#[test]
fn proof_lens_records_proved_failed_and_stale_requests() {
    let toolchain = fake_lean::Toolchain::new("accept", false, true);
    let mut client = Client::start(toolchain.config());
    let uri = "untitled:proof.ves";
    client.open(uri, PROVABLE);
    let id = client.request("workspace/executeCommand", audit_args(uri, "double"));
    let response = client.response(id);
    assert_eq!(response["result"]["status"], "proved", "{response}");
    assert_eq!(response["result"]["scope"], "lean_extraction_axiom_free");
    let (status, title) = lens_status(&mut client, uri, "double");
    assert_eq!(status, "proved");
    assert!(title.starts_with("Lean: proved"), "{title}");
    assert_eq!(lens_status(&mut client, uri, "main").0, "not_attempted");

    // Unknown functions fail at extraction; stale versions are refused.
    let id = client.request("workspace/executeCommand", audit_args(uri, "missing"));
    let response = client.response(id);
    assert_eq!(response["result"]["status"], "failed");
    assert_eq!(response["result"]["stage"], "extract");
    let id = client.request(
        "workspace/executeCommand",
        json!({"command":"vesper.proofAudit","arguments":[{"uri":uri,"version":7,"function":"double"}]}),
    );
    assert_eq!(client.response(id)["error"]["code"], -32801);
    let id = client.request(
        "workspace/executeCommand",
        json!({"command":"rm","arguments":[]}),
    );
    assert_eq!(client.response(id)["error"]["code"], -32602);

    // A new source hash never inherits the old outcome.
    client.replace(uri, 2, &PROVABLE.replace("double(21)", "double(22)"));
    assert_eq!(lens_status(&mut client, uri, "double").0, "not_attempted");
    // Damaged source offers no proof lenses.
    client.replace(uri, 3, "fn double(x: u64) -> u64 {\n    x.\n}\n");
    let id = client.request("textDocument/codeLens", json!({"textDocument":{"uri":uri}}));
    assert_eq!(client.response(id)["result"], json!([]));
    client.close();

    let rejecting = fake_lean::Toolchain::new("reject", false, false);
    let mut client = Client::start(rejecting.config());
    client.open(uri, PROVABLE);
    let id = client.request("workspace/executeCommand", audit_args(uri, "double"));
    let response = client.response(id);
    assert_eq!(response["result"]["status"], "failed");
    assert_eq!(response["result"]["stage"], "kernel");
    let (status, title) = lens_status(&mut client, uri, "double");
    assert_eq!(status, "failed");
    assert!(title.contains("failed at kernel"), "{title}");
    client.close();
}

#[cfg(unix)]
#[test]
fn proof_lens_cancellation_stops_the_whole_process_tree() {
    // The compile step forks a grandchild, as `vesper proof-audit` forks Lean.
    let toolchain = fake_lean::Toolchain::with_compile(
        "tree",
        "/bin/sleep 30 & echo $! > '$DIR/grandchild'; wait",
        true,
    );
    let marker = toolchain.dir.join("grandchild");
    let mut client = Client::start(toolchain.config());
    let uri = "untitled:tree.ves";
    client.open(uri, PROVABLE);
    let id = client.request("workspace/executeCommand", audit_args(uri, "double"));
    let started = std::time::Instant::now();
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(&marker)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "grandchild never started"
        );
        thread::sleep(Duration::from_millis(10));
    };
    client.notify("$/cancelRequest", json!({"id": id}));
    assert_eq!(client.response(id)["error"]["code"], -32800);
    let alive = || {
        std::process::Command::new("/bin/sh")
            .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
            .status()
            .unwrap()
            .success()
    };
    let started = std::time::Instant::now();
    while alive() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "grandchild {pid} survived cancel"
        );
        thread::sleep(Duration::from_millis(20));
    }
    client.close();
}

/// Real Lean 4.24.0 and the real `vesper proof-audit`. Needs `VESPER_LEAN`
/// and `VESPER_LENS_AUDITOR` (a built `vesper` binary); otherwise skipped.
#[test]
fn proof_lens_with_real_lean_and_auditor() {
    let (Ok(lean), Ok(auditor)) = (
        std::env::var("VESPER_LEAN"),
        std::env::var("VESPER_LENS_AUDITOR"),
    ) else {
        eprintln!("VESPER_LEAN or VESPER_LENS_AUDITOR unset; real proof lens skipped");
        return;
    };
    let mut client = Client::start(LensConfig {
        lean,
        auditor: Some(auditor.into()),
        proof_timeout: 120,
    });
    let uri = "untitled:real.ves";
    client.open(uri, PROVABLE);
    let id = client.request("workspace/executeCommand", audit_args(uri, "double"));
    let response = client.response(id);
    assert_eq!(response["result"]["status"], "proved", "{response}");
    assert_eq!(lens_status(&mut client, uri, "double").0, "proved");
    client.close();
}
