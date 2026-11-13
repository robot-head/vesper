//! Actual framed definition requests against current unsaved doc comments.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const LOCAL: &str = include_str!("fixtures/doc_navigation/local.ves");
const EXCLUSIONS: &str = include_str!("fixtures/doc_navigation/exclusions.ves");
const COLLISIONS: &str = include_str!("fixtures/doc_navigation/collisions.ves");
const DISK: &[u8] = b"fn on_disk() -> i64 { 99 } fn main() -> i64 { on_disk() }\n";

fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_DOC_NAVIGATION_BINARY")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_vesper").map(PathBuf::from))
        .expect("actual vesper or VESPER_DOC_NAVIGATION_BINARY required");
    let exe = PathBuf::from(format!("{}.exe", path.display()));
    if exe.is_file() { exe } else { path }
}
fn request(id: i32, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn notice(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn doc(uri: &str) -> Value {
    json!({"textDocument":{"uri":uri}})
}
fn opened(uri: &str, version: i32, source: &str) -> Value {
    notice(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"version":version,"languageId":"vesper","text":source}}),
    )
}
fn changed(uri: &str, version: i32, source: &str) -> Value {
    notice(
        "textDocument/didChange",
        json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":source}]}),
    )
}
fn position(source: &str, offset: usize) -> Value {
    assert!(source.is_char_boundary(offset));
    let prefix = &source[..offset];
    json!({"line":prefix.bytes().filter(|b| *b == b'\n').count(),
        "character":prefix.rsplit('\n').next().unwrap().encode_utf16().count()})
}
fn definition(id: i32, uri: &str, source: &str, needle: &str, delta: usize) -> Value {
    request(
        id,
        "textDocument/definition",
        json!({"textDocument":{"uri":uri},"position":position(source,source.find(needle).unwrap()+delta)}),
    )
}
fn location(uri: &str, source: &str, marker: &str, skip: usize, name: &str) -> Value {
    let start = source.find(marker).unwrap() + skip;
    assert_eq!(&source[start..start + name.len()], name);
    json!({"uri":uri,"range":{"start":position(source,start),"end":position(source,start+name.len())}})
}
fn response(replies: &[Value], id: i32) -> &Value {
    let found: Vec<_> = replies.iter().filter(|r| r["id"] == id).collect();
    assert_eq!(found.len(), 1, "response {id}: {replies:?}");
    found[0]
}
fn result(replies: &[Value], id: i32) -> &Value {
    let value = response(replies, id);
    assert!(value.get("error").is_none(), "{value}");
    value.get("result").unwrap()
}
fn error(replies: &[Value], id: i32, code: i32, kind: Option<&str>) {
    let value = response(replies, id);
    assert_eq!(value["error"]["code"], code, "{value}");
    assert!(value.get("result").is_none());
    if let Some(kind) = kind {
        assert_eq!(value["error"]["data"]["kind"], kind);
    }
}

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    uri: String,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-doc-navigation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("original-🦀.ves");
        fs::write(&path, DISK).unwrap();
        let text = path.to_string_lossy().replace('\\', "/");
        let mut uri = if text.starts_with('/') {
            "file://".to_owned()
        } else {
            "file:///".to_owned()
        };
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric()
                || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~')
            {
                uri.push(char::from(byte));
            } else {
                uri.push_str(&format!("%{byte:02X}"));
            }
        }
        Self { dir, path, uri }
    }
    fn run(&self, mut messages: Vec<Value>) -> Vec<Value> {
        messages.insert(0, request(1, "initialize", json!({"capabilities":{}})));
        messages.push(request(9999, "shutdown", json!({})));
        messages.push(notice("exit", json!({})));
        let mut input = Vec::new();
        for message in messages {
            let body = serde_json::to_vec(&message).unwrap();
            write!(&mut input, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
            input.extend(body);
        }
        let mut child = Command::new(binary())
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let writer = thread::spawn(move || stdin.write_all(&input));
        let mut stdout = child.stdout.take().unwrap();
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .by_ref()
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        });
        let mut stderr = child.stderr.take().unwrap();
        let errors = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr
                .by_ref()
                .take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > Duration::from_secs(45) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("doc navigation stdio timed out");
            }
            thread::sleep(Duration::from_millis(10));
        };
        writer.join().unwrap().unwrap();
        let bytes = reader.join().unwrap();
        let errors = errors.join().unwrap();
        assert!(bytes.len() <= 16 * 1024 * 1024 && errors.len() <= 2 * 1024 * 1024);
        assert!(status.success(), "{}", String::from_utf8_lossy(&errors));
        let mut tail = bytes.as_slice();
        let mut replies = Vec::new();
        while !tail.is_empty() {
            let end = tail.windows(4).position(|b| b == b"\r\n\r\n").unwrap();
            assert!(end <= 8192);
            let header = std::str::from_utf8(&tail[..end]).unwrap();
            let lengths: Vec<_> = header
                .split("\r\n")
                .filter_map(|line| {
                    let (key, value) = line.split_once(':').unwrap();
                    key.eq_ignore_ascii_case("Content-Length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .collect();
            assert_eq!(lengths.len(), 1);
            let size = lengths[0];
            assert!(size > 0 && size <= 2 * 1024 * 1024);
            tail = &tail[end + 4..];
            assert!(tail.len() >= size);
            let value: Value = serde_json::from_slice(&tail[..size]).unwrap();
            assert_eq!(value["jsonrpc"], "2.0");
            replies.push(value);
            tail = &tail[size..];
        }
        assert_eq!(
            result(&replies, 1)["capabilities"]["definitionProvider"],
            true
        );
        assert_eq!(result(&replies, 9999), &Value::Null);
        assert_eq!(fs::read(&self.path).unwrap(), DISK);
        replies
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn current_unicode_crlf_doc_links_select_exact_original_declaration_names() {
    for source in [LOCAL.to_owned(), LOCAL.replace('\n', "\r\n")] {
        let fixture = Fixture::new();
        let uri = &fixture.uri;
        let targets = [
            ("[close]", "\nfn close", 4, "close"),
            ("[`Tx::commit`]", "fn commit", 3, "commit"),
            ("[Ask::ask]", "linear effect Ask { fn ask", 23, "ask"),
            ("[Score]", "trait Score", 6, "Score"),
            ("[i64::score]", "impl i64 { fn score", 14, "score"),
            ("[Point::score]", "impl Point { fn score", 16, "score"),
            ("[Tx]", "linear struct Tx", 14, "Tx"),
            ("[Ask]", "linear effect Ask", 14, "Ask"),
            ("[ask]", "\nfn ask", 4, "ask"),
        ];
        let mut messages = vec![opened(uri, 1, &source)];
        for (i, (reference, ..)) in targets.iter().enumerate() {
            messages.push(definition(i as i32 + 10, uri, &source, reference, 0));
        }
        messages.extend([
            definition(30, uri, &source, "[`Tx::commit`]", 1),
            definition(31, uri, &source, "[`Tx::commit`]", "[`Tx::commit`]".len()),
            definition(32, uri, &source, "; close", 2),
            definition(35, uri, &source, "[`Tx::commit`]", "[`Tx::commit`]".len()-1),
            request(
                33,
                "textDocument/hover",
                json!({"textDocument":{"uri":uri},"position":position(&source,source.find("[close]").unwrap())}),
            ),
        ]);
        let mut midpoint = position(&source, source.find('😀').unwrap());
        midpoint["character"] = json!(midpoint["character"].as_u64().unwrap() + 1);
        messages.push(request(
            34,
            "textDocument/definition",
            json!({"textDocument":{"uri":uri},"position":midpoint}),
        ));
        let replies = fixture.run(messages);
        for (i, (_, marker, skip, name)) in targets.iter().enumerate() {
            assert_eq!(
                result(&replies, i as i32 + 10),
                &location(uri, &source, marker, *skip, name)
            );
        }
        assert_eq!(result(&replies, 30), result(&replies, 11));
        assert_eq!(result(&replies, 35), result(&replies, 11));
        assert_eq!(result(&replies, 31), &Value::Null);
        assert_eq!(result(&replies, 33), &Value::Null);
        assert_eq!(
            result(&replies, 32),
            &location(uri, &source, "let close", 4, "close")
        );
        error(&replies, 34, -32602, None);
    }
}

#[test]
fn rejected_and_excluded_links_return_null_while_other_resolved_links_navigate() {
    let fixture = Fixture::new();
    let uri = &fixture.uri;
    let excluded = [
        "NoInline",
        "NoDouble",
        "NoEscape",
        "NoImage",
        "NoUrl",
        "NoReference",
        "id",
        "T-CallProj",
        "NoFence",
        "NoTilde",
        "NoFourSlash",
        "NoOrdinary",
        "NoString",
    ];
    let mut messages = vec![opened(uri, 1, EXCLUSIONS)];
    messages.push(definition(10, uri, EXCLUSIONS, "[present]", 0));
    for (i, name) in excluded.iter().enumerate() {
        messages.push(definition(
            20 + i as i32,
            uri,
            EXCLUSIONS,
            &format!("[{name}"),
            1,
        ));
    }
    let replies = fixture.run(messages);
    assert_eq!(
        result(&replies, 10),
        &location(uri, EXCLUSIONS, "fn present", 3, "present")
    );
    for id in 20..20 + excluded.len() as i32 {
        assert_eq!(result(&replies, id), &Value::Null);
    }
    let rejected = [
        "[Channel]",
        "[missing]",
        "[Self::commit]",
        "[@local/type::method]",
        "[Box[i64]]",
        "[i64]",
    ];
    let mut messages = vec![opened(uri, 1, COLLISIONS)];
    for (i, reference) in rejected.iter().enumerate() {
        messages.push(definition(10 + i as i32, uri, COLLISIONS, reference, 0));
    }
    messages.extend([
        definition(30, uri, COLLISIONS, "[Channel::read]", 0),
        definition(31, uri, COLLISIONS, "[current]", 0),
    ]);
    let replies = fixture.run(messages);
    for id in 10..10 + rejected.len() as i32 {
        assert_eq!(result(&replies, id), &Value::Null);
    }
    assert_eq!(
        result(&replies, 30),
        &location(uri, COLLISIONS, "fn read", 3, "read")
    );
    assert_eq!(
        result(&replies, 31),
        &location(uri, COLLISIONS, "fn current", 3, "current")
    );
}

#[test]
fn current_edits_rejected_stale_transactions_close_and_reopen_keep_exact_identity() {
    let fixture = Fixture::new();
    let uri = &fixture.uri;
    let original =
        "//! 😀 [current]\r\nfn current() -> i64 { 42 }\r\nfn main() -> i64 { current() }\r\n";
    let renamed = original.replace("current", "renamed");
    let broken = renamed.replace("[renamed]", "[current]");
    let final_text = original.replace("current", "final_name");
    let mut invalid = position(original, original.find('😀').unwrap());
    invalid["character"] = json!(invalid["character"].as_u64().unwrap() + 1);
    let replies = fixture.run(vec![
        opened(uri, 1, original),
        definition(10, uri, original, "[current]", 0),
        changed(uri, 2, &broken),
        definition(11, uri, &broken, "[current]", 0),
        changed(uri, 3, &renamed),
        definition(12, uri, &renamed, "[renamed]", 0),
        changed(uri, 2, original),
        definition(13, uri, &renamed, "[renamed]", 0),
        notice("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":4},"contentChanges":[{"range":{"start":invalid,"end":invalid},"text":"x"}]})),
        definition(14, uri, &renamed, "[renamed]", 0),
        changed(uri, 4, &final_text),
        definition(15, uri, &final_text, "[final_name]", 0),
        changed(uri, 5, &"x".repeat(128 * 1024 + 1)),
        definition(16, uri, &final_text, "[final_name]", 0),
        notice("textDocument/didClose", doc(uri)),
        definition(17, uri, &final_text, "[final_name]", 0),
        opened(uri, 1, original),
        definition(18, uri, original, "[current]", 0),
    ]);
    for id in [10, 18] {
        assert_eq!(
            result(&replies, id),
            &location(uri, original, "fn current", 3, "current")
        );
    }
    assert_eq!(result(&replies, 11), &Value::Null);
    for id in [12, 13, 14] {
        assert_eq!(
            result(&replies, id),
            &location(uri, &renamed, "fn renamed", 3, "renamed")
        );
    }
    for id in [15, 16] {
        assert_eq!(
            result(&replies, id),
            &location(uri, &final_text, "fn final_name", 3, "final_name")
        );
    }
    error(&replies, 17, -32602, None);
}

#[test]
fn doc_resource_refusal_preserves_ordinary_lexical_definition_priority() {
    let fixture = Fixture::new();
    let uri = &fixture.uri;
    let source = format!(
        "//! {}\nfn present() -> i64 {{ 42 }} fn main() -> i64 {{ present() }}\n",
        "[present]; ".repeat(1025)
    );
    let replies = fixture.run(vec![
        opened(uri, 1, &source),
        definition(10, uri, &source, "[present]", 0),
        definition(11, uri, &source, "{ present()", 2),
    ]);
    error(&replies, 10, -32003, Some("resource"));
    assert_eq!(
        result(&replies, 11),
        &location(uri, &source, "fn present", 3, "present")
    );
}

#[test]
fn macro_preview_never_authorizes_original_source_doc_navigation_or_semantic_errors() {
    let fixture = Fixture::new();
    let uri = &fixture.uri;
    let source = "//! [score]\nfn score(in value: ()) -> i64 { 42 }\nmacro make() -> expr { quote { () } }\nfn main() -> i64 { score(make!()) }\n";
    let replies = fixture.run(vec![
        opened(uri, 1, source),
        definition(10, uri, source, "[score]", 0),
        request(
            11,
            "vesper/macroPreview",
            json!({"textDocument":{"uri":uri,"version":1}}),
        ),
        definition(12, uri, source, "[score]", 0),
    ]);
    for id in [10, 12] {
        error(&replies, id, -32003, Some("parse"));
    }
    assert_eq!(result(&replies, 11)["status"], "expanded_checked");
    for (source, kind) in [
        ("//! [main]\nfn main() -> bool { 42 }\n", "semantic"),
        ("//! [main]\nfn broken(\n", "parse"),
    ] {
        let replies = fixture.run(vec![
            opened(uri, 1, source),
            definition(10, uri, source, "[main]", 0),
        ]);
        error(&replies, 10, -32003, Some(kind));
    }
}
