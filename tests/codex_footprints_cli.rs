//! Wave-eight lane 17 (§40): may-use footprints and inferred effect rows,
//! mode-crossing tables, capability badges, the package-wide search index
//! (impls, Pact models, generic instantiation), one-step `.olean` publishing
//! and per-package doctest modules.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_doc::hoogle;

const ACCOUNT: &str = include_str!("../crates/vesper-doc/examples/footprints/account.ves");
const PACKAGE_FILES: &[(&str, &str)] = &[
    (
        "Loom.toml",
        include_str!("../crates/vesper-doc/examples/codex-package/Loom.toml"),
    ),
    (
        "src/ledger.ves",
        include_str!("../crates/vesper-doc/examples/codex-package/src/ledger.ves"),
    ),
    (
        "src/audit.ves",
        include_str!("../crates/vesper-doc/examples/codex-package/src/audit.ves"),
    ),
    (
        "src/models.ves",
        include_str!("../crates/vesper-doc/examples/codex-package/src/models.ves"),
    ),
];
const FOOTPRINTS_GOLDEN: &str =
    include_str!("../crates/vesper-doc/tests/fixtures/account-footprints.json");
const FOOTPRINTS_HTML_GOLDEN: &str =
    include_str!("../crates/vesper-doc/tests/fixtures/account-footprints.html");
const PACKAGE_INDEX_GOLDEN: &str =
    include_str!("../crates/vesper-doc/tests/fixtures/codex-package-search-index.json");
const PACKAGE_CAPS_GOLDEN: &str =
    include_str!("../crates/vesper-doc/tests/fixtures/codex-package-capabilities.json");

struct Scratch(PathBuf);
impl Scratch {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-codex-footprints-{}-{}",
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
    fn package() -> Self {
        let files: Vec<(&str, &[u8])> = PACKAGE_FILES
            .iter()
            .map(|(p, s)| (*p, s.as_bytes()))
            .collect();
        Self::new(&files)
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
fn find<'a>(items: &'a Value, key: &str, value: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i[key] == value)
        .unwrap_or_else(|| panic!("no {key} = {value}"))
}

#[test]
fn footprints_modes_and_capabilities_match_their_goldens() {
    let dir = Scratch::new(&[("account.ves", ACCOUNT.as_bytes())]);
    let out = vesper(&dir.0, &["doc", "account.ves", "--json"]);
    ok(&out);
    let report = json(&out);
    let views = serde_json::json!({
        "may_use": report["may_use"],
        "mode_crossings": report["mode_crossings"],
        "capabilities": report["capabilities"],
        "heatmaps": report["heatmaps"],
    });
    golden(
        "account-footprints.json",
        &pretty(&views),
        FOOTPRINTS_GOLDEN,
    );
    // Deterministic: a second run is byte-identical.
    assert_eq!(
        vesper(&dir.0, &["doc", "account.ves", "--json"]).stdout,
        out.stdout
    );

    // May-use: `pending` is declared but never reached; the shadowing local
    // `limit` is not the parameter; Metrics is declared but never performed.
    let may = &report["may_use"]["functions"];
    let deposit = find(may, "function", "deposit");
    assert_eq!(
        deposit["parameters"][0]["unused_declared"],
        serde_json::json!(["pending"])
    );
    assert_eq!(
        deposit["parameters"][0]["uses"],
        serde_json::json!([{"path": "balance", "access": "write"}])
    );
    let headroom = find(may, "function", "headroom");
    assert_eq!(
        headroom["parameters"][0]["unused_declared"],
        serde_json::json!([])
    );
    let record = find(may, "function", "record");
    assert_eq!(record["performed_effects"], serde_json::json!(["Journal"]));
    assert_eq!(
        record["declared_unperformed"],
        serde_json::json!(["Metrics"])
    );
    assert_eq!(
        find(may, "function", "remaining")["performed_effects"],
        serde_json::json!(["Fail"])
    );
    assert_eq!(
        find(may, "function", "release")["parameters"][0]["uses"][0]["access"],
        "consume"
    );
    // The heatmap row carries the may-use cells next to the declared ones.
    let account = find(&report["heatmaps"]["heatmaps"], "ty", "Account");
    let row = find(&account["rows"], "function", "deposit");
    assert_eq!(row["cells"], serde_json::json!(["write", "write", "free"]));
    assert_eq!(row["may_use"], serde_json::json!(["write", "free", "free"]));

    // [K-Adt] tables: builtin Option, a two-parameter product, a bound.
    let tables = &report["mode_crossings"]["tables"];
    let option = find(tables, "ty", "Option");
    assert_eq!(option["origin"], "builtin");
    let modes: Vec<&str> = option["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["mode"].as_str().unwrap())
        .collect();
    assert_eq!(modes, ["Copy", "Affine", "Linear"]);
    let pair = find(tables, "ty", "Pair");
    assert_eq!(pair["rows"].as_array().unwrap().len(), 9);
    assert_eq!(
        pair["rows"][2]["args"],
        serde_json::json!(["Copy", "Linear"])
    );
    assert_eq!(pair["rows"][2]["mode"], "Linear");
    assert_eq!(pair["rows"][0]["mode"], "Affine", "declared affine struct");
    let shelf = find(tables, "ty", "Shelf");
    assert_eq!(shelf["rows"][2]["mode"], Value::Null);
    assert!(
        shelf["rows"][2]["rejected"]
            .as_str()
            .unwrap()
            .contains("mod affine")
    );

    // Capability badges from the checked rows.
    let caps = &report["capabilities"]["items"];
    let names = |item: &str| -> Vec<String> {
        find(caps, "item", item)["badges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["capability"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names("deposit"), ["pure", "zero_alloc"]);
    assert_eq!(names("record"), ["zero_alloc", "uncancellable_linear_io"]);
    assert_eq!(names("remaining"), ["zero_alloc", "abortable_checkpoint"]);
    assert!(
        find(caps, "item", "remaining")["badges"][1]["evidence"]
            .as_str()
            .unwrap()
            .contains("hold stay live")
    );
    assert_eq!(
        names("fee"),
        ["zero_alloc", "constant_time", "hard_realtime"]
    );

    // HTML: the new sections, golden from the heatmaps through the impls.
    let html = vesper(&dir.0, &["doc", "account.ves"]);
    ok(&html);
    let html = String::from_utf8(html.stdout).unwrap();
    let start = html.find("<section id=\"codex-heatmaps\"").unwrap();
    let end = html.find("<section id=\"codex-layouts\"").unwrap();
    golden(
        "account-footprints.html",
        &format!("{}\n", &html[start..end]),
        FOOTPRINTS_HTML_GOLDEN,
    );
    for id in [
        "codex-may-use",
        "codex-mode-crossings",
        "codex-capabilities",
        "codex-impls",
    ] {
        assert!(html.contains(&format!("<section id=\"{id}\"")), "{id}");
    }
    assert!(html.contains("class=\"unused\""));
    assert!(html.contains("declared but unused</strong>: pending"));
    assert!(html.contains("rejected: T is bounded by mod affine"));
    assert!(html.contains(">Hard-Realtime Safe<"));
}

/// (query, expected hit paths in rank order).
const PACKAGE_QUERIES: &[(&str, &[&str])] = &[
    (
        "u64 -> u64",
        &[
            "src/audit.ves::double",
            "src/audit.ves::report",
            "src/ledger.ves::fee",
            "src/models.ves::keep",
        ],
    ),
    (
        "u64 -> u64 eff []",
        &[
            "src/audit.ves::double",
            "src/ledger.ves::fee",
            "src/models.ves::keep",
        ],
    ),
    (
        "in Entry -> u64",
        &[
            "src/models.ves::Entry as Score::score",
            "src/models.ves::model ByCount::score",
        ],
    ),
    (
        "Score",
        &[
            "src/models.ves::Entry as Score::score",
            "src/models.ves::model ByCount::score",
        ],
    ),
    ("ByCount", &["src/models.ves::model ByCount::score"]),
    // T binds consistently: bool everywhere, or no match.
    ("u64, bool -> bool", &["src/models.ves::tag"]),
    ("u64, bool -> u64", &[]),
    // tag's T is bounded by mod copy: an affine argument is refused.
    ("u64, affine String -> String", &[]),
    ("sink linear Ledger -> Ledger", &["src/models.ves::keep"]),
    (
        "inout Ledger.{total, count}, u64 -> ()",
        &["src/ledger.ves::post"],
    ),
    (
        "in Ledger -> u64 eff [Journal]",
        &["src/ledger.ves::persist"],
    ),
    ("post", &["src/ledger.ves::post"]),
];

#[test]
fn package_index_spans_files_impls_models_and_generic_instantiation() {
    let dir = Scratch::package();
    let out = vesper(&dir.0, &["doc", ".", "--json"]);
    ok(&out);
    let report = json(&out);
    assert_eq!(report["package"]["name"], "@example/codex-package");
    golden(
        "codex-package-search-index.json",
        &pretty(&report["search_index"]),
        PACKAGE_INDEX_GOLDEN,
    );
    golden(
        "codex-package-capabilities.json",
        &pretty(&report["capabilities"]),
        PACKAGE_CAPS_GOLDEN,
    );
    // The Telemetry effect is outside the manifest ceiling.
    assert_eq!(
        report["capabilities"]["outside_ceiling"],
        serde_json::json!([["src/audit.ves::report", "Telemetry"]])
    );
    let files: Vec<&str> = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["src/audit.ves", "src/ledger.ves", "src/models.ves"]);
    let models = find(&report["files"], "file", "src/models.ves");
    assert_eq!(models["impl_methods"].as_array().unwrap().len(), 2);

    for (query, expected) in PACKAGE_QUERIES {
        let out = vesper(&dir.0, &["doc", ".", "--json", "--search", query]);
        ok(&out);
        let hits: Vec<String> = json(&out)["text_search"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(hits, *expected, "query {query}");
    }
    let keep = json(&vesper(
        &dir.0,
        &["doc", ".", "--json", "--search", "u64 -> u64"],
    ));
    let hit = find(&keep["text_search"]["hits"], "name", "keep");
    assert_eq!(hit["instantiation"], serde_json::json!([["T", "u64"]]));
    assert_eq!(hit["exact"], false);
    assert_eq!(hit["href"], "files/src/models.ves.html#item-keep");
    let double = find(&keep["text_search"]["hits"], "name", "double");
    assert_eq!(double["href"], "files/src/audit.ves.html#item-double");

    // Package mode needs --json or --bundle and refuses single-file options.
    for args in [
        &["doc", "."][..],
        &["doc", ".", "-o", "x.html"],
        &["doc", ".", "--json", "--wire", "w.json"],
    ] {
        let out = vesper(&dir.0, args);
        assert!(!out.status.success(), "{args:?}");
    }
    // A file that does not check fails the package, naming it.
    fs::write(
        dir.path("src/broken.ves"),
        "fn broken() -> u64 {\n    missing\n}\n",
    )
    .unwrap();
    let out = vesper(&dir.0, &["doc", ".", "--json"]);
    assert!(!out.status.success());
    let error = json(&out)["error"].as_str().unwrap().to_string();
    assert!(
        error.starts_with("src/broken.ves: source check failed"),
        "{error}"
    );
}

#[test]
fn package_bundle_links_every_page_and_matches_the_page_script() {
    let dir = Scratch::package();
    let out = vesper(&dir.0, &["doc", ".", "--bundle", "site"]);
    ok(&out);
    for file in [
        "index.html",
        "search.html",
        "search-index.json",
        "files/src/audit.ves.html",
        "files/src/ledger.ves.html",
        "files/src/models.ves.html",
    ] {
        assert!(dir.path("site").join(file).is_file(), "{file}");
    }
    let index = fs::read_to_string(dir.path("site/index.html")).unwrap();
    assert!(index.contains("href=\"files/src/ledger.ves.html\""));
    assert!(index.contains("href=\"search.html\""));
    assert!(index.contains(
        "<strong>src/audit.ves::report</strong> uses <code>Telemetry</code> outside the ceiling"
    ));
    let page = fs::read_to_string(dir.path("site/files/src/models.ves.html")).unwrap();
    assert!(page.contains("href=\"../../search.html\""));
    assert!(page.contains("Entry as Score::score"));
    assert!(page.contains("model ByCount::score"));
    let search = fs::read_to_string(dir.path("site/search.html")).unwrap();
    for text in [&index, &page, &search] {
        // The only URL is the SVG namespace, which is never fetched.
        let text = text.replace("http://www.w3.org/2000/svg", "");
        assert!(!text.contains("http://") && !text.contains("https://"));
    }
    assert!(
        !vesper(&dir.0, &["doc", ".", "--bundle", "site"])
            .status
            .success()
    );

    let index_text = fs::read_to_string(dir.path("site/search-index.json")).unwrap();
    let index = hoogle::parse_index(&index_text).unwrap();
    let node = Command::new("node").arg("--version").output();
    if !node.is_ok_and(|o| o.status.success()) {
        eprintln!("node not found: skipping the package search-page parity check");
        return;
    }
    let script = search
        .split("<script>const CODEX_HREF=\"index.html\";\n")
        .nth(1)
        .and_then(|s| s.split("</script>").next())
        .unwrap();
    assert_eq!(script, hoogle::SEARCH_JS);
    let mut queries: Vec<&str> = PACKAGE_QUERIES.iter().map(|(q, _)| *q).collect();
    queries.extend([
        "_ -> _",
        "(u64, _) -> ()",
        "Option[_] -> _",
        "a b",
        "sink _ -> _",
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
fn olean_certificates_publish_from_the_proof_audit_in_one_step() {
    let broker = include_str!("../crates/vesper-doc/examples/broker.ves");
    let olean = b"\x00one-step olean bytes";
    let audit = audit_for("CodexBroker", olean);
    let dir = Scratch::new(&[
        ("broker.ves", broker.as_bytes()),
        ("audit.json", audit.as_bytes()),
        ("build/CodexBroker.olean", olean),
    ]);
    let args = [
        "doc",
        "broker.ves",
        "--json",
        "--proof-audit",
        "audit.json",
        "--olean-store",
        "store",
        "--olean-root",
        "build",
    ];
    // No separate store-olean step: the store is created and linked at once.
    let out = vesper(&dir.0, &args);
    ok(&out);
    let report = json(&out);
    let commit = find(&report["badges"], "item", "commit");
    assert_eq!(commit["level"], "proved");
    assert_eq!(commit["certificates"][0]["status"], "linked");
    let object = commit["certificates"][0]["object_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(object.starts_with("b3v1:artifact:blob:"), "{object}");
    // Idempotent, and a later lookup without the root finds the same object.
    ok(&vesper(&dir.0, &args));
    let later = json(&vesper(&dir.0, &args[..7]));
    assert_eq!(
        find(&later["badges"], "item", "commit")["certificates"][0]["object_id"],
        object.as_str()
    );
    // A bundle copies the published bytes.
    ok(&vesper(
        &dir.0,
        &[
            "doc",
            "broker.ves",
            "--bundle",
            "site",
            "--proof-audit",
            "audit.json",
            "--olean-store",
            "store",
            "--olean-root",
            "build",
        ],
    ));
    let copied = dir
        .path("site/certs")
        .join(format!("{}.olean", blake3::hash(olean).to_hex()));
    assert_eq!(fs::read(copied).unwrap(), olean);
    // Bytes that differ from the audit are refused, and --olean-root needs a store.
    fs::write(dir.path("build/CodexBroker.olean"), b"tampered").unwrap();
    let fresh = [
        "doc",
        "broker.ves",
        "--json",
        "--proof-audit",
        "audit.json",
        "--olean-store",
        "other-store",
        "--olean-root",
        "build",
    ];
    let tampered = vesper(&dir.0, &fresh);
    assert!(!tampered.status.success());
    assert!(
        json(&tampered)["error"]
            .as_str()
            .unwrap()
            .contains("differ from the audited size/BLAKE3")
    );
    let no_store = vesper(
        &dir.0,
        &[
            "doc",
            "broker.ves",
            "--proof-audit",
            "audit.json",
            "--olean-root",
            "build",
        ],
    );
    assert!(!no_store.status.success());
}

#[test]
fn package_doctests_share_one_module_across_files() {
    let dir = Scratch::package();
    let out = vesper(&dir.0, &["test", "--doc", "--json"]);
    ok(&out);
    let report = json(&out);
    assert_eq!(report["package_module"], true);
    assert_eq!(report["batch"]["module_count"], 1);
    // charged (ledger.ves) and doubled (audit.ves) share the module; the
    // aggregate example falls back to the evaluator.
    assert_eq!(report["batch"]["entry_count"], 2);
    assert_eq!(report["jit_examples"], 2);
    assert_eq!(report["evaluator_examples"], 1);
    let engines: Vec<(String, String)> = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["examples"] == true)
        .flat_map(|f| {
            let file = f["file"].as_str().unwrap().to_string();
            f["report"]["examples"]
                .as_array()
                .unwrap()
                .iter()
                .map(move |e| (file.clone(), e["engine"].as_str().unwrap().to_string()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        engines,
        [
            ("src/audit.ves".to_string(), "jit".to_string()),
            ("src/ledger.ves".to_string(), "evaluator".to_string()),
            ("src/ledger.ves".to_string(), "jit".to_string()),
        ]
    );
    let text = vesper(&dir.0, &["test", "--doc"]);
    ok(&text);
    assert!(
        String::from_utf8_lossy(&text.stdout)
            .contains("package module: 2 entry point(s) in one shared JIT module")
    );
}

#[test]
fn package_doctests_fall_back_per_file_when_names_collide() {
    let file = |entry: &str| {
        format!(
            "/// Helper.\n///\n/// ```vesper,entry={entry}\n/// fn {entry}() -> u64 {{\n///     helper()\n/// }}\n/// ```\nfn helper() -> u64 {{\n    7\n}}\n"
        )
    };
    let (a, b) = (file("first"), file("second"));
    let dir = Scratch::new(&[
        (
            "Loom.toml",
            b"[package]\nname = \"@example/collide\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
        ),
        ("src/a.ves", a.as_bytes()),
        ("src/b.ves", b.as_bytes()),
    ]);
    let out = vesper(&dir.0, &["test", "--doc", "--json"]);
    ok(&out);
    let report = json(&out);
    assert_eq!(report["package_module"], false);
    assert!(
        report["package_fallback"]
            .as_str()
            .unwrap()
            .contains("top-level names must be disjoint"),
        "{report}"
    );
    for f in report["files"].as_array().unwrap() {
        assert_eq!(f["passed"], true);
        assert_eq!(f["report"]["batch"]["module_count"], 1);
        assert_eq!(f["report"]["examples"][0]["engine"], "jit");
    }
}
