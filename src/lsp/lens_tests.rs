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
        Self::start_with(
            config,
            json!({"capabilities":{
            "textDocument":{"publishDiagnostics":{"relatedInformation":true}}}}),
        )
    }
    fn start_with(config: LensConfig, initialize: Value) -> Self {
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
        let id = client.request("initialize", initialize);
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
            // Echo one theorem record per `--theorem`, with no axioms when
            // accepting and `sorryAx` when rejecting.
            let (accepted, axioms) = if accept {
                ("true", "[]")
            } else {
                ("false", "[\\\"sorryAx\\\"]")
            };
            let auditor = format!(
                "#!/bin/sh\necho \"$@\" >> '{dir}/audits'\nlist=''\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --theorem ]; then list=\"$list{{\\\"name\\\":\\\"$2\\\",\\\"axioms\\\":{axioms}}},\"; shift; fi\n  shift\ndone\nlist=${{list%,}}\necho \"{{\\\"accepted\\\":{accepted},\\\"kernel_replayed\\\":true,\\\"forbidden_axioms\\\":[],\\\"checked_constants\\\":3,\\\"theorems\\\":[$list]}}\"\n",
                dir = dir.display()
            );
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
                cache: None,
                background: None,
                ..LensConfig::default()
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
fn process_state(pid: u32) -> io::Result<Option<u8>> {
    // Both Linux and BSD/macOS ps expose the leading process state through
    // stat. On Linux, kill -0 also succeeds for zombies, which have already
    // terminated even when their new parent has not yet reaped them.
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()?;
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(None);
        }
        return Err(io::Error::other(format!(
            "ps failed for {pid}: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut rows = text.lines().map(str::trim).filter(|row| !row.is_empty());
    let state = rows.next().and_then(|row| row.as_bytes().first().copied());
    if rows.next().is_some() {
        return Err(io::Error::other("ps returned multiple process rows"));
    }
    Ok(state)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> io::Result<bool> {
    // Every other reported state remains alive, including sleeping S and
    // stopped T/t. An unexpected state must not make cancellation pass.
    Ok(process_state(pid)?.is_some_and(|state| state != b'Z'))
}

#[cfg(unix)]
#[test]
fn proof_lens_process_state_distinguishes_live_stopped_and_zombie() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = ChildGuard(
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap(),
    );
    let pid = child.0.id();
    let wait_for_state = |expected| {
        let started = std::time::Instant::now();
        loop {
            let actual = process_state(pid).unwrap();
            if actual == Some(expected) {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "process {pid}: expected state {}, observed {actual:?}",
                char::from(expected)
            );
            thread::sleep(Duration::from_millis(10));
        }
    };
    wait_for_state(b'S');
    assert!(process_alive(pid).unwrap(), "sleeping child is still alive");
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-STOP", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    wait_for_state(b'T');
    assert!(process_alive(pid).unwrap(), "stopped child is still alive");
    child.0.kill().unwrap();
    // Do not wait/reap until the zombie observation: this is the distinction
    // the process-tree cancellation assertion needs to make correctly.
    wait_for_state(b'Z');
    #[cfg(target_os = "linux")]
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success(),
        "Linux kill -0 includes a dead zombie"
    );
    assert!(
        !process_alive(pid).unwrap(),
        "zombie has already terminated"
    );
    child.0.wait().unwrap();
    assert!(!process_alive(pid).unwrap(), "reaped child is absent");
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
    assert!(
        process_alive(pid).unwrap(),
        "grandchild {pid} must be live before cancellation"
    );
    client.notify("$/cancelRequest", json!({"id": id}));
    assert_eq!(client.response(id)["error"]["code"], -32800);
    let started = std::time::Instant::now();
    while process_alive(pid).unwrap() {
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
        cache: None,
        background: None,
        ..LensConfig::default()
    });
    let uri = "untitled:real.ves";
    client.open(uri, PROVABLE);
    let id = client.request("workspace/executeCommand", audit_args(uri, "double"));
    let response = client.response(id);
    assert_eq!(response["result"]["status"], "proved", "{response}");
    assert_eq!(lens_status(&mut client, uri, "double").0, "proved");
    client.close();
}

// ---------------------------------------------------------------------------
// Wave seven: deeper recovery, pattern obligations, semantic proof lenses,
// background runs and cancellation of every request.

fn lens_meta(published: &Value) -> &Value {
    &published["vesper"]["recovery"]["lens"]
}

fn poisoned(published: &Value) -> Vec<String> {
    published["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "VESPER_POISONED_RECOVERY")
        .map(|d| d["data"]["binding"].as_str().unwrap().to_owned())
        .collect()
}

fn inlay(client: &mut Client, uri: &str) -> Vec<(String, String)> {
    let id = client.request(
        "textDocument/inlayHint",
        json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},
            "end":{"line":400,"character":0}}}),
    );
    let response = client.response(id);
    response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("{response}"))
        .iter()
        .map(|h| {
            (
                h["data"]["vesper"]["binding"].as_str().unwrap().to_owned(),
                h["label"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn completion_labels(client: &mut Client, uri: &str, text: &str, needle: &str) -> Vec<String> {
    let (line, column) = position_of(text, needle);
    let response = client.at(
        "textDocument/completion",
        uri,
        line,
        column + needle.trim_end().len() as u32,
    );
    response["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{response}"))
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn nested_statement_damage_cuts_at_the_innermost_block() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:nested.ves";
    // The damage is two blocks deep; the bindings of both blocks before it
    // stay in the repaired view.
    let text = with_main(
        "    let tx = open_txn(1);\n    if true {\n        let inner = open_txn(2);\n        let n = inner.slot();\n        inner.\n    }\n    tx.commit()\n",
    );
    let published = client.open(uri, &text);
    let lens = lens_meta(&published);
    assert_eq!(lens["scope"], "recovered_prefix", "{published}");
    assert_eq!(lens["regions"][0]["kind"], "body", "{lens}");
    assert_eq!(lens["regions"][0]["depth"], 1, "{lens}");
    assert_eq!(poisoned(&published), ["tx", "inner"], "{published}");
    assert!(
        !codes(&published)
            .iter()
            .any(|c| c == "E0302" || c == "E0402")
    );
    // The inner receiver completes with its consuming calls first.
    let labels = completion_labels(&mut client, uri, &text, "inner.\n");
    assert_eq!(labels[..2], ["abort", "commit"], "{labels:?}");

    // A statement already complete inside the nested block is kept: the
    // consumption there is seen and does not poison.
    let text = with_main(
        "    let tx = open_txn(1);\n    if true {\n        let inner = open_txn(2);\n        inner.abort();\n        let x = \n    }\n    tx.commit()\n",
    );
    let published = client.replace(uri, 2, &text);
    assert_eq!(poisoned(&published), ["tx"], "{published}");
    client.close();
}

const LEDGER: &str = "linear struct Ledger {
    slot: u64
}

impl Ledger {
    fn close(sink self) -> u64 {
        let other = open_txn(3);
        self.
    }
    fn peek(self) -> u64 {
        self.slot
    }
    fn settle(sink self) -> u64 {
        let Ledger {
            slot
        }
        = self;
        slot
    }
}
";

#[test]
fn impl_method_damage_gets_a_repaired_view_that_keeps_later_methods() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:impl.ves";
    let text = format!("{PREFIX}\n{LEDGER}\nfn main() -> u64 {{\n    7\n}}\n");
    let published = client.open(uri, &text);
    let lens = lens_meta(&published);
    assert_eq!(lens["status"], "available", "{published}");
    assert_eq!(lens["regions"][0]["function"], "Ledger::close", "{lens}");
    assert_eq!(lens["regions"][0]["kind"], "body", "{lens}");
    let mut names = poisoned(&published);
    names.sort();
    assert_eq!(names, ["other", "self"], "{published}");
    // `settle`, after the damaged method, is still a known consuming call.
    let labels = completion_labels(&mut client, uri, &text, "self.\n");
    assert_eq!(labels, ["close", "settle", "peek"], "{labels:?}");
    client.close();
}

#[test]
fn struct_effect_and_header_damage_keep_the_rest_of_the_file() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:members.ves";
    let body = "fn work() -> u64 {\n    let tx = open_txn(1);\n    let n = tx.slot();\n    tx.commit()\n}\n";
    for (version, (damaged, kind)) in [
        ("struct Point {\n    x: u64,\n    y: \n}\n", "member"),
        (
            "effect Audit {\n    fn record(slot: u64) -> Unit;\n    fn \n}\n",
            "member",
        ),
        (
            "fn helper(a: u64, b: ) -> u64 {\n    let tx = open_txn(a);\n    tx.commit()\n}\n",
            "header",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let text = format!("{PREFIX}\n{damaged}\n{body}");
        let version = version as i32 + 1;
        let published = if version == 1 {
            client.open(uri, &text)
        } else {
            client.replace(uri, version, &text)
        };
        let lens = lens_meta(&published);
        assert_eq!(lens["status"], "available", "{damaged}: {published}");
        assert_eq!(lens["regions"][0]["kind"], kind, "{lens}");
        // Nothing is poisoned by a member or header repair.
        assert!(poisoned(&published).is_empty(), "{published}");
        // The complete function keeps its obligation facts.
        let hints = inlay(&mut client, uri);
        let (commit_line, _) = position_of(&text, "    tx.commit()\n}\n\0".trim_end_matches('\0'));
        assert!(
            hints
                .iter()
                .any(|(binding, label)| binding == "tx"
                    && label.starts_with("linear: consumed on line")),
            "{damaged}: {hints:?} {commit_line}"
        );
    }
    client.close();
}

#[test]
fn pattern_bound_linear_values_are_tracked_as_obligations() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:patterns.ves";
    let types = "linear struct Pair {\n    left: WriteTxn,\n    right: WriteTxn\n}\n\nlinear enum Maybe {\n    Some(WriteTxn),\n    Nothing\n}\n";
    let main = "fn main() -> u64 {\n    let pair = Pair {\n        left: open_txn(1),\n        right: open_txn(2)\n    };\n    let Pair {\n        left,\n        right\n    }\n    = pair;\n    let a = left.commit();\n    let m = Maybe::Some(right);\n    let b = match m {\n        Maybe::Some(t) => t.commit(),\n        Maybe::Nothing => 0,\n    };\n    wrapping_add(a, b)\n}\n";
    let text = format!("{PREFIX}\n{types}\n{main}");
    let published = client.open(uri, &text);
    assert_eq!(published["vesper"]["status"], "checked", "{published}");
    let hints = inlay(&mut client, uri);
    let line_of = |needle: &str| position_of(&text, needle).0 + 1;
    for (binding, needle) in [
        ("left", "left.commit()"),
        ("right", "Maybe::Some(right)"),
        ("t", "=> t.commit()"),
    ] {
        let expected = format!("linear: consumed on line {}", line_of(needle));
        assert!(
            hints.iter().any(|(b, l)| b == binding && *l == expected),
            "{binding}: {hints:?}"
        );
    }
    // Between the destructuring and its use, `right` is outstanding.
    let (line, column) = position_of(&text, "let a = left");
    let hover = client.at("textDocument/hover", uri, line, column);
    let value = hover["result"]["contents"]["value"].as_str().unwrap_or("");
    assert!(value.contains("- right: WriteTxn"), "{hover}");
    assert!(value.contains("- left: WriteTxn"), "{hover}");

    // Unfinished code inside a match arm poisons the arm's binding.
    let damaged = text.replace(
        "Maybe::Some(t) => t.commit(),",
        "Maybe::Some(t) => {\n            t.\n        }",
    );
    let published = client.replace(uri, 2, &damaged);
    assert!(
        poisoned(&published).contains(&"t".to_owned()),
        "{published}"
    );
    client.close();
}

#[test]
fn cancellation_answers_queued_and_running_requests_at_once() {
    let mut client = Client::start(LensConfig::default());
    let uri = "untitled:cancel.ves";
    let text = with_main("    let tx = open_txn(1);\n    tx.commit()\n");
    client.open(uri, &text);
    // A long request runs; a hover is queued behind it.
    let long = client.request("vesper/testBlock", json!({}));
    let (line, column) = position_of(&text, "tx.commit()");
    let queued = client.request(
        "textDocument/hover",
        json!({"textDocument":{"uri":uri},"position":{"line":line,"character":column}}),
    );
    let started = std::time::Instant::now();
    client.notify("$/cancelRequest", json!({"id": queued}));
    let response = client.response(queued);
    assert_eq!(response["error"]["code"], -32800, "{response}");
    client.notify("$/cancelRequest", json!({"id": long}));
    let response = client.response(long);
    assert_eq!(response["error"]["code"], -32800, "{response}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancellation waited for the long request"
    );
    // Neither request answers again, and the server keeps serving.
    let hover = client.at("textDocument/hover", uri, line, column);
    assert!(hover["result"].is_object(), "{hover}");
    while let Some(message) = client.next_within(Duration::from_millis(300)) {
        assert!(
            message.get("id") != Some(&json!(long)) && message.get("id") != Some(&json!(queued)),
            "{message}"
        );
    }
    client.close();
}

const SCALARS: &str = "fn inc(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    if x < 100 {
        x + 1
    } else {
        0
    }
}

fn add(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    a + b
}

fn main() -> i64 eff [Fail[ArithOverflow]] {
    add(inc(3), 4)
}
";

fn semantic_lens(client: &mut Client, uri: &str, function: &str) -> Value {
    let id = client.request("textDocument/codeLens", json!({"textDocument":{"uri":uri}}));
    let response = client.response(id);
    response["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| {
            l["data"]["vesper"]["function"] == function
                && l["data"]["vesper"]["scope"] == "lean_model_arith_fault_free"
        })
        .cloned()
        .unwrap_or_else(|| panic!("no semantic lens for {function}: {response}"))
}

fn semantic_args(uri: &str, version: i32, function: &str) -> Value {
    json!({"command":"vesper.proofAudit","arguments":[{"uri":uri,"version":version,
        "function":function,"obligation":"semantic"}]})
}

#[cfg(unix)]
#[test]
fn semantic_lenses_follow_the_model_identity() {
    let toolchain = fake_lean::Toolchain::new("semantic", false, true);
    let mut client = Client::start(toolchain.config());
    let uri = "untitled:semantic.ves";
    client.open(uri, SCALARS);
    let lens = semantic_lens(&mut client, uri, "inc");
    assert_eq!(lens["data"]["vesper"]["status"], "not_attempted", "{lens}");
    assert_eq!(lens["command"]["arguments"][0]["obligation"], "semantic");
    let id = client.request("workspace/executeCommand", semantic_args(uri, 1, "inc"));
    let response = client.response(id);
    assert_eq!(response["result"]["status"], "proved", "{response}");
    assert_eq!(response["result"]["scope"], "lean_model_arith_fault_free");
    assert!(response["result"]["model_blake3"].is_string());
    let lens = semantic_lens(&mut client, uri, "inc");
    assert_eq!(lens["data"]["vesper"]["status"], "proved", "{lens}");
    // Editing another function keeps `inc`'s model and so its outcome; the
    // extraction obligation is per source and resets.
    client.replace(uri, 2, &SCALARS.replace("add(inc(3), 4)", "add(inc(5), 4)"));
    let lens = semantic_lens(&mut client, uri, "inc");
    assert_eq!(lens["data"]["vesper"]["status"], "proved", "{lens}");
    assert_eq!(lens_status(&mut client, uri, "inc").0, "not_attempted");
    // Editing `inc` itself changes its model.
    client.replace(uri, 3, &SCALARS.replace("x < 100", "x < 101"));
    let lens = semantic_lens(&mut client, uri, "inc");
    assert_eq!(lens["data"]["vesper"]["status"], "not_attempted", "{lens}");
    // Functions outside the scalar model say why, and the command reports it.
    let text = format!("{SCALARS}\n{PREFIX}");
    client.replace(uri, 4, &text);
    let lens = semantic_lens(&mut client, uri, "open_txn");
    assert_eq!(lens["data"]["vesper"]["status"], "unavailable", "{lens}");
    assert!(
        lens["data"]["vesper"]["reason"]
            .as_str()
            .unwrap()
            .contains("outside the scalar model"),
        "{lens}"
    );
    let id = client.request(
        "workspace/executeCommand",
        semantic_args(uri, 4, "open_txn"),
    );
    let response = client.response(id);
    assert_eq!(response["result"]["status"], "failed", "{response}");
    assert_eq!(response["result"]["stage"], "model");
    let id = client.request(
        "workspace/executeCommand",
        json!({"command":"vesper.proofAudit","arguments":[{"uri":uri,"function":"inc",
            "obligation":"everything"}]}),
    );
    assert_eq!(client.response(id)["error"]["code"], -32602);
    client.close();
}

#[cfg(unix)]
#[test]
fn background_runs_are_debounced_and_publish_status() {
    let toolchain = fake_lean::Toolchain::new("background", false, true);
    let mut config = toolchain.config();
    config.background = Some(Duration::from_millis(300));
    let mut client = Client::start_with(
        config,
        json!({"capabilities":{"workspace":{"codeLens":{"refreshSupport":true}}}}),
    );
    let uri = "untitled:background.ves";
    client.open(uri, SCALARS);
    // A burst of edits inside the debounce window gives one run, for the
    // last version only.
    for version in 2..=5 {
        client.replace(
            uri,
            version,
            &SCALARS.replace("inc(3)", &format!("inc({version})")),
        );
    }
    let mut status = None;
    let mut refresh = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline && (status.is_none() || !refresh) {
        let Some(message) = client.next_within(Duration::from_secs(5)) else {
            break;
        };
        if message["method"] == "vesper/proofStatus" {
            assert!(status.is_none(), "a second background run: {message}");
            status = Some(message["params"].clone());
        }
        if message["method"] == "workspace/codeLens/refresh" {
            refresh = true;
            // The server's own request needs no meaningful answer.
            client.send(json!({"jsonrpc":"2.0","id":message["id"],"result":null}));
        }
    }
    let status = status.expect("background proof status");
    assert!(refresh, "code lens refresh was not requested");
    assert_eq!(status["version"], 5, "{status}");
    let results = status["results"].as_array().unwrap();
    for obligation in ["extraction", "semantic"] {
        assert!(
            results
                .iter()
                .any(|r| r["obligation"] == obligation && r["status"] == "proved"),
            "{obligation}: {status}"
        );
    }
    // One audit for the whole run.
    let audits = std::fs::read_to_string(toolchain.dir.join("audits")).unwrap();
    assert_eq!(audits.lines().count(), 1, "{audits}");
    assert_eq!(lens_status(&mut client, uri, "inc").0, "proved");
    let lens = semantic_lens(&mut client, uri, "add");
    assert_eq!(lens["data"]["vesper"]["status"], "proved", "{lens}");
    // Nothing more runs while the buffer is unchanged.
    assert!(
        client
            .next_within(Duration::from_millis(800))
            .is_none_or(|m| m["method"] != "vesper/proofStatus")
    );
    client.close();
}

#[cfg(unix)]
#[test]
fn an_edit_cancels_the_running_background_job() {
    let toolchain = fake_lean::Toolchain::with_compile(
        "background-cancel",
        "/bin/sleep 30 & echo $! >> '$DIR/children'; wait",
        true,
    );
    let mut config = toolchain.config();
    config.background = Some(Duration::from_millis(50));
    let mut client = Client::start(config);
    let uri = "untitled:background-cancel.ves";
    client.open(uri, SCALARS);
    let marker = toolchain.dir.join("children");
    let started = std::time::Instant::now();
    let first = loop {
        if let Ok(text) = std::fs::read_to_string(&marker)
            && let Some(pid) = text
                .lines()
                .next()
                .and_then(|l| l.trim().parse::<u32>().ok())
        {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no background run"
        );
        thread::sleep(Duration::from_millis(10));
    };
    client.replace(uri, 2, &SCALARS.replace("inc(3)", "inc(9)"));
    let started = std::time::Instant::now();
    while process_alive(first).unwrap() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the stale background job survived the edit"
        );
        thread::sleep(Duration::from_millis(20));
    }
    client.close();
}

/// Real Lean 4.24.0: semantic theorems proved by omega and replayed by the
/// kernel, a bv_decide counterexample, and `.olean` reuse across servers.
#[test]
fn semantic_proof_lenses_with_real_lean() {
    let (Ok(lean), Ok(auditor)) = (
        std::env::var("VESPER_LEAN"),
        std::env::var("VESPER_LENS_AUDITOR"),
    ) else {
        eprintln!("VESPER_LEAN or VESPER_LENS_AUDITOR unset; real semantic lens skipped");
        return;
    };
    let cache = std::env::temp_dir().join(format!("vesper-lens-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    let config = LensConfig {
        lean,
        auditor: Some(auditor.into()),
        proof_timeout: 120,
        cache: Some(cache.clone()),
        background: None,
        ..LensConfig::default()
    };
    let uri = "untitled:real-semantic.ves";
    let mut compiled_first = 0;
    for round in 0..2 {
        let mut client = Client::start(config.clone());
        client.open(uri, SCALARS);
        let id = client.request("workspace/executeCommand", semantic_args(uri, 1, "inc"));
        let response = client.response(id);
        assert_eq!(response["result"]["status"], "proved", "{response}");
        let detail = response["result"]["detail"].as_str().unwrap();
        assert!(detail.contains("replayed by the kernel"), "{detail}");
        if round == 0 {
            compiled_first = response["result"]["olean_compiled"].as_u64().unwrap();
            assert!(compiled_first >= 1, "{response}");
        } else {
            // A new server reuses the cached `.olean` and still replays it.
            assert_eq!(response["result"]["olean_compiled"], 0, "{response}");
            assert!(response["result"]["olean_reused"].as_u64().unwrap() >= 1);
        }
        let id = client.request("workspace/executeCommand", semantic_args(uri, 1, "add"));
        let response = client.response(id);
        assert_eq!(response["result"]["status"], "refuted", "{response}");
        let counterexample = response["result"]["counterexample"].as_str().unwrap();
        assert!(counterexample.contains("a = ") && counterexample.contains("b = "));
        let lens = semantic_lens(&mut client, uri, "add");
        assert!(
            lens["command"]["title"]
                .as_str()
                .unwrap()
                .starts_with("Lean model: arithmetic fault when a = "),
            "{lens}"
        );
        let id = client.request("workspace/executeCommand", audit_args(uri, "inc"));
        let response = client.response(id);
        assert_eq!(response["result"]["status"], "proved", "{response}");
        client.close();
    }
    assert!(compiled_first >= 1);
    let _ = std::fs::remove_dir_all(&cache);
}
