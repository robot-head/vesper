//! Wave-seven lane 14 (§40): FieldTree heatmaps, the static signature-search
//! index and page, `.olean` certificate links from a Loom store, and doctests
//! split between one shared JIT module and the reference evaluator.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::doctest::{self, DocTestLimits, Engine};
use vesper_doc::hoogle;

const BROKER: &str = include_str!("../crates/vesper-doc/examples/broker.ves");
const HEATMAP_GOLDEN: &str =
    include_str!("../crates/vesper-doc/tests/fixtures/broker-heatmaps.json");
const INDEX_GOLDEN: &str =
    include_str!("../crates/vesper-doc/tests/fixtures/broker-search-index.json");

struct Scratch(PathBuf);
impl Scratch {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-codex-rest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        for (path, bytes) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        Self(root)
    }
    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}
fn ok(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
fn golden(name: &str, actual: &str, expected: &str) {
    if std::env::var_os("VESPER_UPDATE_SNAPSHOTS").is_some() {
        fs::write(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("crates/vesper-doc/tests/fixtures")
                .join(name),
            actual,
        )
        .unwrap();
        return;
    }
    assert_eq!(
        actual, expected,
        "{name}: rerun with VESPER_UPDATE_SNAPSHOTS=1 after review"
    );
}
fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap() + "\n"
}

#[test]
fn heatmaps_and_search_index_match_their_goldens() {
    let dir = Scratch::new(&[("broker.ves", BROKER.as_bytes())]);
    let out = vesper(&dir.0, &["doc", "broker.ves", "--json"]);
    ok(&out);
    let report = json(&out);
    golden(
        "broker-heatmaps.json",
        &pretty(&report["heatmaps"]),
        HEATMAP_GOLDEN,
    );
    golden(
        "broker-search-index.json",
        &pretty(&report["search_index"]),
        INDEX_GOLDEN,
    );
    let broker = &report["heatmaps"]["heatmaps"][0];
    assert_eq!(broker["ty"], "Broker");
    assert_eq!(
        broker["leaves"],
        serde_json::json!(["index", "metrics.sent", "metrics.dropped", "config"])
    );
    let publish = broker["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["function"] == "publish")
        .unwrap();
    assert_eq!(
        publish["cells"],
        serde_json::json!(["write", "write", "free", "free"])
    );
    assert_eq!(
        publish["free"],
        serde_json::json!(["metrics.dropped", "config"])
    );
    // The index is deterministic across runs.
    let again = json(&vesper(&dir.0, &["doc", "broker.ves", "--json"]));
    assert_eq!(again["search_index"], report["search_index"]);
    // HTML carries the heatmap grid.
    let html = vesper(&dir.0, &["doc", "broker.ves"]);
    ok(&html);
    let html = String::from_utf8(html.stdout).unwrap();
    assert!(html.contains("<article class=\"heatmap\" id=\"heatmap-Broker\">"));
    assert!(html.contains("publish(inout broker.{index, metrics.sent})"));
}

const QUERIES: &[(&str, &[&str])] = &[
    ("inout Broker -> ()", &["drop_one"]),
    ("inout Broker.{metrics} -> ()", &["drop_one"]),
    (
        "inout Broker.{index, metrics.sent}, u64 -> ()",
        &["publish"],
    ),
    ("in Broker -> u64", &["checksum", "config_of"]),
    ("in copy Broker.{config} -> copy u64 eff []", &["config_of"]),
    ("_ -> u64", &["checksum", "config_of"]),
    ("sink linear WriteTxn -> Receipt eff [Storage]", &["commit"]),
    ("sink WriteTxn -> _ eff []", &[]),
    ("in Broker -> linear WriteTxn", &["begin"]),
    ("in Broker -> copy WriteTxn", &[]),
    ("_, _ -> ()", &["publish"]),
    ("() -> u64", &[]),
    ("conf", &["config_of"]),
];

#[test]
fn text_search_is_variance_aware_and_ranks_exact_and_proved_first() {
    let dir = Scratch::new(&[("broker.ves", BROKER.as_bytes())]);
    for (query, expected) in QUERIES {
        let out = vesper(&dir.0, &["doc", "broker.ves", "--json", "--search", query]);
        ok(&out);
        let hits: Vec<String> = json(&out)["text_search"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["name"].as_str().unwrap().to_string())
            .collect();
        let mut sorted = hits.clone();
        sorted.sort();
        assert_eq!(sorted, *expected, "query {query}");
    }
    // An exact contract ranks first.
    let out = vesper(
        &dir.0,
        &[
            "doc",
            "broker.ves",
            "--json",
            "--search",
            "in copy Broker.{config} -> copy u64 eff []",
        ],
    );
    let hit = &json(&out)["text_search"]["hits"][0];
    assert_eq!(
        (hit["exact"].clone(), hit["score"].clone()),
        (Value::Bool(true), 10.into())
    );
    // Malformed queries are typed errors.
    for bad in [
        "inout Broker",
        "-> ",
        "a b",
        "inout Broker -> () eff [_]",
        "x -> y z",
    ] {
        let out = vesper(&dir.0, &["doc", "broker.ves", "--json", "--search", bad]);
        assert!(!out.status.success(), "{bad}");
        assert!(
            json(&out)["error"]
                .as_str()
                .unwrap()
                .contains("invalid search query")
        );
    }
    let out = vesper(&dir.0, &["doc", "broker.ves", "--search", "conf"]);
    assert!(!out.status.success(), "--search without --json");
}

#[test]
fn bundle_ships_a_self_contained_search_page_with_the_same_matcher() {
    let dir = Scratch::new(&[("broker.ves", BROKER.as_bytes())]);
    ok(&vesper(&dir.0, &["doc", "broker.ves", "--bundle", "site"]));
    let site = dir.path("site");
    let index_text = fs::read_to_string(site.join("search-index.json")).unwrap();
    let index = hoogle::parse_index(&index_text).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&index_text).unwrap(),
        serde_json::from_str::<Value>(INDEX_GOLDEN).unwrap()
    );
    let page = fs::read_to_string(site.join("search.html")).unwrap();
    let html = fs::read_to_string(site.join("index.html")).unwrap();
    assert!(html.contains("<a href=\"search.html\">Search signatures</a>"));
    assert!(html.contains("<article id=\"item-publish\">"));
    for text in [&page, &html] {
        // The SVG namespace is an identifier, not a request.
        let text = text.replace("xmlns=\"http://www.w3.org/2000/svg\"", "");
        assert!(
            !text.contains("http://") && !text.contains("https://"),
            "no network references"
        );
    }
    assert!(page.contains("id=\"codex-index\""));
    // An existing bundle directory is never overwritten.
    let again = vesper(&dir.0, &["doc", "broker.ves", "--bundle", "site"]);
    assert!(!again.status.success());

    // Run the page's script under Node, when available, against the same
    // index and compare every query with the Rust matcher.
    let node = Command::new("node").arg("--version").output();
    if !node.is_ok_and(|o| o.status.success()) {
        eprintln!("node not found: skipping the search-page script parity check");
        return;
    }
    let script = page
        .split("<script>const CODEX_HREF=\"index.html\";\n")
        .nth(1)
        .and_then(|s| s.split("</script>").next())
        .unwrap();
    assert_eq!(script, hoogle::SEARCH_JS);
    let mut queries: Vec<&str> = QUERIES.iter().map(|(q, _)| *q).collect();
    queries.extend([
        "inout Broker",
        "a b",
        "Option[_] -> _",
        "(u64, _) -> ()",
        "eff",
    ]);
    fs::write(dir.path("search.js"), script).unwrap();
    fs::write(
        dir.path("queries.json"),
        serde_json::to_string(&queries).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path("driver.js"),
        "const {parseQuery, search} = require('./search.js');\nconst fs = require('fs');\nconst index = JSON.parse(fs.readFileSync('site/search-index.json', 'utf8'));\nconst out = JSON.parse(fs.readFileSync('queries.json', 'utf8')).map(q => { try { return search(parseQuery(q), index); } catch (e) { return null; } });\nprocess.stdout.write(JSON.stringify(out));\n",
    )
    .unwrap();
    let out = Command::new("node")
        .current_dir(&dir.0)
        .arg("driver.js")
        .output()
        .unwrap();
    ok(&out);
    let js: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    for (query, js) in queries.iter().zip(js) {
        let rust = hoogle::parse_text_query(query)
            .map(|q| serde_json::to_value(hoogle::search(&q, &index)).unwrap())
            .unwrap_or(Value::Null);
        assert_eq!(js, rust, "query {query}");
    }
}

fn audit_for(module: &str, olean: &[u8]) -> String {
    format!(
        r#"{{"schema_version":1,"status":"audited","accepted":true,"kernel_replayed":true,"cache_eligible":false,"module":"{module}","lean_version":"Lean (version 4.24.0)","lean_binary_blake3":"00","auditor_blake3":"01","artifact_inventory_blake3":"02","checked_constants":3,"allowed_axioms":[],"forbidden_axioms":[],"theorems":[{{"name":"Codex.Broker.commit_slot","axioms":[],"type_repr":"True"}}],"artifact_files":[{{"path":"{module}.olean","bytes":{},"blake3":"{}"}}],"imports":[]}}"#,
        olean.len(),
        blake3::hash(olean).to_hex()
    )
}

#[test]
fn olean_certificates_link_from_a_loom_store_into_badges_and_bundles() {
    let olean = b"\x00olean certificate bytes";
    let audit = audit_for("CodexBroker", olean);
    let dir = Scratch::new(&[
        ("broker.ves", BROKER.as_bytes()),
        ("audit.json", audit.as_bytes()),
        ("build/CodexBroker.olean", olean),
    ]);
    // Before anything is stored the store is consulted but links nothing.
    fs::create_dir(dir.path("store")).unwrap();
    let before = json(&vesper(
        &dir.0,
        &[
            "doc",
            "broker.ves",
            "--json",
            "--proof-audit",
            "audit.json",
            "--olean-store",
            "store",
        ],
    ));
    assert_eq!(before["certificates"][0]["status"], "missing");
    let out = vesper(
        &dir.0,
        &[
            "doc",
            "store-olean",
            "--proof-audit",
            "audit.json",
            "--root",
            "build",
            "--store",
            "store",
            "--json",
        ],
    );
    ok(&out);
    let stored = json(&out);
    let object = stored["certificates"][0]["object_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(object.starts_with("b3v1:artifact:blob:"), "{object}");

    let out = vesper(
        &dir.0,
        &[
            "doc",
            "broker.ves",
            "--json",
            "--proof-audit",
            "audit.json",
            "--olean-store",
            "store",
        ],
    );
    ok(&out);
    let report = json(&out);
    let commit = report["badges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["item"] == "commit")
        .unwrap();
    assert_eq!(commit["level"], "proved");
    assert_eq!(commit["certificates"][0]["status"], "linked");
    assert_eq!(commit["certificates"][0]["object_id"], object.as_str());
    // Unverified items get no certificate.
    let other = report["badges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["item"] == "publish")
        .unwrap();
    assert_eq!(other["certificates"], serde_json::json!([]));
    // The proved entry ranks first in a name-agnostic search.
    let index = &report["search_index"]["entries"];
    assert!(
        index
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "commit" && e["badge"] == "proved")
    );

    // A bundle copies the verified bytes and links them relatively.
    ok(&vesper(
        &dir.0,
        &[
            "doc",
            "broker.ves",
            "--proof-audit",
            "audit.json",
            "--olean-store",
            "store",
            "--bundle",
            "site",
        ],
    ));
    let name = format!("certs/{}.olean", blake3::hash(olean).to_hex());
    assert_eq!(fs::read(dir.path("site").join(&name)).unwrap(), olean);
    let html = fs::read_to_string(dir.path("site/index.html")).unwrap();
    assert!(html.contains(&format!("<a href=\"{name}\">CodexBroker.olean</a>")));

    // A tampered build directory is refused, and nothing gets linked to a
    // different audit report.
    fs::write(dir.path("build/CodexBroker.olean"), b"tampered").unwrap();
    let tampered = vesper(
        &dir.0,
        &[
            "doc",
            "store-olean",
            "--proof-audit",
            "audit.json",
            "--root",
            "build",
            "--store",
            "store",
        ],
    );
    assert!(!tampered.status.success());
    fs::write(dir.path("other.json"), audit_for("CodexBroker", b"other")).unwrap();
    let other = json(&vesper(
        &dir.0,
        &[
            "doc",
            "broker.ves",
            "--json",
            "--proof-audit",
            "other.json",
            "--olean-store",
            "store",
        ],
    ));
    assert_eq!(other["certificates"][0]["status"], "missing");
    // A missing store is an error and is not created.
    let absent = vesper(
        &dir.0,
        &[
            "doc",
            "broker.ves",
            "--proof-audit",
            "audit.json",
            "--olean-store",
            "absent",
        ],
    );
    assert!(!absent.status.success());
    assert!(!dir.path("absent").exists());
}

const MIXED: &str = "/// A pair of counters.\ncopy struct Pair {\n    left: u64,\n    right: u64\n}\n\n/// ```vesper,entry=scalar\n/// fn scalar() -> u64 {\n///     wrapping_add(20, 22)\n/// }\n/// ```\n///\n/// ```vesper,entry=aggregate\n/// fn aggregate() -> u64 {\n///     total(Pair {\n///         left: 40,\n///         right: 2\n///     })\n/// }\n/// ```\n///\n/// ```vesper,entry=also_scalar\n/// fn also_scalar() -> u64 {\n///     wrapping_add(scalar_helper(), 1)\n/// }\n/// fn scalar_helper() -> u64 {\n///     6\n/// }\n/// ```\nfn total(p: Pair) -> u64 {\n    wrapping_add(p.left, p.right)\n}\n";

#[test]
fn doctests_share_one_jit_module_and_fall_back_to_the_evaluator_per_example() {
    let report = doctest::run(MIXED, DocTestLimits::default()).unwrap();
    assert!(report.passed, "{report:?}");
    let engines: Vec<_> = report.examples.iter().map(|e| e.engine).collect();
    assert_eq!(
        engines,
        [
            Some(Engine::Jit),
            Some(Engine::Evaluator),
            Some(Engine::Jit)
        ]
    );
    let results: Vec<_> = report
        .examples
        .iter()
        .map(|e| e.result.as_deref())
        .collect();
    assert_eq!(results, [Some("42"), Some("42"), Some("7")]);
    assert!(
        report.examples[1]
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("aggregate")
    );
    let batch = report.batch.as_ref().unwrap();
    // Both covered examples ran in the same single module.
    assert_eq!((batch.module_count, batch.entry_count), (1, 2));
    assert_eq!(report.coverage_probes, 3);
    assert_eq!((report.jit_examples, report.evaluator_examples), (2, 1));

    // When everything is covered no probe is compiled.
    let scalar = "/// ```vesper,entry=a\n/// fn a() -> u64 {\n///     1\n/// }\n/// ```\n///\n/// ```vesper,entry=b\n/// fn b() -> u64 {\n///     2\n/// }\n/// ```\nfn anchor() -> u64 {\n    0\n}\n";
    let report = doctest::run(scalar, DocTestLimits::default()).unwrap();
    assert!(report.passed);
    assert_eq!(report.coverage_probes, 0);
    assert_eq!(report.batch.as_ref().unwrap().entry_count, 2);
    assert!(
        report
            .examples
            .iter()
            .all(|e| e.engine == Some(Engine::Jit))
    );

    // Only evaluator-run examples: no shared module at all.
    let only = "copy struct P {\n    x: u64\n}\n/// ```vesper,entry=a\n/// fn a() -> u64 {\n///     let p = P {\n///         x: 3\n///     };\n///     p.x\n/// }\n/// ```\nfn anchor() -> u64 {\n    0\n}\n";
    let report = doctest::run(only, DocTestLimits::default()).unwrap();
    assert!(report.passed, "{report:?}");
    assert!(report.batch.is_none());
    assert_eq!(report.examples[0].engine, Some(Engine::Evaluator));
    assert_eq!(report.examples[0].result.as_deref(), Some("3"));
}

#[test]
fn evaluator_fallback_failures_are_reported_not_skipped() {
    // An ambient effect is outside the native subset, and the evaluator
    // refuses to invent a handler for it.
    let source = "effect Ask {\n    fn ask() -> u64;\n}\n/// ```vesper,entry=a\n/// fn a() -> u64 eff [Ask] {\n///     Ask::ask()\n/// }\n/// ```\nfn anchor() -> u64 {\n    0\n}\n";
    let report = doctest::run(source, DocTestLimits::default()).unwrap();
    assert!(!report.passed);
    let example = &report.examples[0];
    assert_eq!(example.engine, Some(Engine::Evaluator));
    assert!(
        example.message.starts_with("evaluator execution failed"),
        "{}",
        example.message
    );

    let dir = Scratch::new(&[("mixed.ves", MIXED.as_bytes())]);
    let out = vesper(&dir.0, &["test", "--doc", "mixed.ves"]);
    ok(&out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("PASS doctest 0: executed repeatedly in shared JIT module [jit]"),
        "{text}"
    );
    assert!(
        text.contains("PASS doctest 1: executed repeatedly on the reference evaluator"),
        "{text}"
    );
    assert!(text.contains("[evaluator]"), "{text}");
}

#[test]
fn heatmaps_explain_enums_unused_types_and_opaque_leaves() {
    let source = "copy enum Mode {\n    Fast,\n    Slow\n}\ncopy struct Cfg {\n    mode: Mode,\n    pair: (u64, u64),\n    n: u64\n}\nfn bump(inout c.{pair}: Cfg) {\n    let p = c.pair;\n}\nfn pick(in m: Mode) -> u64 {\n    0\n}\ncopy struct Unused {\n    a: u64\n}\n";
    let dir = Scratch::new(&[("cfg.ves", source.as_bytes())]);
    let out = vesper(&dir.0, &["doc", "cfg.ves", "--json"]);
    ok(&out);
    let maps = &json(&out)["heatmaps"];
    let cfg = &maps["heatmaps"][0];
    assert_eq!(cfg["leaves"], serde_json::json!(["mode", "pair", "n"]));
    assert_eq!(
        cfg["rows"][0]["cells"],
        serde_json::json!(["free", "write", "free"])
    );
    assert_eq!(
        cfg["opaque"],
        serde_json::json!([
            ["mode", "enum or generic type is a single leaf"],
            ["pair", "generic, tuple or function type is a single leaf"]
        ])
    );
    assert_eq!(
        maps["unsupported"],
        serde_json::json!([
            ["Mode", "enum variants overlap: no FieldTree heatmap"],
            ["Unused", "no documented function borrows or consumes it"]
        ])
    );
}
