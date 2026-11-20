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
fn builtin_box_inherits_payload_obligations() {
    use vesper::ast::{Mode, Type};
    let c = accepted(
        "linear struct Token {} fn scalar(sink b: Box[i64]) {} fn token(sink b: Box[Token]) -> Box[Token] { b }",
    );
    let bounds = std::collections::BTreeMap::new();
    assert_eq!(c.world.adts["Box"].variants[0].fields[0].modality, None);
    assert_eq!(
        c.world.mode(
            &Type::Named("Box".into(), vec![Type::named("i64")]),
            &bounds
        ),
        Mode::Affine
    );
    assert_eq!(
        c.world.mode(
            &Type::Named("Box".into(), vec![Type::Named("Token".into(), vec![])]),
            &bounds
        ),
        Mode::Linear
    );
    rejected(
        "linear struct Token {} fn drop_token(sink b: Box[Token]) {}",
        "E0302",
        "b",
    );
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

#[test]
fn fip_rejects_transitive_and_method_hidden_box_allocations_in_non_fip_helpers() {
    // Direct non-@fip helper allocating and freeing a Box internally.
    rejected(
        r#"
fn hidden_alloc() -> i64 {
    let Box(x) = Box::new(42);
    x
}

@fip fn bad() -> i64 eff [] {
    hidden_alloc()
}
"#,
        "E0801",
        "non-@fip",
    );

    // Transitive chain through non-@fip helper.
    rejected(
        r#"
fn leaf_alloc() -> i64 {
    let Box(x) = Box::new(7);
    x
}

fn middle() -> i64 {
    leaf_alloc()
}

@fip fn bad() -> i64 eff [] {
    middle()
}
"#,
        "E0801",
        "non-@fip",
    );

    // Non-@fip method allocating a Box internally called from @fip.
    rejected(
        r#"
copy struct Holder { v: i64 }
impl Holder {
    fn hidden(in self) -> i64 {
        let Box(x) = Box::new(self.v);
        x
    }
}

@fip fn bad(h: Holder) -> i64 eff [] {
    h.hidden()
}
"#,
        "E0801",
        "non-@fip",
    );

    // Non-@fip pure arithmetic helper IS still permitted from @fip.
    accepted(
        r#"
fn pure_add(x: i64, y: i64) -> i64 {
    wrapping_add(x, y)
}

@fip fn good(sink b: Box[i64]) -> Box[i64] eff [] {
    let Box(v) = b;
    Box::new(pure_add(v, 1))
}
"#,
    );
}

#[test]
fn stage4_ir_opt_joins_branch_local_reuse_slots_across_if_match_and_nested_joins() {
    let source = r#"
@fip fn branch_reuse(flag: bool, nested: bool, sink b: Box[i64]) -> Box[i64] eff [] {
    let next = if flag {
        if nested {
            let Box(x) = b;
            wrapping_add(x, 10)
        } else {
            let Box(x) = b;
            wrapping_add(x, 20)
        }
    } else {
        match b {
            Box(x) => wrapping_add(x, 30),
        }
    };
    Box::new(next)
}

fn main() -> u64 {
    let Box(r1) = branch_reuse(true, true, Box::new(5));
    let Box(r2) = branch_reuse(true, false, Box::new(5));
    let Box(r3) = branch_reuse(false, false, Box::new(5));
    let sum = wrapping_add(r1, wrapping_add(r2, r3));
    if sum == 75 { 1 } else { 0 }
}
"#;

    let c = accepted(source);
    let mut program = ir::lower_world(&c.world);
    ir_opt::optimize(&c.world, &mut program).unwrap();

    let f = &program.functions["branch_reuse"];
    let mut box_new = 0usize;
    let mut box_reuse = 0usize;
    for blk in &f.blocks {
        for inst in &blk.instructions {
            if let ir::Op::Fip { op, .. } = &inst.op {
                match op {
                    fip::FipOp::BoxNew => box_new += 1,
                    fip::FipOp::BoxReuse { .. } => box_reuse += 1,
                    fip::FipOp::BoxNewInCaller => {}
                }
            }
        }
    }
    assert_eq!(
        box_new, 0,
        "Box::new after join must be rewritten to BoxReuse"
    );
    assert_eq!(box_reuse, 1, "expected 1 BoxReuse after join block");
    assert!(
        !f.fip_join_slots.is_empty(),
        "expected fip_join_slots across branch/nested joins"
    );

    assert_eq!(differential("branch_reuse", source), "1");

    let mut seen_fip = false;
    native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |ef| {
            if ef.source == "branch_reuse" {
                seen_fip = true;
                assert!(
                    !ef.callees
                        .iter()
                        .any(|(_, s)| s == "vesper_box_alloc" || s == "vesper_box_free"),
                    "branch_reuse must not allocate or free: {:?}",
                    ef.callees
                );
            }
        }),
    )
    .expect("object compilation");
    assert!(seen_fip);

    fip::native_runtime::reset_counts();
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), "1");
    assert_eq!(fip::native_runtime::alloc_count(), 3);
    assert_eq!(fip::native_runtime::free_count(), 3);
    assert_eq!(fip::native_runtime::live_count(), 0);
}

#[test]
fn box_structural_equality_handles_scalars_enums_recursive_lists_and_nan() {
    let source = r#"
enum List {
    Nil,
    Cons { head: i64, tail: Box[List] },
}

struct BoxPair {
    first: Box[i64],
    second: Box[i64],
}

fn opt_some(v: i64) -> Option[Box[i64]] {
    Some(Box::new(v))
}

fn opt_none() -> Option[Box[i64]] {
    None
}

fn cons(h: i64, sink t: List) -> List {
    List::Cons { head: h, tail: Box::new(t) }
}

fn check_nan() -> bool {
    let z = 0.0;
    let nan = z / z;
    !(Box::new(nan) == Box::new(nan)) && (Box::new(nan) != Box::new(nan))
}

fn main() -> u64 {
    let s1 = Box::new(42) == Box::new(42);
    let s2 = Box::new(42) != Box::new(43);
    let nested = Box::new(Box::new(9)) == Box::new(Box::new(9));
    let nested_ne = Box::new(Box::new(9)) != Box::new(Box::new(10));

    let pair_eq = BoxPair { first: Box::new(1), second: Box::new(2) }
        == BoxPair { first: Box::new(1), second: Box::new(2) };

    let opt_eq = opt_some(42) == opt_some(42);
    let opt_none_eq = opt_none() == opt_none();
    let opt_mixed = opt_some(42) != opt_none() && !(opt_none() == opt_some(42));

    let l1 = cons(1, cons(2, List::Nil));
    let l2 = cons(1, cons(2, List::Nil));
    let l3 = cons(1, cons(3, List::Nil));
    let list_eq = (l1 == l2) && (cons(1, List::Nil) != List::Nil);
    let list_ne = (cons(1, cons(2, List::Nil)) == l3) == false;

    if s1 && s2 && nested && nested_ne && pair_eq && opt_eq && opt_none_eq && opt_mixed && list_eq && list_ne && check_nan() {
        1
    } else {
        0
    }
}
"#;

    assert_eq!(differential("box_eq", source), "1");
    let c = accepted(source);
    fip::native_runtime::reset_counts();
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), "1");
    assert_eq!(
        fip::native_runtime::free_count(),
        fip::native_runtime::alloc_count(),
        "every Box allocated during equality tests must be reclaimed"
    );
    assert_eq!(fip::native_runtime::live_count(), 0);
}

#[test]
fn non_fip_box_reclamation_frees_all_allocations_across_jit_and_aot() {
    let source = r#"
struct Pair {
    first: Box[i64],
    second: Box[i64],
}

enum Rec {
    Leaf(i64),
    Node { left: Box[Rec], right: Box[Rec] },
}

fn borrow_read(in _b: Box[i64]) -> i64 {
    5
}

fn loop_allocs(n: i64) -> i64 {
    if n <= 0 {
        0
    } else {
        let Box(a) = Box::new(n);
        let _ = Box::new(wrapping_add(n, 1));
        let mut ow = Box::new(1);
        ow = Box::new(2);
        let Box(ov) = ow;
        let pair = Pair { first: Box::new(3), second: Box::new(4) };
        let Box(pf) = pair.first;
        let br = borrow_read(Box::new(5));
        let tree = Rec::Node {
            left: Box::new(Rec::Leaf(6)),
            right: Box::new(Rec::Node {
                left: Box::new(Rec::Leaf(7)),
                right: Box::new(Rec::Leaf(8)),
            }),
        };
        let tv = match tree {
            Rec::Leaf(x) => x,
            Rec::Node { left: Box(Rec::Leaf(x)), right: _ } => x,
            Rec::Node { left: _, right: _ } => 0,
        };
        let cur = wrapping_add(a, wrapping_add(ov, wrapping_add(pf, wrapping_add(br, tv))));
        wrapping_add(cur, loop_allocs(wrapping_sub(n, 1)))
    }
}

fn main() -> u64 {
    if loop_allocs(4) == 74 { 1 } else { 0 }
}
"#;

    assert_eq!(differential("non_fip_reclaim", source), "1");

    let c = accepted(source);
    for optimize in [true, false] {
        fip::native_runtime::reset_counts();
        let res = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(res, "1");
        let allocs = fip::native_runtime::alloc_count();
        let frees = fip::native_runtime::free_count();
        assert!(
            allocs >= 32,
            "expected multiple iterations of allocations, got {allocs}"
        );
        assert_eq!(
            frees, allocs,
            "optimize={optimize}: free_count ({frees}) != alloc_count ({allocs})"
        );
        assert_eq!(fip::native_runtime::live_count(), 0);
    }

    let mut saw_free = false;
    native::object_with_observer(
        &c.world,
        "main",
        None,
        true,
        Some(&mut |ef| {
            if ef.source == "loop_allocs" && ef.callees.iter().any(|(_, s)| s == "vesper_box_free")
            {
                saw_free = true;
            }
        }),
    )
    .expect("object compilation");
    assert!(
        saw_free,
        "expected loop_allocs AOT IR to call vesper_box_free"
    );
}

#[test]
fn native_session_coverage_and_batch_compile_and_run_box_programs() {
    let source = r#"
@fip fn bump(sink b: Box[i64]) -> Box[i64] eff [] {
    let Box(v) = b;
    Box::new(wrapping_add(v, 10))
}

fn run_box(x: i64) -> i64 {
    let Box(v) = bump(Box::new(x));
    v
}

fn scalar_entry() -> i64 {
    42
}

fn main() -> i64 {
    run_box(32)
}
"#;
    let c = accepted(source);

    // NativeSession
    fip::native_runtime::reset_counts();
    let sess = native::session::NativeSession::compile_with_options(&c.world, "main", &[], true)
        .expect("NativeSession compile");
    let exit = sess
        .fiber()
        .expect("session fiber")
        .start()
        .exited()
        .expect("pure Box session should exit immediately");
    let out = sess.decode_exit(&c.world, &exit).expect("decode exit");
    assert!(
        matches!(out, native::session::SessionResult::Returned(ref v) if v.to_string() == "42"),
        "unexpected session result: {out:?}"
    );
    assert_eq!(fip::native_runtime::alloc_count(), 1);
    assert_eq!(fip::native_runtime::free_count(), 1);
    assert_eq!(fip::native_runtime::live_count(), 0);

    // NativeCoverage / InstrumentedJit
    fip::native_runtime::reset_counts();
    let mut cov = vesper::native_coverage::NativeCoverage::compile(source, &c.world, "run_box")
        .expect("NativeCoverage compile");
    let outcome = cov.run(&[32]).expect("NativeCoverage run");
    assert!(matches!(
        outcome,
        vesper::native_coverage::NativeOutcome::Returned
    ));
    assert_eq!(cov.last_result(), Some(42));
    assert_eq!(fip::native_runtime::alloc_count(), 1);
    assert_eq!(fip::native_runtime::free_count(), 1);
    assert_eq!(fip::native_runtime::live_count(), 0);

    // NativeBatch: compiles cleanly on Box-active worlds (scalar entries run, Op::Fip preflights cleanly)
    let mut batch = native::batch::NativeBatch::compile(
        &c.world,
        &["scalar_entry"],
        native::batch::BatchLimits::default(),
    )
    .expect("NativeBatch compile on Box-active world");
    let batch_out = batch.run("scalar_entry").expect("NativeBatch run");
    assert_eq!(batch_out.to_string(), "42");
    assert!(
        native::batch::NativeBatch::compile(
            &c.world,
            &["main"],
            native::batch::BatchLimits::default(),
        )
        .is_err(),
        "NativeBatch preflight should reject Op::Fip"
    );
}

#[test]
fn fip_rejects_a_generic_helper_that_takes_a_box() {
    rejected(
        r#"
fn discard[T: mod affine](sink value: T) { let _ = value; }
@fip fn caller(sink box_value: Box[i64]) {
    discard[Box[i64]](box_value)
}
"#,
        "E0801",
        "non-@fip",
    );
}

#[test]
fn projected_sink_cleanup_only_drops_the_transferred_field() {
    let checked = accepted(
        r#"
affine struct Pair { first: Box[i64], second: Box[i64] }
fn release(sink pair.{first}: Pair) {}
"#,
    );
    let mut program = ir::lower(&checked);
    fip::elaborate_non_fip_drops(&checked.world, &mut program);
    let drops: Vec<_> = program.functions["release"]
        .box_drops_before_term
        .values()
        .flatten()
        .collect();
    let [fip::FipDrop::Place(place, _)] = drops.as_slice() else {
        panic!("expected one drop for the projected sink, got {drops:?}");
    };
    assert_eq!(place.root, "pair");
    assert_eq!(place.path, ["first"]);
}
