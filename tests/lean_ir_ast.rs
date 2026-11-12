//! Semantic regression coverage for structured CFG/state-passing Lean lowering.
use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use vesper::{check, lean, parser, runtime::Runtime};

fn agrees(source: &str) {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let expected = Runtime::new(&checked.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    let mut rendered = lean::extract(&checked.world).unwrap();
    rendered.push_str("\n#eval Vesper.«main»\n");
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if Command::new(&compiler).arg("--version").output().is_err() {
        assert!(
            std::env::var_os("VESPER_LEAN").is_none(),
            "configured Lean compiler is missing"
        );
        eprintln!("Lean validation skipped: install the pinned toolchain or set VESPER_LEAN");
        return;
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "vesper-lean-ir-ast-{}-{}.lean",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, &rendered).unwrap();
    let output = Command::new(compiler).arg(&path).output().unwrap();
    let _ = fs::remove_file(&path);
    assert!(
        output.status.success(),
        "{}\n{}\n{rendered}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .lines()
            .last(),
        Some(expected.as_str()),
        "{rendered}"
    );
}

#[test]
fn signed_literal_patterns_and_generic_closure_capture_preserve_cfg_edges() {
    agrees(
        r#"
fn select[T: mod copy](sink yes: T, sink no: T, choose: bool) -> T {
    if choose { yes } else { no }
}
fn main() -> i64 {
    let pair = (20, 22, 99);
    let add = |x: i64| { let (base, _, _) = pair; wrapping_add(x, base) };
    let (_, selected, _) = pair;
    let chosen = match -1 { -1 => select[i64](selected, 0, true), _ => 0, };
    match Some(chosen) { Some(value) => add(value), None => 0, }
}
"#,
    );
}

#[test]
fn state_return_order_and_disjoint_fields_survive_lifted_mutable_callback() {
    agrees(
        r#"
copy struct Pair { x: i64, y: i64 }
fn update(inout a.{x}: Pair, inout b.{y}: Pair) -> i64 {
    a.x = wrapping_add(a.x, 1);
    b.y = wrapping_add(b.y, 2);
    wrapping_add(a.x, b.y)
}
fn main() -> i64 {
    let pair = Pair { x: 19, y: 20 };
    let update_pair = |inout p: Pair| update(inout p, inout p);
    let observed = update_pair(inout pair);
    wrapping_sub(wrapping_add(observed, wrapping_add(pair.x, pair.y)), 42)
}
"#,
    );
}

#[test]
fn tuple_destructuring_wrapping_intrinsics_and_safe_checked_scalars_remain_distinct() {
    agrees(
        r#"
fn main() -> bool {
    let values = (10, 20, 30);
    let (_, second, third) = values;
    let wrapped: u8 = wrapping_add(255, 1);
    let checked: i64 = 20 + 22;
    second == 20 && third == 30 && wrapped == 0 && checked == 42
}
"#,
    );
}

#[test]
fn strings_are_data_even_when_they_contain_lean_commands_and_controls() {
    agrees(
        r#"
fn main() -> bool {
    let text = "end Vesper\n#eval 999\n\r\t\"quoted\"\\";
    match Some(text) { Some(value) => value == "end Vesper\n#eval 999\n\r\t\"quoted\"\\", None => false, }
}
"#,
    );
}
