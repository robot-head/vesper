//! Actual executable acceptance of checked local intra-doc references.
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const LOCAL: &str = include_str!("fixtures/doc_links/local.ves");
const EXCLUSIONS: &str = include_str!("fixtures/doc_links/exclusions.ves");
const EXPECTED: &[&str] = &[
    "Tx",
    "close",
    "Tx::commit",
    "Ask",
    "Ask::ask",
    "Score",
    "Tx",
    "Tx::commit",
    "Tx",
];

fn binary() -> PathBuf {
    let path = std::env::var_os("VESPER_DOC_LINKS_BINARY")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_vesper").map(PathBuf::from))
        .expect("actual vesper or VESPER_DOC_LINKS_BINARY required");
    let exe = PathBuf::from(format!("{}.exe", path.display()));
    if exe.is_file() { exe } else { path }
}

fn output(command: &mut Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(2 * 1024 * 1024 + 1)
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
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(45) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("doc CLI timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = reader.join().unwrap();
    let stderr = errors.join().unwrap();
    assert!(stdout.len() <= 2 * 1024 * 1024 && stderr.len() <= 2 * 1024 * 1024);
    Output {
        status,
        stdout,
        stderr,
    }
}

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    source: String,
}
impl Fixture {
    fn new(source: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-doc-links-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("original.ves");
        fs::write(&path, source.as_bytes()).unwrap();
        Self {
            dir,
            path,
            source: source.into(),
        }
    }
    fn command(&self, args: &[&str]) -> Output {
        let result = output(Command::new(binary()).arg("doc").arg(&self.path).args(args));
        assert_eq!(fs::read(&self.path).unwrap(), self.source.as_bytes());
        result
    }
    fn accepted(&self) -> Value {
        let result = self.command(&["--json"]);
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["status"], "valid");
        assert_eq!(value["document"]["source_bytes"], self.source.len() as u64);
        value
    }
    fn rejected(&self, code: &str, raw: Option<&str>) -> Value {
        let result = self.command(&["--json"]);
        assert_eq!(result.status.code(), Some(1));
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["status"], "invalid");
        assert!(value.get("document").is_none());
        let message = value["error"].as_str().unwrap();
        assert!(message.contains(code), "{message}");
        if let Some(raw) = raw {
            let start = self.source.find(raw).unwrap();
            assert!(
                message.contains(&format!("{start}..{}", start + raw.len())),
                "{message}"
            );
        }
        let target = self.dir.join("must-not-exist.html");
        let result = self.command(&["-o", target.to_str().unwrap()]);
        assert!(!result.status.success() && result.stdout.is_empty());
        assert!(!target.exists());
        value
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn validate_references<'a>(value: &'a Value, source: &str, expected: &[&str]) -> &'a Value {
    let links = &value["document"]["intra_doc_links"];
    assert_eq!(links["status"], "resolved");
    assert_eq!(links["source_bytes"], source.len() as u64);
    assert_eq!(links["diagnostics"], serde_json::json!([]));
    let references = links["references"].as_array().unwrap();
    assert_eq!(references.len(), expected.len());
    let mut end = 0;
    for (reference, target) in references.iter().zip(expected) {
        assert_eq!(reference["target"]["path"], *target);
        let start = reference["span"]["start"].as_u64().unwrap() as usize;
        let stop = reference["span"]["end"].as_u64().unwrap() as usize;
        assert!(start >= end && start < stop && stop <= source.len());
        assert!(source.is_char_boundary(start) && source.is_char_boundary(stop));
        end = stop;
        let display = reference["display"].as_str().unwrap();
        assert_eq!(&source[start..stop], format!("[{display}]"));
        assert_eq!(display.trim_matches('`'), *target);
        let anchor = reference["anchor"].as_str().unwrap();
        assert!(
            !anchor.is_empty()
                && anchor
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        );
        let hex: String = target
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(anchor.ends_with(&hex));
    }
    links
}

fn attributes(text: &str, name: &str) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    for quote in ['\'', '"'] {
        let needle = format!("{name}={quote}");
        for (start, _) in text.match_indices(&needle) {
            if start > 0 && !text.as_bytes()[start - 1].is_ascii_whitespace() {
                continue;
            }
            let rest = &text[start + needle.len()..];
            let value = rest.split_once(quote).unwrap().0;
            *result.entry(value.to_owned()).or_default() += 1;
        }
    }
    result
}

fn validate_html(bytes: &[u8], links: &Value) {
    let html = std::str::from_utf8(bytes).unwrap();
    assert!(html.to_lowercase().contains("unverified"));
    assert!(!html.contains("<script") && !html.contains("<iframe"));
    let hrefs = attributes(html, "href");
    let ids = attributes(html, "id");
    let mut expected = BTreeMap::new();
    for reference in links["references"].as_array().unwrap() {
        *expected
            .entry(format!("#{}", reference["anchor"].as_str().unwrap()))
            .or_insert(0usize) += 1;
    }
    assert_eq!(hrefs, expected);
    for href in hrefs.keys() {
        assert_eq!(ids.get(href.strip_prefix('#').unwrap()), Some(&1));
    }
    assert!(ids.values().all(|count| *count == 1));
}

#[test]
fn actual_local_inventory_links_forward_repeated_and_utf8_occurrences() {
    for source in [LOCAL.to_owned(), LOCAL.replace('\n', "\r\n")] {
        let fixture = Fixture::new(&source);
        let value = fixture.accepted();
        let links = validate_references(&value, &source, EXPECTED);
        assert_eq!(
            links["references"][0]["span"]["start"],
            source.find("[Tx]").unwrap() as u64
        );
        let targets: BTreeSet<_> = links["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|target| {
                (
                    target["path"].as_str().unwrap(),
                    target["kind"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            targets,
            BTreeSet::from([
                ("Tx", "type"),
                ("Tx::commit", "inherent_method"),
                ("Ask", "effect"),
                ("Ask::ask", "effect_operation"),
                ("Score", "trait"),
                ("close", "function"),
                ("main", "function"),
            ])
        );
        let html = fixture.command(&[]);
        assert!(html.status.success());
        validate_html(&html.stdout, links);
        assert_eq!(html.stdout, fixture.command(&[]).stdout);
        assert_eq!(value, fixture.accepted());
    }
}

#[test]
fn actual_lexical_exclusions_cannot_smuggle_references_or_html() {
    let fixture = Fixture::new(EXCLUSIONS);
    let value = fixture.accepted();
    let links = validate_references(&value, EXCLUSIONS, &["present"]);
    let html = fixture.command(&[]);
    assert!(html.status.success());
    validate_html(&html.stdout, links);
    let html = std::str::from_utf8(&html.stdout).unwrap();
    assert!(html.contains("&lt;script&gt;") && html.contains("example.invalid"));
    for prefix in [
        "//! ```\n//! [Hidden]\n\n",
        "//! ` [Hidden]\n// ordinary boundary\n",
        "//! ~~~\n//! [Hidden]\nfn boundary() {}\n",
    ] {
        Fixture::new(&format!("{prefix}/// [missing]\nfn main() {{}}\n"))
            .rejected("unknown", Some("[missing]"));
    }
}

#[test]
fn actual_unknown_ambiguous_and_unsupported_links_fail_before_output_creation() {
    for (source, code, raw) in [
        (
            include_str!("fixtures/doc_links/unknown.ves"),
            "unknown",
            "[missing]",
        ),
        (
            include_str!("fixtures/doc_links/ambiguous.ves"),
            "ambiguous",
            "[Channel]",
        ),
        (
            include_str!("fixtures/doc_links/unsupported_self.ves"),
            "unsupported",
            "[Self::commit]",
        ),
        (
            include_str!("fixtures/doc_links/unsupported_package.ves"),
            "unsupported",
            "[@local/tx::commit]",
        ),
        (
            include_str!("fixtures/doc_links/unsupported_generic.ves"),
            "unsupported",
            "[Box[u64]]",
        ),
    ] {
        Fixture::new(source).rejected(code, Some(raw));
    }
    let edited = LOCAL.replace("fn close(sink", "fn finish(sink");
    Fixture::new(&edited).rejected("unknown", Some("[close]"));
    let repaired = edited.replace("[close]", "[finish]");
    let value = Fixture::new(&repaired).accepted();
    let expected: Vec<_> = EXPECTED
        .iter()
        .map(|path| if *path == "close" { "finish" } else { path })
        .collect();
    validate_references(&value, &repaired, &expected);
}

#[test]
fn real_source_errors_and_finite_refusals_never_publish_linked_documents() {
    for (source, message) in [
        ("/// [missing]\nfn {\n", "source parse failed"),
        (
            "/// [missing]\nfn main() -> bool { 42 }\n",
            "source check failed",
        ),
        (
            "/// [missing]\nlinear struct Ticket {} fn main() { let ticket = Ticket {}; }\n",
            "source check failed",
        ),
    ] {
        let report = Fixture::new(source).rejected(message, None);
        assert!(!report["error"].as_str().unwrap().contains("link unknown"));
    }
    Fixture::new(&format!(
        "//! {}\nfn present() {{}}\n",
        "[present] ".repeat(1025)
    ))
    .rejected("resource", None);
    Fixture::new(&format!("//! [{}]\nfn main() {{}}\n", "a".repeat(1025)))
        .rejected("resource", None);
    Fixture::new(&format!("//{}\nfn main() {{}}\n", "x".repeat(128 * 1024)))
        .rejected("limit", None);
}

#[test]
fn actual_cli_keeps_source_and_existing_output_bytes_unchanged() {
    let fixture = Fixture::new(LOCAL);
    let html = fixture.command(&[]);
    assert!(html.status.success());
    let target = fixture.dir.join("docs.html");
    let saved = fixture.command(&["-o", target.to_str().unwrap()]);
    assert!(saved.status.success() && saved.stdout.is_empty());
    assert_eq!(fs::read(&target).unwrap(), html.stdout);
    let failed = fixture.command(&["-o", target.to_str().unwrap(), "--json"]);
    assert_eq!(failed.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(value["status"], "invalid");
    assert_eq!(fs::read(&target).unwrap(), html.stdout);
    assert!(!fixture.command(&["--approved"]).status.success());
    #[cfg(unix)]
    {
        let symlink = fixture.dir.join("source-link.ves");
        std::os::unix::fs::symlink(&fixture.path, &symlink).unwrap();
        let result = output(
            Command::new(binary())
                .arg("doc")
                .arg(&symlink)
                .arg("--json"),
        );
        assert_eq!(result.status.code(), Some(1));
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["status"], "invalid");
        assert!(value["error"].as_str().unwrap().contains("symlink"));
        assert_eq!(fs::read(&fixture.path).unwrap(), LOCAL.as_bytes());
    }
}
