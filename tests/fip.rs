//! Zero-RC Functional-But-In-Place (`@fip`) Constructor Reuse & `Box[T]` (§12.4):
//! parsing, formatting, recovery, Chroma Tier 0 highlighting, static verifier
//! (`E0800`–`E0809`), Stage 4 `egglog` ReuseSlot pairing (`ir_opt`), zero-alloc
//! native CLIF & JIT `alloc_count` verification, CEK vs JIT (unopt/opt) vs AOT
//! differential execution on §12.4 Red-Black tree `balance_left`, and Lean 4
//! extraction + compiler evaluation with `Box` erasure and recursive `RbNode`.

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, fip, ir, ir_opt, lean, native, parser, runtime::Runtime};
use vesper_highlight::{VesperTokenClass as C, highlight_tier0, verify_lossless_spans};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-fip-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const SECTION_12_4_RB_TREE: &str = r#"enum Color { Red, Black }

affine struct RbNode[K, V] {
    color: Color,
    key: K,
    val: V,
    left: Option[Box[RbNode[K, V]]],
    right: Option[Box[RbNode[K, V]]],
}

@fip fn balance_left[K: mod copy, V](
    color: Color,
    key: K,
    sink val: V,
    sink left: Option[Box[RbNode[K, V]]],
    sink right: Option[Box[RbNode[K, V]]],
) -> Box[RbNode[K, V]] eff [] {
    match left {
        Some(Box(RbNode {
            color: Color::Red,
            key: lk, val: lv,
            left: Some(Box(RbNode { color: Color::Red, key: llk, val: llv, left: a, right: b })),
            right: c,
        })) => {
            let new_left  = Box::new(RbNode { color: Color::Black, key: llk, val: llv, left: a, right: b });
            let new_right = Box::new(RbNode { color: Color::Black, key, val, left: c, right });
            Box::new_in_caller(RbNode { color: Color::Red, key: lk, val: lv, left: Some(new_left), right: Some(new_right) })
        }
        other_left => Box::new_in_caller(RbNode { color, key, val, left: other_left, right }),
    }
}
"#;

const SECTION_12_4_RUNNABLE: &str = r#"copy enum Color { Red, Black }

affine struct RbNode[K, V] {
    color: Color,
    key: K,
    val: V,
    left: Option[Box[RbNode[K, V]]],
    right: Option[Box[RbNode[K, V]]],
}

@fip fn balance_left[K: mod copy, V](
    color: Color,
    key: K,
    sink val: V,
    sink left: Option[Box[RbNode[K, V]]],
    sink right: Option[Box[RbNode[K, V]]],
) -> Box[RbNode[K, V]] eff [] {
    match left {
        Some(Box(RbNode {
            color: Color::Red,
            key: lk, val: lv,
            left: Some(Box(RbNode { color: Color::Red, key: llk, val: llv, left: a, right: b })),
            right: c,
        })) => {
            let new_left  = Box::new(RbNode { color: Color::Black, key: llk, val: llv, left: a, right: b });
            let new_right = Box::new(RbNode { color: Color::Black, key, val, left: c, right });
            Box::new_in_caller(RbNode { color: Color::Red, key: lk, val: lv, left: Some(new_left), right: Some(new_right) })
        }
        other_left => Box::new_in_caller(RbNode { color, key, val, left: other_left, right }),
    }
}
"#;

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c
}

fn rejected(source: &str, code: &str, needle: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(needle)),
        "expected {code} containing {needle:?}, got {:#?}",
        c.diagnostics
    );
}

fn executable(name: &str, source: &str) -> (Option<i32>, String) {
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    fs::write(&file, source).unwrap();
    let exe = temp.0.join(name);
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{name}: vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(&exe).output().unwrap();
    (
        run.status.code(),
        String::from_utf8_lossy(&run.stderr).into_owned(),
    )
}

fn differential(name: &str, source: &str) -> String {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();

    fip::native_runtime::reset_counts();
    let jit = native::jit(&c.world, "main")
        .unwrap_or_else(|e| panic!("{name}: JIT failed: {e}"))
        .to_string();
    assert_eq!(jit, evaluated, "{name}: JIT vs evaluator");

    fip::native_runtime::reset_counts();
    let jit_unopt = native::jit_with_options(&c.world, "main", false)
        .unwrap_or_else(|e| panic!("{name}: unopt JIT failed: {e}"))
        .to_string();
    assert_eq!(jit_unopt, evaluated, "{name}: unopt JIT vs evaluator");

    if cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        && let Ok(val) = evaluated.parse::<u8>()
    {
        let (status, stderr) = executable(name, source);
        assert_eq!(status, Some(i32::from(val)), "{name}: AOT stderr={stderr}");
    }
    evaluated
}

#[test]
fn parse_format_recovery_and_chroma_highlight_section_12_4() {
    let parsed = parser::parse(SECTION_12_4_RB_TREE);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let bal = parsed
        .module
        .items
        .iter()
        .find_map(|item| match item {
            vesper::ast::Item::Function(f) if f.name == "balance_left" => Some(f),
            _ => None,
        })
        .expect("balance_left parsed");
    assert!(bal.fip);

    // `@fip` works before or after `pub` and inside `impl` blocks.
    let forms = r#"
@fip pub fn a(sink b: Box[i64]) -> Box[i64] eff [] {
    let Box(v) = b;
    Box::new(wrapping_add(v, 1))
}
pub @fip fn b(v: i64) -> Box[i64] eff [] {
    Box::new_in_caller(wrapping_add(v, 2))
}
copy struct Wrap { x: i64 }
impl Wrap {
    @fip fn id(sink self, sink b: Box[i64]) -> Box[i64] eff [] {
        let Box(v) = b;
        Box::new(wrapping_add(v, self.x))
    }
}
"#;
    let c_forms = accepted(forms);
    assert!(c_forms.world.functions["a"].fip);
    assert!(c_forms.world.functions["b"].fip);
    assert!(c_forms.world.functions["Wrap::id"].fip);

    // Formatting idempotence preserves `@fip`.
    let fmt1 = vesper_fmt::format_source(SECTION_12_4_RB_TREE, vesper_fmt::Limits::default())
        .expect("format pass 1");
    assert!(fmt1.output.contains("@fip fn balance_left"));
    let fmt2 = vesper_fmt::format_source(&fmt1.output, vesper_fmt::Limits::default())
        .expect("format pass 2");
    assert_eq!(fmt1.output, fmt2.output);

    // Recovery mode handles partial `@fip` signature.
    let broken = "@fip fn broken(";
    let recovered = parser::parse_recovery(broken, Default::default()).unwrap();
    assert!(!recovered.declarations().is_empty());

    // Chroma Tier 0 highlighting covers all spans losslessly and tags `@fip`.
    let spans = highlight_tier0(SECTION_12_4_RB_TREE).unwrap();
    verify_lossless_spans(SECTION_12_4_RB_TREE.len(), &spans).unwrap();
    assert!(spans.iter().any(|s| {
        matches!(&SECTION_12_4_RB_TREE[s.start..s.end], "@" | "fip") && s.class == C::KeywordControl
    }));
}

#[test]
fn static_verifier_diagnostics_e0800_to_e0809_and_user_box_compat() {
    // E0800: `Box(...)` value literal rejected when built-in prelude is active.
    rejected(
        r#"
fn bad() -> Box[i64] { Box(42) }
"#,
        "E0800",
        "Box::new or Box::new_in_caller",
    );

    // E0801: `@fip` cannot declare or call `Alloc`.
    rejected(
        r#"
effect Alloc { fn alloc() -> i64; }
@fip fn bad(x: i64) -> i64 eff [Alloc] { x }
"#,
        "E0801",
        "Alloc",
    );
    rejected(
        r#"
fn helper(sink b: Box[i64]) -> Box[i64] {
    let Box(v) = b;
    Box::new(wrapping_add(v, 1))
}
@fip fn bad(sink b: Box[i64]) -> Box[i64] eff [] {
    helper(b)
}
"#,
        "E0801",
        "non-@fip",
    );
    rejected(
        r#"
@fip fn bad(sink f: fn(i64) -> i64, x: i64) -> i64 eff [] { f(x) }
"#,
        "E0801",
        "indirect calls",
    );

    // E0802: `Box::new_in_caller` forbidden in non-@fip functions.
    rejected(
        r#"
fn bad(x: i64) -> Box[i64] { Box::new_in_caller(x) }
"#,
        "E0802",
        "only permitted inside @fip",
    );

    // E0804: `Box::new` without a matching ReuseSlot.
    rejected(
        r#"
@fip fn bad(x: i64) -> Box[i64] eff [] {
    let b = Box::new(x);
    b
}
"#,
        "E0804",
        "ReuseSlot",
    );
    rejected(
        r#"
@fip fn bad_size(sink b: Box[u8]) -> (Box[(i64, i64)], Box[u8]) eff [] {
    let Box(x) = b;
    let big = Box::new((1, 2));
    (big, Box::new(x))
}
"#,
        "E0804",
        "ReuseSlot",
    );

    // E0805: unpacked ReuseSlot left unconsumed.
    rejected(
        r#"
@fip fn bad(sink b: Box[i64]) -> i64 eff [] {
    let Box(v) = b;
    v
}
"#,
        "E0805",
        "unconsumed Box ReuseSlot",
    );

    // E0806: dropping owned Box without unpacking/moving.
    rejected(
        r#"
@fip fn bad(sink b: Box[i64]) -> i64 eff [] {
    42
}
"#,
        "E0806",
        "cannot drop owned Box",
    );
    rejected(
        r#"
fn consume_borrow(in b: Box[i64]) -> i64 { 1 }
@fip fn bad(sink b: Box[i64]) -> i64 eff [] {
    let Box(v) = b;
    consume_borrow(Box::new(v))
}
"#,
        "E0806",
        "drops temporary Box",
    );

    // E0807: `Box::new_in_caller` return type mismatch.
    rejected(
        r#"
@fip fn bad(x: i64) -> i64 eff [] {
    let _ = Box::new_in_caller(x);
    x
}
"#,
        "E0807",
        "Box[T]",
    );

    // E0808: caller ReuseSlot used twice or on only one branch.
    rejected(
        r#"
@fip fn bad(sink a: Box[i64]) -> Box[i64] eff [] {
    let Box(v) = a;
    let first = Box::new_in_caller(v);
    let _b = first;
    Box::new_in_caller(wrapping_add(v, 1))
}
"#,
        "E0808",
        "already consumed",
    );
    rejected(
        r#"
@fip fn bad(flag: bool, sink b: Box[i64]) -> Box[i64] eff [] {
    if flag {
        b
    } else {
        let Box(v) = b;
        let reused = Box::new(v);
        Box::new_in_caller(v)
    }
}
"#,
        "E0808",
        "caller ReuseSlot",
    );

    // E0809: branches leave incompatible remaining ReuseSlots.
    rejected(
        r#"
@fip fn bad(flag: bool, sink a: Box[i64], sink b: Box[i64]) -> Box[i64] eff [] {
    let Box(va) = a;
    let Box(vb) = b;
    if flag {
        let reuse_a = Box::new(va);
        let _ = reuse_a;
        Box::new_in_caller(vb)
    } else {
        let x = Box::new(va);
        let y = Box::new(vb);
        Box::new_in_caller(wrapping_add(va, vb))
    }
}
"#,
        "E0809",
        "incompatible remaining Box ReuseSlots",
    );

    // Backwards compatibility: user-defined `struct Box[T]` disables the built-in prelude.
    let user_box = r#"
copy struct Box[T] { inner: T }
fn main() -> i64 {
    let b = Box { inner: 42 };
    b.inner
}
"#;
    let c = accepted(user_box);
    assert!(!fip::active(&c.world));
    assert_eq!(
        Runtime::new(&c.world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
        "42"
    );
}

#[test]
fn stage4_ir_opt_pairs_all_slots_in_balance_left_and_forwarded_tail_calls() {
    let src = format!(
        r#"{SECTION_12_4_RUNNABLE}
@fip fn forward_balance[K: mod copy, V](
    color: Color,
    key: K,
    sink val: V,
    sink left: Option[Box[RbNode[K, V]]],
    sink right: Option[Box[RbNode[K, V]]],
) -> Box[RbNode[K, V]] eff [] {{
    balance_left[K, V](color, key, val, left, right)
}}

@fip fn rotate_local(
    sink spare: Box[RbNode[i64, i64]],
    sink boxed_left: Box[RbNode[i64, i64]],
    sink right: Option[Box[RbNode[i64, i64]]],
) -> (RbNode[i64, i64], Box[RbNode[i64, i64]]) eff [] {{
    let Box(s) = spare;
    let rot = balance_left[i64, i64](Color::Black, 30, 300, Some(boxed_left), right);
    (s, rot)
}}
"#
    );
    let c = accepted(&src);
    let mut program = ir::lower_world(&c.world);
    ir_opt::optimize(&c.world, &mut program).unwrap();

    let bal = &program.functions["balance_left"];
    let mut box_new = 0usize;
    let mut box_reuse = 0usize;
    let mut box_new_in_caller = 0usize;
    for blk in &bal.blocks {
        for inst in &blk.instructions {
            if let ir::Op::Fip { op, .. } = &inst.op {
                match op {
                    fip::FipOp::BoxNew => box_new += 1,
                    fip::FipOp::BoxReuse { .. } => box_reuse += 1,
                    fip::FipOp::BoxNewInCaller => box_new_in_caller += 1,
                }
            }
        }
    }
    assert_eq!(
        box_new, 0,
        "all Box::new in balance_left must be rewritten to BoxReuse"
    );
    assert_eq!(
        box_reuse, 2,
        "balance_left must emit 2 BoxReuse instructions"
    );
    assert_eq!(
        box_new_in_caller, 2,
        "balance_left has 2 BoxNewInCaller instructions"
    );

    let fwd = &program.functions["forward_balance"];
    assert!(
        fwd.fip_call_slots
            .values()
            .any(|s| matches!(s, fip::CallSlot::CallerSlot)),
        "tail-forwarded balance_left call should use CallSlot::CallerSlot: {:?}",
        fwd.fip_call_slots
    );

    let rot = &program.functions["rotate_local"];
    assert!(
        rot.fip_call_slots
            .values()
            .any(|s| matches!(s, fip::CallSlot::ReuseSlot { .. })),
        "non-tail balance_left call unpacking ReuseSlot should receive ReuseSlot: {:?}",
        rot.fip_call_slots
    );
}

#[test]
fn balance_left_executes_identically_across_cek_jit_aot_with_zero_fip_allocs() {
    let source = format!(
        r#"{SECTION_12_4_RUNNABLE}

fn node_score(sink opt: Option[Box[RbNode[i64, i64]]], mult: i64) -> i64 {{
    match opt {{
        Some(Box(n)) => {{
            let c_score = match n.color {{ Color::Red => 1, Color::Black => 2 }};
            wrapping_add(c_score, wrapping_add(wrapping_mul(n.key, mult), n.val))
        }}
        None => 0,
    }}
}}

@fip fn fip_driver(
    sink l: Box[RbNode[i64, i64]],
    sink r: Box[RbNode[i64, i64]],
) -> Box[RbNode[i64, i64]] eff [] {{
    balance_left[i64, i64](Color::Black, 40, 4, Some(l), Some(r))
}}

fn check_rotation() -> u64 {{
    let a = Box::new(RbNode {{ color: Color::Black, key: 5, val: 50, left: None, right: None }});
    let b = Box::new(RbNode {{ color: Color::Black, key: 15, val: 150, left: None, right: None }});
    let c = Box::new(RbNode {{ color: Color::Black, key: 25, val: 250, left: None, right: None }});
    let r = Box::new(RbNode {{ color: Color::Black, key: 50, val: 500, left: None, right: None }});
    let ll = Box::new(RbNode {{ color: Color::Red, key: 10, val: 100, left: Some(a), right: Some(b) }});
    let l = Box::new(RbNode {{ color: Color::Red, key: 20, val: 200, left: Some(ll), right: Some(c) }});

    let Box(root) = fip_driver(l, r);
    // Expected after Red-Red left-left rotation:
    // root: Red (key=20, val=200)
    // root.left: Black (key=10, val=100, left=5, right=15)
    // root.right: Black (key=40, val=4, left=25, right=50)
    let root_ok = match root.color {{ Color::Red => root.key == 20 && root.val == 200, Color::Black => false }};
    let left_ok = match root.left {{
        Some(Box(nl)) => match nl.color {{
            Color::Black => nl.key == 10 && nl.val == 100 && node_score(nl.left, 1) == 57 && node_score(nl.right, 1) == 167,
            Color::Red => false,
        }},
        None => false,
    }};
    let right_ok = match root.right {{
        Some(Box(nr)) => match nr.color {{
            Color::Black => nr.key == 40 && nr.val == 4 && node_score(nr.left, 1) == 277 && node_score(nr.right, 1) == 552,
            Color::Red => false,
        }},
        None => false,
    }};

    // Fallback case (`other_left => Box::new_in_caller(...)`): black left node triggers no rotation.
    let bl = Box::new(RbNode {{ color: Color::Black, key: 7, val: 70, left: None, right: None }});
    let Box(fb) = balance_left[i64, i64](Color::Black, 99, 9, Some(bl), None);
    let fb_ok = match fb.color {{
        Color::Black => fb.key == 99 && fb.val == 9 && node_score(fb.left, 1) == 79,
        Color::Red => false,
    }};

    if root_ok && left_ok && right_ok && fb_ok {{ 1 }} else {{ 0 }}
}}

fn main() -> u64 {{
    check_rotation()
}}
"#
    );

    // 1) Differential run across CEK, JIT (opt & unopt), and AOT executable.
    let result = differential("balance_left", &source);
    assert_eq!(result, "1");

    // 2) Verify CLIF emitted for `balance_left` and `fip_driver` contains ZERO calls to `vesper_box_alloc`.
    let c = accepted(&source);
    let mut emitted_fip_fns = Vec::new();
    let (_, clif) = native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |ef| {
            if ef.source == "balance_left" || ef.source == "fip_driver" {
                let callees: Vec<String> = ef.callees.into_iter().map(|(_, s)| s).collect();
                emitted_fip_fns.push((ef.source, callees));
            }
        }),
    )
    .expect("object compilation");
    assert!(
        emitted_fip_fns.iter().any(|(n, _)| n == "balance_left"),
        "expected balance_left in emitted functions, got CLIF:\n{clif}"
    );
    for (name, callees) in &emitted_fip_fns {
        assert!(
            !callees.iter().any(|c| c == "vesper_box_alloc"),
            "@fip function `{name}` must not reference `vesper_box_alloc`, got callees {callees:?}"
        );
    }

    // 3) Verify exact JIT `alloc_count()` == 9 (zero inside @fip functions).
    fip::native_runtime::reset_counts();
    let jit = native::jit(&c.world, "main").unwrap().to_string();
    assert_eq!(jit, "1");
    assert_eq!(
        fip::native_runtime::alloc_count(),
        9,
        "expected only the 9 non-@fip allocations (zero inside @fip functions)"
    );
}

#[test]
fn lean4_extraction_erases_box_emits_inductive_rbnode_and_evaluates_balance_left() {
    let source = format!(
        r#"{SECTION_12_4_RUNNABLE}

fn main() -> u64 {{
    let a = Box::new(RbNode {{ color: Color::Black, key: 5, val: 50, left: None, right: None }});
    let b = Box::new(RbNode {{ color: Color::Black, key: 15, val: 150, left: None, right: None }});
    let c = Box::new(RbNode {{ color: Color::Black, key: 25, val: 250, left: None, right: None }});
    let r = Box::new(RbNode {{ color: Color::Black, key: 50, val: 500, left: None, right: None }});
    let ll = Box::new(RbNode {{ color: Color::Red, key: 10, val: 100, left: Some(a), right: Some(b) }});
    let l = Box::new(RbNode {{ color: Color::Red, key: 20, val: 200, left: Some(ll), right: Some(c) }});

    let Box(root) = balance_left[i64, i64](Color::Black, 40, 4, Some(l), Some(r));
    let left_key = match root.left {{
        Some(Box(nl)) => nl.key,
        None => 0,
    }};
    let right_key = match root.right {{
        Some(Box(nr)) => nr.key,
        None => 0,
    }};
    if root.key == 20 && left_key == 10 && right_key == 40 {{
        42
    }} else {{
        0
    }}
}}
"#
    );

    let c = accepted(&source);
    let expected = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(expected, "42");

    let mut rendered = lean::extract(&c.world).unwrap();
    assert!(
        !rendered.contains("structure «Box»") && !rendered.contains("inductive «Box»"),
        "Box must be completely erased in Lean extraction:\n{rendered}"
    );
    assert!(
        rendered.contains("inductive «RbNode»"),
        "recursive RbNode must extract as inductive after Box erasure:\n{rendered}"
    );

    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if Command::new(&compiler).arg("--version").output().is_err() {
        assert!(
            std::env::var_os("VESPER_LEAN").is_none(),
            "configured Lean compiler is missing"
        );
        eprintln!("Lean validation skipped: install the pinned toolchain or set VESPER_LEAN");
        return;
    }

    rendered.push_str("\n#eval Vesper.«main»\n");
    let temp = Temp::new();
    let lean_file = temp.0.join("fip_balance.lean");
    fs::write(&lean_file, &rendered).unwrap();
    let output = Command::new(&compiler).arg(&lean_file).output().unwrap();
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

    // Also run `vesper emit --emit lean` and `vesper jit` CLI end-to-end.
    let ves_file = temp.0.join("fip_balance.ves");
    fs::write(&ves_file, &source).unwrap();
    let emit_lean = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("emit")
        .arg(&ves_file)
        .arg("--emit")
        .arg("lean")
        .output()
        .unwrap();
    assert!(
        emit_lean.status.success(),
        "vesper emit --emit lean failed:\n{}\n{}",
        String::from_utf8_lossy(&emit_lean.stdout),
        String::from_utf8_lossy(&emit_lean.stderr)
    );
    let jit_cli = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("jit")
        .arg(&ves_file)
        .output()
        .unwrap();
    assert!(
        jit_cli.status.success() && String::from_utf8_lossy(&jit_cli.stdout).trim() == "42",
        "vesper jit failed:\n{}\n{}",
        String::from_utf8_lossy(&jit_cli.stdout),
        String::from_utf8_lossy(&jit_cli.stderr)
    );
}
