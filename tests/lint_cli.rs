use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture {
    dir: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new(source: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-lint-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("source.ves");
        fs::write(&path, source).unwrap();
        Self { dir, path }
    }
    fn run(&self, args: &[&str]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vesper"));
        c.arg("lint").arg(&self.path);
        c.args(args);
        c.output().unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "stderr={} stdout={}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn text(&self) -> String {
        fs::read_to_string(&self.path).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn findings(report: &Value, code: &str) -> Vec<Value> {
    report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == code)
        .cloned()
        .collect()
}
fn check(path: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("check")
        .arg(path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
#[test]
fn unused_effects_fix_rechecks_disappears_and_is_semver_compatible() {
    let source = "effect Audit { fn event(); }\nfn identity(x: u64) -> u64 eff [/* keep */ Audit] { x }\nfn main() -> u64 eff [Audit] { identity(42) }\n";
    let f = Fixture::new(source);
    let report = f.json(&["--json"]);
    assert_eq!(findings(&report, "L0102").len(), 2);
    assert_eq!(f.text(), source);
    let fixed = f.json(&["--fix", "--semver-safe", "--json"]);
    assert_eq!(fixed["changed"], true);
    assert!(fixed["applied_edits"].as_u64().unwrap() > 0);
    assert!(f.text().contains("/* keep */"));
    check(&f.path);
    let run = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("run")
        .arg(&f.path)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "42");
    assert!(findings(&f.json(&["--json"]), "L0102").is_empty());
    assert_eq!(f.json(&["--fix", "--json"])["changed"], false);
}
#[test]
fn direct_and_transitive_performed_effects_remain() {
    let f = Fixture::new(
        "linear effect Clock { fn tick() -> u64; } effect Spare { fn idle(); }\nfn leaf() -> u64 eff [Clock, Spare] { Clock::tick() }\nfn middle() -> u64 eff [Clock, Spare] { leaf() }\nfn main() -> u64 eff [Clock, Spare] { middle() }\n",
    );
    let before = f.json(&["--json"]);
    let unused = findings(&before, "L0102");
    assert_eq!(unused.len(), 3);
    assert!(
        unused
            .iter()
            .all(|d| d["detail"]["effect"]["name"] == "Spare")
    );
    f.json(&["--fix", "--semver-safe", "--json"]);
    assert!(findings(&f.json(&["--json"]), "L0102").is_empty());
    assert_eq!(f.text().matches("eff [Clock").count(), 3);
    check(&f.path);
}
#[test]
fn generic_instances_and_capability_labels_are_preserved() {
    let source = "effect Mark[T: mod copy] { fn touch(in value: T); }\nfn generic[T: mod copy](in value: T) eff [Mark[T]] { Mark::touch(in value); }\nfn main() eff [left: Mark[u64], right: Mark[u64], Mark[u32]] { left.touch(1); generic[u64](2); }\n";
    let f = Fixture::new(source);
    let r = f.json(&["--json"]);
    let unused = findings(&r, "L0102");
    assert_eq!(unused.len(), 1, "{r}");
    assert_eq!(unused[0]["detail"]["effect"]["args"][0]["Named"][0], "u32");
    f.json(&["--fix", "--json"]);
    let next = f.json(&["--json"]);
    assert!(findings(&next, "L0102").is_empty());
    assert!(f.text().contains("left: Mark[u64], right: Mark[u64]"));
    check(&f.path);
}
#[test]
fn semver_safe_rejects_unsupported_named_interface_and_preserves_original() {
    let source = "effect E { fn act(); } fn main() eff [local: E] { }";
    let f = Fixture::new(source);
    let output = f.run(&["--json", "--fix", "--semver-safe"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("semver-safe"));
    assert_eq!(f.text(), source);
    assert_eq!(f.json(&["--fix", "--json"])["changed"], true);
    assert!(findings(&f.json(&["--json"]), "L0102").is_empty());
}
#[test]
fn handlers_closures_indirect_open_rows_and_recursion_are_conservative() {
    let cases = [
        "effect E { fn ask() -> u64; } effect Spare { fn no(); } fn main() -> u64 eff [Spare] { handle E in { E::ask() } with { return x => x, ask(k) => 42, } }",
        "effect Spare { fn no(); } fn main() -> u64 eff [Spare] { let f = || -> u64 { 42 }; f() }",
        "effect Spare { fn no(); } fn invoke(in f: fn@copy() -> u64) -> u64 eff [Spare] { f() }",
        "effect Spare { fn no(); } fn open[r: eff]() eff [Spare | r] { }",
        "effect Spare { fn no(); } fn recur() eff [Spare] { recur() }",
    ];
    for source in cases {
        let f = Fixture::new(source);
        let r = f.json(&["--json"]);
        assert!(findings(&r, "L0102").is_empty(), "{r}");
        assert!(!r["skipped"].as_array().unwrap().is_empty());
        assert_eq!(f.json(&["--fix", "--json"])["changed"], false);
        assert_eq!(f.text(), source);
    }
    let source = "effect E { fn ask() -> u64; } fn opaque() -> u64 { handle E in { E::ask() } with { return x=>x, ask(k)=>42, } } fn unused() eff [E] {}";
    let f = Fixture::new(source);
    assert!(!f.run(&["--fix", "--json"]).status.success());
    assert_eq!(f.text(), source);
}
#[test]
fn checked_projection_accesses_include_callee_selectors_and_shadowed_locals() {
    let source = "copy struct Pair { left: u64, right: u64 }\nfn write_left(inout p.{left}: Pair) { p.left = 7; }\nfn update(inout p: Pair) { write_left(inout p); let p = 0; p; }\n";
    let f = Fixture::new(source);
    let r = f.json(&["--json"]);
    let projection = findings(&r, "L0101");
    assert_eq!(projection.len(), 1, "{r}");
    assert_eq!(projection[0]["function"], "update");
    assert_eq!(
        projection[0]["detail"]["mutable_paths"],
        serde_json::json!([["left"]])
    );
    assert_eq!(projection[0]["fixable"], false);
    assert_eq!(
        f.json(&["--fix", "--semver-safe", "--json"])["changed"],
        false
    );
    assert_eq!(f.text(), source);
}
#[test]
fn projection_read_siblings_and_whole_root_calls_are_not_guessed() {
    let f = Fixture::new(
        "copy struct Pair { left: u64, right: u64 } fn change(inout p: Pair) { p.left = p.right; } fn whole(in p: Pair) {} fn broad(inout p: Pair) { whole(in p); p.left=1; }",
    );
    let r = f.json(&["--json"]);
    let d = findings(&r, "L0101");
    assert_eq!(d.len(), 1, "{r}");
    assert_eq!(d[0]["function"], "change");
    assert_eq!(d[0]["detail"]["read_paths"], serde_json::json!([["right"]]));
}
#[test]
fn arithmetic_effects_and_linear_cleanup_are_not_removed() {
    let source = "linear struct Ticket { value: u64 } effect Spare { fn no(); } fn consume(sink t: Ticket) -> u64 eff [Spare] { let Ticket { value } = t; value } fn add(x:u64,y:u64) -> u64 eff [Fail[ArithOverflow], Spare] { x+y }";
    let f = Fixture::new(source);
    let r = f.json(&["--json"]);
    let d = findings(&r, "L0102");
    assert_eq!(d.len(), 2, "{r}");
    assert!(d.iter().all(|f| f["detail"]["effect"]["name"] == "Spare"));
    f.json(&["--fix", "--semver-safe", "--json"]);
    check(&f.path);
    assert!(f.text().contains("Fail[ArithOverflow]"));
    assert!(findings(&f.json(&["--json"]), "L0102").is_empty());
    let bad = "linear struct Ticket { value:u64 } effect Spare {fn no();} fn leak(sink t:Ticket) eff [Spare] { }";
    let f = Fixture::new(bad);
    let o = f.run(&["--fix", "--json"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("E030"));
    assert_eq!(f.text(), bad);
}
#[test]
fn malformed_mismatched_instances_and_bounded_reads_never_replace_input() {
    for source in [
        "fn broken( {",
        "effect Mark[T: mod copy] {fn touch(in x:T);} fn wrong() eff [Mark[u32]] { Mark::touch[u64](1); }",
    ] {
        let f = Fixture::new(source);
        assert!(!f.run(&["--fix", "--json"]).status.success());
        assert_eq!(f.text(), source);
    }
    let source = " ".repeat(262145);
    let f = Fixture::new(&source);
    assert!(!f.run(&["--fix", "--json"]).status.success());
    assert_eq!(f.text(), source);
    let source = format!(
        "fn deep() -> u64 {{ {}1{} }}",
        "(".repeat(33),
        ")".repeat(33)
    );
    let f = Fixture::new(&source);
    assert!(!f.run(&["--fix", "--json"]).status.success());
    assert_eq!(f.text(), source);
}
#[test]
fn token_edits_preserve_comments_and_remove_only_unused_instances() {
    let source = "linear effect Kept { fn value() -> u64; } effect Spare {fn no();} fn main()->u64 eff [/* before */ Spare /* after */, // keep line\n Kept, /* middle */ Spare] { Kept::value() }";
    // Identical unnamed declarations remain distinct source entries. Both are
    // removable here because neither is performed.
    let f = Fixture::new(source);
    f.json(&["--fix", "--json"]);
    assert!(findings(&f.json(&["--json"]), "L0102").is_empty());
    let source = "linear effect Kept { fn value() -> u64; } effect A {fn no();} effect B {fn no();} fn main()->u64 eff [/* before */ A /* after */, // keep line\n Kept, /* middle */ B,] { Kept::value() }";
    let f = Fixture::new(source);
    f.json(&["--fix", "--json"]);
    let after = f.text();
    for comment in [
        "/* before */",
        "/* after */",
        "// keep line",
        "/* middle */",
    ] {
        assert!(after.contains(comment));
    }
    check(&f.path);
    assert!(findings(&f.json(&["--json"]), "L0102").is_empty());
}
#[test]
fn help_options_and_no_partial_temporary_files() {
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["lint", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("--semver-safe"));
    let f = Fixture::new("effect E{fn no();} fn main() eff[E]{}");
    assert!(!f.run(&["--semver-safe"]).status.success());
    assert!(!f.run(&["--unknown"]).status.success());
    assert_eq!(fs::read_dir(&f.dir).unwrap().count(), 1);
}
#[test]
#[cfg(unix)]
fn symlink_and_readonly_failures_preserve_sources() {
    let source = "effect E{fn no();} fn main() eff[E]{}";
    let f = Fixture::new(source);
    let link = f.dir.join("link.ves");
    std::os::unix::fs::symlink(&f.path, &link).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("lint")
        .arg(&link)
        .args(["--fix", "--json"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert_eq!(f.text(), source);
    let mut permission = fs::metadata(&f.path).unwrap().permissions();
    permission.set_readonly(true);
    fs::set_permissions(&f.path, permission).unwrap();
    assert!(!f.run(&["--fix", "--json"]).status.success());
    assert_eq!(f.text(), source);
    assert_eq!(fs::read_dir(&f.dir).unwrap().count(), 2);
}

#[test]
fn elseif_unary_and_result_type_recursion_are_bounded_before_parse() {
    let chain = format!("fn main()->u64{{ {}1 }}", "if true {0} else ".repeat(65));
    let f = Fixture::new(&chain);
    let o = f.run(&["--fix", "--json"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("conditional limit"));
    assert_eq!(f.text(), chain);
    let unary = format!("fn main()->bool{{ {}true }}", "!".repeat(257));
    let f = Fixture::new(&unary);
    assert!(!f.run(&["--fix", "--json"]).status.success());
    assert_eq!(f.text(), unary);
    let arrows = format!("fn main()->{}u64;", "fn()->".repeat(65));
    let f = Fixture::new(&arrows);
    assert!(!f.run(&["--fix", "--json"]).status.success());
    assert_eq!(f.text(), arrows);
}

#[test]
fn compact_nominal_expansion_dags_are_refused_before_checking() {
    let mut source = "copy struct N0 { value:u64 }\n".to_owned();
    for i in 1..25 {
        source.push_str(&format!(
            "copy struct N{i} {{ left:N{}, right:N{} }}\n",
            i - 1,
            i - 1
        ));
    }
    source.push_str("effect E{fn no();} fn main() eff[E]{}");
    assert!(source.len() < 2000);
    let f = Fixture::new(&source);
    let o = f.run(&["--fix", "--json"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("nominal expansion budget"));
    assert_eq!(f.text(), source);
    let source = "copy struct Recursive[T] { child:Recursive[T] } fn main() {}";
    let f = Fixture::new(source);
    let o = f.run(&["--fix", "--json"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("recursive nominal"));
    assert_eq!(f.text(), source);
}
#[test]
fn proved_total_checked_arithmetic_does_not_keep_unused_failure_authority() {
    let f = Fixture::new("fn main()->u64 eff[Fail[ArithOverflow]]{1+2}");
    let before = f.json(&["--json"]);
    assert_eq!(findings(&before, "L0102").len(), 1);
    f.json(&["--fix", "--semver-safe", "--json"]);
    assert!(findings(&f.json(&["--json"]), "L0102").is_empty());
    check(&f.path);
}

#[test]
fn generic_nominal_dag_budget_and_small_instantiations_are_distinguished() {
    let small = "copy struct Leaf[T:mod copy]{value:T} copy struct Pair[T:mod copy]{left:Leaf[T],right:Leaf[T]} fn inspect(in p:Pair[u64]){}";
    let f = Fixture::new(small);
    assert_eq!(f.json(&["--json"])["status"], "checked");
    let mut source = "copy struct G0[T:mod copy]{value:T}\n".to_owned();
    for i in 1..22 {
        source.push_str(&format!(
            "copy struct G{i}[T:mod copy]{{left:G{}[T],right:G{}[T]}}\n",
            i - 1,
            i - 1
        ));
    }
    source.push_str("fn main(){}");
    let f = Fixture::new(&source);
    let output = f.run(&["--json", "--fix"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("nominal expansion budget"));
    assert_eq!(f.text(), source);
}

#[test]
fn result_propagation_retains_the_actual_checked_error_instance() {
    let source = "fn propagate(r:Result[u64,u32])->u64 eff[Fail[u32],Fail[u64]]{r?}";
    let f = Fixture::new(source);
    let before = f.json(&["--json"]);
    let unused = findings(&before, "L0102");
    assert_eq!(unused.len(), 1, "{before}");
    assert_eq!(unused[0]["detail"]["effect"]["args"][0]["Named"][0], "u64");
    f.json(&["--fix", "--semver-safe", "--json"]);
    assert!(f.text().contains("Fail[u32]"));
    assert!(!f.text().contains("Fail[u64]"));
    assert!(findings(&f.json(&["--json"]), "L0102").is_empty());
    check(&f.path);
}

#[test]
fn inferred_type_doubling_and_brace_reset_expression_chains_fail_before_checking() {
    let nested = format!(
        "fn dup[T:mod copy](in x:T)->(T,T){{(x,x)}} fn main(){{{}0{};}}",
        "dup(".repeat(25),
        ")".repeat(25)
    );
    let f = Fixture::new(&nested);
    let o = f.run(&["--fix", "--json"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("duplicating generic"));
    assert_eq!(f.text(), nested);
    let mut tuple = "fn main(){let x=0;".to_owned();
    for _ in 0..25 {
        tuple.push_str("let x=(x,x);");
    }
    tuple.push('}');
    let f = Fixture::new(&tuple);
    let o = f.run(&["--fix", "--json"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("composite tuple"));
    assert_eq!(f.text(), tuple);
    let chain = format!("fn main()->u64{{1{}}}", "+{1}".repeat(300));
    let f = Fixture::new(&chain);
    let o = f.run(&["--fix", "--json"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("recursive-expression token"));
    assert_eq!(f.text(), chain);
    let single = format!(
        "fn identity[T:mod copy](in x:T)->T{{x}} fn main()->u64{{{}0{}}}",
        "identity(".repeat(25),
        ")".repeat(25)
    );
    let f = Fixture::new(&single);
    assert_eq!(f.json(&["--json"])["status"], "checked");
}

#[test]
fn result_construction_is_supported_only_with_explicit_closed_types() {
    let supported =
        "effect Spare{fn no();} fn result()->Result[u64,u32] eff[Spare]{Result::Ok[u64,u32](42)}";
    let f = Fixture::new(supported);
    assert_eq!(findings(&f.json(&["--json"]), "L0102").len(), 1);
    f.json(&["--fix", "--semver-safe", "--json"]);
    check(&f.path);
    let unsupported = "fn result()->Result[u64,u32]{Result::Ok(42)}";
    let f = Fixture::new(unsupported);
    let output = f.run(&["--fix", "--json"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("explicit closed type arguments"));
    assert_eq!(f.text(), unsupported);
}
