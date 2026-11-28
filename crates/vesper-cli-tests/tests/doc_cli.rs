mod support;

use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::Output,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_doc::Type;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "vesper-doc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str, text: &str) -> String {
        let p = self.0.join(name);
        fs::write(&p, text).unwrap();
        p.to_str().unwrap().into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(args: &[&str]) -> Output {
    support::vesper().args(args).output().unwrap()
}
fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)))
}
const SOURCE: &str = include_str!("../../../crates/vesper-doc/examples/api.ves");
#[test]
fn cli_json_html_and_local_output_are_bounded_escaped_and_deterministic() {
    let fixture = Fixture::new();
    let source = fixture.write("api.ves", SOURCE);
    let out = run(&["doc", &source, "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(report(&out)["status"], "valid");
    let a = run(&["doc", &source]);
    let b = run(&["doc", &source]);
    assert!(a.status.success());
    assert_eq!(a.stdout, b.stdout);
    let html = String::from_utf8(a.stdout).unwrap();
    assert!(html.contains("&lt;b&gt;native&lt;/b&gt;"));
    assert!(!html.contains("<b>native</b>"));
    let output = fixture.0.join("docs.html");
    let out = run(&["doc", &source, "-o", output.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(fs::read(&output).unwrap(), html.as_bytes());
    let out = run(&["doc", &source, "-o", output.to_str().unwrap(), "--json"]);
    assert!(!out.status.success());
    assert_eq!(report(&out)["status"], "invalid");
    assert_eq!(fs::read(output).unwrap(), html.as_bytes());
}
#[test]
fn cli_structural_search_rejects_equal_mode_vectors_with_incompatible_full_types() {
    let fixture = Fixture::new();
    let source = fixture.write(
        "types.ves",
        "fn number(in x: i64) -> i64 { x } fn boolean(in x: bool) -> bool { x }",
    );
    let query = vesper_doc::Query {
        parameters: vec![vesper_doc::QueryParameter {
            pass: vesper_doc::Pass::In,
            mode: vesper_doc::Mode::Copy,
            ty: Type::named("bool"),
            selector: vesper_doc::Selector::All,
        }],
        result: Type::named("bool"),
        result_max_mode: vesper_doc::Mode::Copy,
        effect_ceiling: Default::default(),
    };
    let query = fixture.write("query.json", &serde_json::to_string(&query).unwrap());
    let out = run(&["doc", &source, "--query", &query, "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = report(&out);
    assert_eq!(r["search"]["matches"].as_array().unwrap().len(), 1);
    assert_eq!(r["search"]["matches"][0]["function"], "boolean");
}
#[test]
fn invalid_source_arguments_utf8_and_source_budgets_create_no_output() {
    let fixture = Fixture::new();
    let source = fixture.write("bad.ves", "fn broken() -> bool { 1 }");
    let dest = fixture.0.join("bad.html");
    let out = run(&["doc", &source, "--json", "-o", dest.to_str().unwrap()]);
    assert!(!out.status.success());
    assert_eq!(report(&out)["status"], "invalid");
    assert!(!dest.exists());
    for args in [
        vec!["doc", "--json"],
        vec!["doc", "--bad", "--json"],
        vec!["doc", &source, "extra", "--json"],
        vec!["doc", &source, "-o", "--json"],
    ] {
        let out = run(&args);
        assert!(!out.status.success());
        assert_eq!(report(&out)["status"], "invalid");
    }
    fs::write(&source, [0xff]).unwrap();
    assert!(!run(&["doc", &source, "--json"]).status.success());
    fs::write(&source, vec![b' '; 131_073]).unwrap();
    assert!(!run(&["doc", &source, "--json"]).status.success());
    assert!(run(&["doc", "--help"]).status.success());
}

#[test]
fn wcsd_stack_measures_standalone_and_nested_realtime_calls_across_targets() {
    let fixture = Fixture::new();
    let source = fixture.write(
        "realtime_chain.ves",
        "fn pure_fn(in x: u64) -> u64 { wrapping_add(x, 1) }
         fn leaf(in x: u64) -> u64 eff [Realtime] { wrapping_add(x, 1) }
         fn step(in x: u64) -> u64 eff [Realtime] { wrapping_add(leaf(x), 2) }
         fn top(in x: u64) -> u64 eff [Realtime] { wrapping_add(step(x), 3) }
         fn generic_rt[T](sink x: T) -> T eff [Realtime] { x }",
    );

    // Host default target works without --target.
    let host_out = run(&["doc", &source, "--wcsd-stack", "--json"]);
    assert!(
        host_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&host_out.stderr)
    );
    let host_json = report(&host_out);
    let host_items = host_json["capabilities"]["items"].as_array().unwrap();
    let host_top = host_items.iter().find(|i| i["item"] == "top").unwrap();
    assert!(
        host_top["wcsd_stack"]["worst_case_bytes"]
            .as_u64()
            .is_some(),
        "expected numeric WCSD on host: {host_top}"
    );
    assert_eq!(
        host_top["wcsd_stack"]["worst_path"],
        serde_json::json!(["top", "step", "leaf"])
    );

    // Explicit x86_64 and AArch64 targets both measure call chains cross-platform.
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let out = run(&["doc", &source, "--wcsd-stack", "--target", triple, "--json"]);
        assert!(
            out.status.success(),
            "target {triple} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let parsed = report(&out);
        let items = parsed["capabilities"]["items"].as_array().unwrap();
        let top = items.iter().find(|i| i["item"] == "top").unwrap();
        let step = items.iter().find(|i| i["item"] == "step").unwrap();
        let leaf = items.iter().find(|i| i["item"] == "leaf").unwrap();
        let top_bytes = top["wcsd_stack"]["worst_case_bytes"].as_u64().unwrap();
        let step_bytes = step["wcsd_stack"]["worst_case_bytes"].as_u64().unwrap();
        let leaf_bytes = leaf["wcsd_stack"]["worst_case_bytes"].as_u64().unwrap();
        assert_eq!(top["wcsd_stack"]["target"], triple);
        assert_eq!(
            top["wcsd_stack"]["worst_path"],
            serde_json::json!(["top", "step", "leaf"])
        );
        assert!(top_bytes >= step_bytes && step_bytes >= leaf_bytes);
        let rt_badge = top["badges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["capability"] == "hard_realtime")
            .unwrap();
        assert!(
            rt_badge["evidence"].as_str().unwrap().contains(&format!(
                "{top_bytes} bytes on {triple} via top -> step -> leaf"
            )),
            "unexpected badge evidence: {rt_badge}"
        );

        let generic = items.iter().find(|i| i["item"] == "generic_rt").unwrap();
        assert!(generic["wcsd_stack"]["worst_case_bytes"].is_null());
        assert!(
            generic["wcsd_stack"]["unavailable_reason"]
                .as_str()
                .unwrap()
                .contains("generic")
        );

        let pure = items.iter().find(|i| i["item"] == "pure_fn").unwrap();
        assert!(pure.get("wcsd_stack").is_none());
    }

    // HTML output renders WCSD pills only when --wcsd-stack is requested.
    let plain_html = run(&["doc", &source]);
    assert!(plain_html.status.success());
    let plain_str = String::from_utf8(plain_html.stdout).unwrap();
    assert!(!plain_str.contains("wcsd-bound"));

    let wcsd_html = run(&[
        "doc",
        &source,
        "--wcsd-stack",
        "--target",
        "aarch64-unknown-linux-gnu",
    ]);
    assert!(wcsd_html.status.success());
    let wcsd_str = String::from_utf8(wcsd_html.stdout).unwrap();
    assert!(wcsd_str.contains("class=\"wcsd-bound\""));
    assert!(wcsd_str.contains("WCSD unavailable"));
}

#[test]
fn wcsd_stack_rejects_target_without_flag_and_unsupported_targets() {
    let fixture = Fixture::new();
    let source = fixture.write(
        "rt.ves",
        "fn fee(in amount: u64) -> u64 eff [Realtime] { wrapping_add(amount, 1) }",
    );

    let missing_flag = run(&[
        "doc",
        &source,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--json",
    ]);
    assert!(!missing_flag.status.success());
    let err_json = report(&missing_flag);
    assert!(
        err_json["error"]
            .as_str()
            .unwrap()
            .contains("--target requires --wcsd-stack")
    );

    let bad_target = run(&[
        "doc",
        &source,
        "--wcsd-stack",
        "--target",
        "riscv64-unknown-none-elf",
        "--json",
    ]);
    assert!(!bad_target.status.success());
    let bad_json = report(&bad_target);
    assert!(
        bad_json["error"]
            .as_str()
            .unwrap()
            .contains("x86_64 or AArch64")
    );
}

#[test]
fn wcsd_stack_works_for_packages_and_bundles() {
    let fixture = Fixture::new();
    let pkg = fixture.0.join("rtpkg");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::write(
        pkg.join("Loom.toml"),
        "[package]\nname = \"@test/rtpkg\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Realtime\"]\n",
    )
    .unwrap();
    fs::write(
        pkg.join("src/dsp.ves"),
        "fn mix(in a: u64, in b: u64) -> u64 eff [Realtime] { wrapping_add(a, b) }",
    )
    .unwrap();

    let json_out = run(&[
        "doc",
        pkg.to_str().unwrap(),
        "--wcsd-stack",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--json",
    ]);
    assert!(
        json_out.status.success(),
        "{}",
        String::from_utf8_lossy(&json_out.stderr)
    );
    let parsed = report(&json_out);
    let mix = parsed["files"][0]["capabilities"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["item"] == "mix")
        .unwrap();
    assert!(mix["wcsd_stack"]["worst_case_bytes"].as_u64().is_some());

    let bundle_dir = fixture.0.join("bundle");
    let bundle_out = run(&[
        "doc",
        pkg.to_str().unwrap(),
        "--wcsd-stack",
        "--target",
        "aarch64-unknown-linux-gnu",
        "--bundle",
        bundle_dir.to_str().unwrap(),
    ]);
    assert!(
        bundle_out.status.success(),
        "{}",
        String::from_utf8_lossy(&bundle_out.stderr)
    );
    let page = fs::read_to_string(bundle_dir.join("files/src/dsp.ves.html")).unwrap();
    assert!(page.contains("class=\"wcsd-bound\""));
}
