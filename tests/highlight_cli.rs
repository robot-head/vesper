//! `vesper highlight` acceptance (§43): Tier 0 losslessness over the
//! repository's `.ves` sources, Tier 1 facts from the real checker, the typed
//! Tier 0 fallback, and byte-exact HTML.
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};
use vesper_highlight::{css, highlight_tier0, highlight_tier0_tokens, verify_lossless_spans};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn vesper(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .current_dir(root())
        .output()
        .expect("run vesper")
}

fn fixture(name: &str) -> String {
    format!("tests/fixtures/highlight/{name}")
}

/// Every tracked-looking `.ves` file under the repository root, skipping
/// build output and hidden directories.
fn sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "target" || name.starts_with("bazel-") {
                continue;
            }
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut out = vec![];
    walk(&root(), &mut out);
    out.sort();
    out
}

#[test]
fn tier0_round_trips_every_repository_source_and_its_prefixes() {
    let files = sources();
    // Cargo sees all 340+; Bazel's root-package runfiles hold the root
    // package's own directories (examples/, tests/, packages/, bootstrap/).
    assert!(files.len() >= 300, "only {} .ves files found", files.len());
    let mut checked = 0usize;
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let spans = highlight_tier0(&src).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let joined: String = spans.iter().map(|s| &src[s.start..s.end]).collect();
        assert_eq!(joined, src, "{}", path.display());
        // Token mode (the wasm path) has the same token boundaries.
        let tokens = highlight_tier0_tokens(&src).unwrap();
        assert_eq!(tokens.len(), spans.len(), "{}", path.display());
        assert!(
            tokens
                .iter()
                .zip(&spans)
                .all(|(a, b)| (a.start, a.end) == (b.start, b.end))
        );
        // Truncated and incomplete prefixes: 24 cut points per file plus
        // every cut inside the first 64 bytes.
        let step = (src.len() / 24).max(1);
        let cuts = (0..src.len().min(64)).chain((0..src.len()).step_by(step));
        for cut in cuts {
            if !src.is_char_boundary(cut) {
                continue;
            }
            let prefix = &src[..cut];
            let spans = highlight_tier0(prefix)
                .unwrap_or_else(|e| panic!("{} prefix {cut}: {e}", path.display()));
            verify_lossless_spans(prefix.len(), &spans)
                .unwrap_or_else(|e| panic!("{} prefix {cut}: {e}", path.display()));
            checked += 1;
        }
    }
    eprintln!("{} files, {checked} prefixes round-tripped", files.len());
}

fn json(args: &[&str]) -> (Value, String) {
    let out = vesper(args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        serde_json::from_slice(&out.stdout).expect("JSON"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

struct Tok {
    text: String,
    class: String,
    flags: u64,
    hover: Option<String>,
}

fn tokens(doc: &Value, src: &str) -> Vec<Tok> {
    let hovers = doc["hovers"].as_array().unwrap();
    doc["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let start = s["byte_start"].as_u64().unwrap() as usize;
            let len = s["byte_len"].as_u64().unwrap() as usize;
            Tok {
                text: src[start..start + len].to_string(),
                class: s["class"].as_str().unwrap().to_string(),
                flags: s["semantic_flags"].as_u64().unwrap(),
                hover: s["hover_sym_id"]
                    .as_u64()
                    .map(|i| hovers[i as usize]["text"].as_str().unwrap().to_string()),
            }
        })
        .collect()
}

#[test]
fn tier1_marks_linear_inout_and_effect_facts_from_the_checker() {
    let path = fixture("wal_txn.ves");
    let src = std::fs::read_to_string(root().join(&path)).unwrap();
    let (doc, _) = json(&["highlight", "--json", &path]);
    assert_eq!(doc["schema"], "vesper.chroma.spans/1");
    assert_eq!(doc["tier"], 1);
    assert!(doc["fallback"].is_null());
    let toks = tokens(&doc, &src);
    let joined: String = toks.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(joined, src);
    let find = |text: &str| -> Vec<&Tok> { toks.iter().filter(|t| t.text == text).collect() };

    // `tx` is minted by the `let`, used by `abort` in one branch and
    // discharged by `commit` in the other: all three are live linear.
    let tx = find("tx");
    assert_eq!(tx.len(), 3);
    for t in &tx {
        assert_eq!(t.class, "linear-binding");
        assert_eq!(t.flags & 1, 1, "underline bit");
        assert_eq!(t.hover.as_deref(), Some("tx : WriteTxn (mod linear)"));
    }
    // `sink self` of the linear WriteTxn and its destructuring use.
    let selves: Vec<_> = find("self")
        .into_iter()
        .filter(|t| t.class == "linear-binding")
        .collect();
    assert_eq!(selves.len(), 4);
    assert_eq!(
        selves[0].hover.as_deref(),
        Some("sink self: WriteTxn (mod linear)")
    );
    // inout projection leaves: written on the left of `=`, read on the right.
    let index = find("index");
    // [0] is the struct field, [1] the `inout broker.{index, ..}` projection.
    assert_eq!(index[1].class, "field-leaf-mut");
    assert_eq!(index[2].class, "field-leaf-mut");
    assert_eq!(index[2].flags & 2, 2, "mutated-leaf bit");
    assert_eq!(index[3].class, "field-leaf-read");
    let max: Vec<_> = find("max_batch_bytes");
    assert!(max.iter().skip(1).all(|t| t.class == "field-leaf-read"));
    // `config` is a copy `in` binding: no linear decoration.
    assert!(find("config").iter().all(|t| t.flags & 1 == 0));
    // Effects from the checked World.
    assert!(find("Storage").iter().all(|t| t.class == "effect-linear"));
    assert!(find("Fail").iter().all(|t| t.class == "effect-abortable"));
    // The wire bytes agree with the records.
    let hex = doc["wire_hex"].as_str().unwrap();
    assert_eq!(hex.len(), toks.len() * 32);
}

#[test]
fn tier1_respects_block_scopes_and_destructuring() {
    // An inner linear `tx` shadows an outer copy `tx`; after the block the
    // outer one is visible again and is not linear.
    let path = fixture("shadowing.ves");
    let src = std::fs::read_to_string(root().join(&path)).unwrap();
    let (doc, _) = json(&["highlight", "--json", &path]);
    assert_eq!(doc["tier"], 1);
    let tx: Vec<_> = tokens(&doc, &src)
        .into_iter()
        .filter(|t| t.text == "tx")
        .collect();
    assert_eq!(tx.len(), 4);
    assert_eq!(tx[0].class, "pass-mode-in");
    assert_eq!(tx[0].hover.as_deref(), Some("tx : u64 (mod copy)"));
    assert_eq!(tx[1].class, "linear-binding");
    assert_eq!(tx[2].class, "linear-binding");
    assert_eq!(tx[3].class, "pass-mode-in", "outer tx after the block");
    assert_eq!(tx[3].flags & 1, 0);

    // `let Pair { left, right } = pair;` binds two linear Tickets.
    let path = "tests/fixtures/runtime_resource_trace/mutation.ves";
    let src = std::fs::read_to_string(root().join(path)).unwrap();
    let (doc, _) = json(&["highlight", "--json", path]);
    assert_eq!(doc["tier"], 1);
    let toks = tokens(&doc, &src);
    let routing = src.find("fn routing").unwrap();
    let mut offset = 0;
    let mut linear = vec![];
    for t in &toks {
        if offset > routing
            && (t.text == "left" || t.text == "right")
            && t.class == "linear-binding"
        {
            linear.push((t.text.clone(), t.hover.clone()));
        }
        offset += t.text.len();
    }
    assert!(linear.contains(&("left".into(), Some("left : Ticket (mod linear)".into()))));
    assert!(linear.contains(&("right".into(), Some("right : Ticket (mod linear)".into()))));
}

#[test]
fn implicit_arguments_take_the_callee_parameter_mode() {
    // `bump(c.hits)` passes `c.hits` to an `inout` parameter without a mode
    // keyword; the leaf is still a write.
    let path = fixture("implicit_inout.ves");
    let src = std::fs::read_to_string(root().join(&path)).unwrap();
    let (doc, _) = json(&["highlight", "--json", &path]);
    assert_eq!(doc["tier"], 1);
    let call = src.find("bump(c.hits)").unwrap();
    let (mut offset, mut leaves) = (0, vec![]);
    for t in tokens(&doc, &src) {
        if offset > call && (t.text == "hits" || t.text == "total") {
            leaves.push((t.text.clone(), t.class.clone(), t.flags & 2));
        }
        offset += t.text.len();
    }
    assert_eq!(
        leaves,
        [
            ("hits".into(), "field-leaf-mut".into(), 2),
            ("total".into(), "field-leaf-read".into(), 0)
        ]
    );
}

#[test]
fn inputs_past_the_analysis_budget_fall_back_quickly() {
    // Each `let x = (x, x);` doubles the inferred type; the guarded front end
    // stops it before the checker allocates.
    let dir = std::env::temp_dir().join(format!("vesper-highlight-budget-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("grow.ves");
    let text = format!(
        "fn main() -> u64 {{\n    let x = 0;\n{}    0\n}}\n",
        "    let x = (x, x);\n".repeat(40)
    );
    std::fs::write(&path, &text).unwrap();
    let start = std::time::Instant::now();
    let (doc, stderr) = json(&["highlight", "--json", path.to_str().unwrap()]);
    assert!(start.elapsed().as_secs() < 30, "{:?}", start.elapsed());
    assert_eq!(doc["tier"], 0);
    assert_eq!(doc["fallback"]["kind"], "budget");
    assert!(stderr.contains("analysis budget exceeded"), "{stderr}");
    let toks = tokens(&doc, &text);
    assert_eq!(
        toks.iter().map(|t| t.text.as_str()).collect::<String>(),
        text
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn snippets_that_do_not_check_fall_back_to_tier0_with_a_reason() {
    let path = fixture("abort_across_inout.ves");
    let (doc, stderr) = json(&["highlight", "--json", &path]);
    assert_eq!(doc["tier"], 0);
    assert_eq!(doc["fallback"]["kind"], "check");
    assert_eq!(doc["fallback"]["code"], "E0402");
    assert!(stderr.contains("Tier 1 unavailable"), "{stderr}");
    let src = std::fs::read_to_string(root().join(&path)).unwrap();
    let toks = tokens(&doc, &src);
    assert!(toks.iter().all(|t| t.class != "linear-binding"));
    // Tier 0 still classifies the projection leaves syntactically.
    assert!(
        toks.iter()
            .any(|t| t.text == "index" && t.class == "field-leaf-mut")
    );

    // An incomplete snippet: a syntax fallback, still lossless.
    let dir = std::env::temp_dir().join(format!("vesper-highlight-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let partial = dir.join("partial.ves");
    let text = "fn main( -> u64 eff [Fail[u64]] {\n    let tx = Storage::begin(";
    std::fs::write(&partial, text).unwrap();
    let (doc, _) = json(&["highlight", "--json", partial.to_str().unwrap()]);
    assert_eq!(doc["tier"], 0);
    assert_eq!(doc["fallback"]["kind"], "syntax");
    let toks = tokens(&doc, text);
    assert_eq!(
        toks.iter().map(|t| t.text.as_str()).collect::<String>(),
        text
    );
    // Asking for Tier 0 is not a failure.
    let (doc, stderr) = json(&[
        "highlight",
        "--json",
        "--tier",
        "0",
        &fixture("wal_txn.ves"),
    ]);
    assert_eq!(doc["tier"], 0);
    assert_eq!(doc["fallback"]["kind"], "not-requested");
    assert!(stderr.is_empty(), "{stderr}");
    std::fs::remove_dir_all(dir).ok();
}

/// Text a browser copies: drop `user-select: none` elements, strip tags,
/// decode entities.
fn copied_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    let mut hidden = 0usize;
    while let Some(lt) = rest.find('<') {
        if hidden == 0 {
            out.push_str(&rest[..lt]);
        }
        let gt = rest[lt..].find('>').unwrap() + lt;
        let tag = &rest[lt..=gt];
        if hidden > 0 {
            if tag.starts_with("</") {
                hidden -= 1;
            } else {
                hidden += 1;
            }
        } else if tag.contains("vc-gutter") || tag.contains("vc-twoslash-pin") {
            hidden = 1;
        }
        rest = &rest[gt + 1..];
    }
    out.push_str(rest);
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

#[test]
fn html_strips_back_to_the_source_byte_for_byte() {
    for (name, theme) in [("wal_txn.ves", "dawn"), ("abort_across_inout.ves", "dusk")] {
        let path = fixture(name);
        let src = std::fs::read_to_string(root().join(&path)).unwrap();
        let out = vesper(&["highlight", "--html", "--theme", theme, &path]);
        assert!(out.status.success());
        let html = String::from_utf8(out.stdout).unwrap();
        let prefix = format!("<pre class=\"vc\" data-vesper-theme=\"{theme}\" data-vc-tier=\"");
        assert!(html.starts_with(&prefix), "{html}");
        let inner = &html[html.find("<code>").unwrap() + 6..html.rfind("</code>").unwrap()];
        assert_eq!(copied_text(inner), src);
        assert!(html.contains("class=\"vc-gutter\" aria-hidden=\"true\""));
        if name == "wal_txn.ves" {
            assert!(html.contains("vc-l vc-lin-live"));
        }
    }
}

#[test]
fn css_and_ansi_and_argument_errors() {
    let out = vesper(&["highlight", "--css"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), css::stylesheet());
    let out = vesper(&["highlight", &fixture("wal_txn.ves")]);
    assert!(out.status.success());
    let ansi = String::from_utf8(out.stdout).unwrap();
    let src = std::fs::read_to_string(root().join(fixture("wal_txn.ves"))).unwrap();
    assert_eq!(vesper_highlight::ansi::strip(&ansi), src);
    for bad in [
        vec!["highlight"],
        vec!["highlight", "--theme", "noon", "x.ves"],
        vec!["highlight", "--tier", "2", "x.ves"],
        vec!["highlight", "--html", "--json", "x.ves"],
        vec!["highlight", "--css", "x.ves"],
        vec!["highlight", "--bogus"],
        vec!["highlight", "missing-file.ves"],
    ] {
        let out = vesper(&bad);
        assert!(!out.status.success(), "{bad:?}");
    }
    let help = vesper(&["highlight", "--help"]);
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("Usage: vesper highlight")
    );
}
