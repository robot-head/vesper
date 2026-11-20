//! Element views (§13, self-hosting SH2 lane 1, #1173): an element of a
//! `ChunkSeq[T]` or `Slice[T]` whose type is not copy is borrowed in place,
//! never copied out. `f(in s[i])` and `f(inout s[i])` lend it to a call (and
//! `s[i].m()` to a method), `swap` and `replace` exchange it, and
//! `match s.try_get(i)` / `match s.try_get_mut(i)` bind `Some(x)` to it
//! totally. The evaluator, both JIT configurations and an executable agree,
//! and the JIT frees every sequence exactly once.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-element-borrows-{}-{}",
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

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    let program = vesper::ir::lower_world(&c.world);
    let d = vesper::ir_check::validate(&c.world, &program);
    assert!(d.is_empty(), "Place IR check: {d:#?}");
    c
}

fn links() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Runs `main` in the evaluator, the JIT (optimizer on and off) and, where
/// a C compiler links, an executable; all must agree. The JIT must free
/// every sequence itself and never free one that is not live.
fn differential(name: &str, source: &str) -> u64 {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let stray = native_runtime::stray_frees();
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit, evaluated,
            "{name}: JIT (optimize {optimize}) vs evaluator"
        );
        assert_eq!(
            native_runtime::teardown_frees(),
            0,
            "{name}: JIT (optimize {optimize}) left sequences for teardown"
        );
        assert_eq!(
            native_runtime::stray_frees(),
            stray,
            "{name}: JIT (optimize {optimize}) freed a sequence that was not live"
        );
    }
    let expected: u64 = evaluated.parse().unwrap();
    if links() {
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
        assert_eq!(
            run.status.code(),
            Some((expected & 0xff) as i32),
            "{name}: executable stderr={}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    expected
}

/// `main` that runs `body` under a `Fail` handler (element indexing raises
/// `IndexOutOfBounds`).
fn handled(items: &str) -> String {
    format!(
        "{items}
fn main() -> u64 {{ handle Fail[RuntimeError] in {{ body() }} with {{ return x => x, raise(e, k) => 999, }} }}
"
    )
}

const STATE: &str = "struct State { slots: ChunkSeq[u32], n: u64 }
fn fresh(n: u64) -> State { State { slots: ChunkSeq::new[u32](), n: n } }
";

/// The census repro (gap C5): a struct that owns a sequence, read out of a
/// sequence with `try_get`. It was `E1522`; the `match` now borrows the
/// element in place.
#[test]
fn census_repro_reads_a_state_in_place() {
    let source = format!(
        "{STATE}fn first_len(in states: ChunkSeq[State]) -> u64 {{
    match states.try_get(0) {{ Some(s) => s.slots.len(), None => 0 }}
}}
fn main() -> u64 {{
    let mut states = ChunkSeq::new[State]();
    let empty = first_len(in states);
    let mut s = fresh(0);
    s.slots.push(4);
    s.slots.push(5);
    states.push(s);
    wrapping_add(wrapping_mul(empty, 100), first_len(in states))
}}
"
    );
    assert_eq!(differential("census_repro", &source), 2);
}

/// `f(in s[i])`, `f(inout s[i])` and method receivers `s[i].m()` lend the
/// element in place: writes through the view reach the element.
#[test]
fn call_arguments_and_receivers_lend_the_element() {
    let source = handled(&format!(
        "{STATE}impl State {{
    fn bump(inout self, v: u32) {{ self.slots.push(v); self.n = wrapping_add(self.n, 1); }}
    fn weight(in self) -> u64 {{ wrapping_add(self.slots.len(), wrapping_mul(self.n, 10)) }}
}}
fn add(inout s: State, v: u32) {{ s.slots.push(v); s.n = wrapping_add(s.n, 100); }}
fn weigh(in s: State) -> u64 {{ s.weight() }}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut states = ChunkSeq::new[State]();
    states.push(fresh(0));
    states.push(fresh(0));
    add(inout states[1], 7);
    add(inout states[1], 8);
    add(inout states[0], 9);
    states[1].bump(3);
    let w = wrapping_add(weigh(in states[0]), wrapping_mul(states[1].weight(), 10000));
    w
}}"
    ));
    // states[0]: 1 slot, n 100 -> 1001; states[1]: 3 slots, n 201 -> 2013.
    assert_eq!(differential("arguments", &source), 1001 + 2013 * 10000);
}

/// Total views: `match s.try_get(i)` binds an `in` view of a non-copy
/// element, `match s.try_get_mut(i)` an `inout` view of any element, and an
/// index past the end takes the `None` arm.
#[test]
fn match_views_are_total() {
    let source = format!(
        "{STATE}fn add(inout s: State, v: u32) {{ s.slots.push(v); s.n = wrapping_add(s.n, 1); }}
fn total(in states: ChunkSeq[State]) -> u64 {{
    let mut i: u64 = 0;
    let mut t: u64 = 0;
    loop {{
        match states.try_get(i) {{
            Some(s) => {{ t = wrapping_add(t, wrapping_add(s.slots.len(), wrapping_mul(s.n, 1000))); }},
            None => {{ break; }},
        }}
        i = wrapping_add(i, 1);
    }}
    t
}}
fn touch(inout xs: Slice[State]) -> bool {{
    match xs.try_get_mut(0) {{ Some(s) => {{ add(inout s, 1); true }}, None => false }}
}}
fn main() -> u64 {{
    let mut states = ChunkSeq::new[State]();
    states.push(fresh(0));
    states.push(fresh(0));
    match states.try_get_mut(1) {{
        Some(s) => {{ add(inout s, 4); add(inout s, 5); s.n = wrapping_add(s.n, 10); s.slots.push(6); }},
        None => {{}},
    }}
    let missed = match states.try_get_mut(7) {{ Some(s) => {{ s.n = 99; 1 }}, None => 0 }};
    let touched = touch(inout states[0..1]);
    let mut nested = ChunkSeq::new[ChunkSeq[u64]]();
    nested.push(ChunkSeq::new[u64]());
    match nested.try_get_mut(0) {{ Some(q) => {{ q.push(3); q.push(4); }}, None => {{}} }}
    let q = match nested.try_get(0) {{ Some(q) => q.len(), _ => 0 }};
    let mut words = ChunkSeq::new[u64]();
    words.push(5);
    match words.try_get_mut(0) {{ Some(w) => {{ w = wrapping_mul(w, 3); }}, None => {{}} }}
    let w = match words.try_get(0) {{ Some(w) => w, None => 0 }};
    let r = wrapping_add(total(in states), wrapping_mul(q, 1000000));
    let flags = if touched {{ 10000000 }} else {{ 0 }};
    wrapping_add(wrapping_add(r, flags), wrapping_add(wrapping_mul(missed, 7), wrapping_mul(w, 100000000)))
}}
"
    );
    // states[0]: 1 slot, n 1 -> 1001; states[1]: 3 slots, n 12 -> 12003.
    assert_eq!(
        differential("match_views", &source),
        1001 + 12003 + 2_000_000 + 10_000_000 + 1_500_000_000
    );
}

/// `swap` and `replace` exchange elements in place, also through generic
/// code instantiated with `u8` and `u32` elements, which native code stores
/// in fewer than 8 bytes.
#[test]
fn swap_and_replace_exchange_elements() {
    let source = handled(&format!(
        "{STATE}fn put[T: mod affine](inout xs: ChunkSeq[T], i: u64, sink v: T) -> T eff [Fail[RuntimeError]] {{
    replace(inout xs[i], v)
}}
fn lend[T: mod affine](inout xs: ChunkSeq[T], inout y: T) eff [Fail[RuntimeError]] {{
    swap(inout xs[0], inout y)
}}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut a = ChunkSeq::new[u32]();
    a.push(1); a.push(2); a.push(3);
    let old = put(inout a, 1, 7);
    let mut b = ChunkSeq::new[u8]();
    b.push(1); b.push(2); b.push(3);
    let old8 = put(inout b, 0, 9);
    let mut y: u32 = 40;
    lend(inout a, inout y);
    let mut states = ChunkSeq::new[State]();
    states.push(fresh(5));
    let mut other = fresh(6);
    other.slots.push(1);
    swap(inout states[0], inout other);
    let was = replace(inout states[0], fresh(8));
    let mut spare = fresh(2);
    lend(inout states, inout spare);
    let r = wrapping_add(wrapping_add(was.n, wrapping_mul(other.n, 10)), wrapping_mul(spare.n, 100));
    let digits = wrapping_add(wrapping_add(widen_u64(a[0]), wrapping_mul(widen_u64(a[1]), 100)), wrapping_mul(widen_u64(a[2]), 10000));
    let more = wrapping_add(wrapping_mul(widen_u64(old), 1000000), wrapping_mul(widen_u64(y), 10000000));
    let bytes = wrapping_add(wrapping_mul(widen_u64(b[0]), 1000000000), wrapping_add(wrapping_mul(widen_u64(b[1]), 10000000000), wrapping_mul(widen_u64(old8), 100000000000)));
    wrapping_add(wrapping_add(digits, more), wrapping_add(bytes, wrapping_mul(r, 1000000000000)))
}}"
    ));
    // a = [40, 7, 3], old 2, y 1; b = [9, 2, 3], old8 1; was 6, other 5,
    // spare 8.
    let digits = 40 + 7 * 100 + 3 * 10000;
    let more = 2 * 1_000_000 + 10_000_000;
    let bytes = 9 * 1_000_000_000 + 2 * 10_000_000_000 + 100_000_000_000;
    let r: u64 = 6 + 5 * 10 + 8 * 100;
    assert_eq!(
        differential("swap_replace", &source),
        digits + more + bytes + r * 1_000_000_000_000
    );
}

/// `inout` match views of elements that native code stores in fewer than
/// 8 bytes (`u32`, `bool`, `u8`), directly and through generic code, read
/// and write the element at its width.
#[test]
fn narrow_elements_are_viewed_at_their_width() {
    let source = handled(
        "fn bump[T: mod affine](inout xs: ChunkSeq[T], sink v: T) -> bool {
    match xs.try_get_mut(1) { Some(x) => { let old = replace(inout x, v); true }, None => false }
}
fn inc(inout x: u32) { x = wrapping_add(x, 100); }
fn body() -> u64 eff [Fail[RuntimeError]] {
    let mut a = ChunkSeq::new[u32]();
    a.push(1); a.push(2); a.push(3);
    let ok = bump(inout a, 7);
    match a.try_get_mut(2) { Some(x) => { x = wrapping_add(x, 10); inc(inout x); x = wrapping_add(x, 1000); }, None => {} }
    let mut b = ChunkSeq::new[bool]();
    b.push(false); b.push(false); b.push(true);
    let okb = bump(inout b, true);
    match b.try_get_mut(0) { Some(x) => { x = true; }, None => {} }
    let mut c = ChunkSeq::new[u8]();
    c.push(1); c.push(2); c.push(3);
    match c.try_get_mut(1) { Some(x) => { x = 200; }, None => {} }
    let b0: u64 = if b[0] { 1 } else { 0 };
    let b1: u64 = if b[1] { 10 } else { 0 };
    let b2: u64 = if b[2] { 100 } else { 0 };
    let bs = wrapping_add(b0, wrapping_add(b1, b2));
    let r = wrapping_add(wrapping_add(widen_u64(a[0]), wrapping_mul(widen_u64(a[1]), 10)), wrapping_add(wrapping_mul(widen_u64(a[2]), 100), wrapping_add(wrapping_mul(bs, 10000000), wrapping_add(wrapping_mul(widen_u64(c[0]), 1000000000), wrapping_add(wrapping_mul(widen_u64(c[1]), 10000000000), wrapping_mul(widen_u64(c[2]), 10000000000000))))));
    if ok && okb { r } else { 0 }
}",
    );
    // a = [1, 7, 1113], b = [true, true, true], c = [1, 200, 3].
    assert_eq!(
        differential("narrow", &source),
        1 + 70
            + 111_300
            + 111 * 10_000_000
            + 1_000_000_000
            + 2_000_000_000_000
            + 30_000_000_000_000
    );
}

/// Elements that own sequences are never copied by a view: replacing,
/// swapping, popping and dropping them frees each sequence exactly once
/// (`differential` checks the JIT's frees), in loops too.
#[test]
fn owning_elements_do_not_leak_or_double_free() {
    let source = handled(&format!(
        "{STATE}fn grow(inout s: State, k: u64) {{
    let mut i: u64 = 0;
    loop {{ if i >= k {{ break; }} s.slots.push(wrapping_u32(i)); i = wrapping_add(i, 1); }}
    s.n = wrapping_add(s.n, k);
}}
fn reset(inout s: State) {{
    let old = replace(inout s.slots, ChunkSeq::new[u32]());
    s.n = wrapping_add(s.n, old.len());
}}
fn body() -> u64 eff [Fail[RuntimeError]] {{
    let mut states = ChunkSeq::new[State]();
    let mut i: u64 = 0;
    loop {{
        if i >= 6 {{ break; }}
        states.push(fresh(i));
        grow(inout states[i], wrapping_add(i, 1));
        i = wrapping_add(i, 1);
    }}
    reset(inout states[2]);
    let mut j: u64 = 0;
    loop {{
        match states.try_get_mut(j) {{
            Some(s) => {{ grow(inout s, 2); s.slots = ChunkSeq::new[u32](); s.slots.push(1); }},
            None => {{ break; }},
        }}
        j = wrapping_add(j, 1);
    }}
    let dropped = replace(inout states[0], fresh(100));
    let popped = match states.pop() {{ Some(s) => s.n, None => 0 }};
    let mut total: u64 = 0;
    let mut k: u64 = 0;
    loop {{
        match states.try_get(k) {{
            Some(s) => {{ total = wrapping_add(total, wrapping_add(s.n, s.slots.len())); }},
            None => {{ break; }},
        }}
        k = wrapping_add(k, 1);
    }}
    wrapping_add(wrapping_add(total, wrapping_mul(popped, 1000)), wrapping_mul(dropped.n, 100000))
}}"
    ));
    // n after grow: i + (i + 1); states[2] reset adds its 3 slots; the loop
    // adds 2. Slots are then [1]. states[0] is replaced by an empty n 100, states[5]
    // (n 5 + 6 + 2 = 13) is popped.
    let n = |i: u64| i + (i + 1) + 2 + if i == 2 { 3 } else { 0 };
    let total = 100 + (1..5).map(|i| n(i) + 1).sum::<u64>();
    assert_eq!(
        differential("owning_elements", &source),
        total + n(5) * 1000 + n(0) * 100_000
    );
}

/// Views of `linear` elements borrow them: the element is not consumed, so
/// the sequence must still be drained.
#[test]
fn linear_elements_are_borrowed() {
    let source = handled(
        "linear struct Ticket { id: u64 }
fn id(in t: Ticket) -> u64 { t.id }
fn stamp(inout t: Ticket) { t.id = wrapping_add(t.id, 10); }
fn body() -> u64 eff [Fail[RuntimeError]] {
    let mut s = ChunkSeq::new[Ticket]();
    s.push(Ticket { id: 1 });
    match s.try_get_mut(0) { Some(t) => stamp(inout t), None => {} }
    let seen = match s.try_get(0) { Some(t) => id(in t), None => 0 };
    let back = match s.pop() { Some(Ticket { id }) => id, None => 0 };
    ChunkSeq::finish(sink s);
    wrapping_add(wrapping_mul(seen, 100), back)
}",
    );
    assert_eq!(differential("linear", &source), 1100 + 11);
}

#[test]
fn views_keep_alias_xor_mutability() {
    for (body, code, needle) in [
        // Copying a non-copy element out stays refused.
        (
            "fn f(inout xs: ChunkSeq[State]) -> u64 eff [Fail[RuntimeError]] { let s = xs[0]; s.n }",
            "E1522",
            "borrow it in place",
        ),
        (
            "fn f(inout xs: ChunkSeq[State]) -> u64 { let o = xs.try_get(0); 0 }",
            "E1522",
            "not copy",
        ),
        // An `in` view's sequence cannot change while the match runs.
        (
            "fn f(inout xs: ChunkSeq[State]) -> u64 { match xs.try_get(0) { Some(s) => { xs.clear(); s.n }, None => 0 } }",
            "E0301",
            "enclosing match",
        ),
        // An `inout` view lends its sequence exclusively.
        (
            "fn f(inout xs: ChunkSeq[State]) -> u64 { match xs.try_get_mut(0) { Some(s) => { s.n = xs.len(); 0 }, None => 0 } }",
            "E0301",
            "conflicts",
        ),
        // A view is a borrow: it cannot be moved out or written through `in`.
        (
            "fn take(sink s: State) -> u64 { s.n }
fn f(inout xs: ChunkSeq[State]) -> u64 { match xs.try_get(0) { Some(s) => take(s), None => 0 } }",
            "E0305",
            "",
        ),
        (
            "fn wr(inout s: State) { s.n = 1; }
fn f(inout xs: ChunkSeq[State]) -> u64 { match xs.try_get(0) { Some(s) => { wr(inout s); 0 }, None => 0 } }",
            "E0307",
            "",
        ),
        (
            "fn take(sink s: State) -> u64 { s.n }
fn f(inout xs: ChunkSeq[State]) -> u64 { match xs.try_get_mut(0) { Some(s) => take(s), None => 0 } }",
            "E0305",
            "",
        ),
        // `try_get_mut` is only a view.
        (
            "fn f(inout xs: ChunkSeq[State]) -> u64 { let o = xs.try_get_mut(0); 0 }",
            "E1522",
            "scrutinee of a match",
        ),
        (
            "fn f(in xs: ChunkSeq[State]) -> u64 { match xs.try_get_mut(0) { Some(s) => 0, None => 0 } }",
            "E0307",
            "",
        ),
        // Two views of one sequence in one call overlap.
        (
            "fn g(inout a: State, in b: State) {}
fn f(inout xs: ChunkSeq[State]) eff [Fail[RuntimeError]] { g(inout xs[0], in xs[1]) }",
            "E0301",
            "overlapping",
        ),
        (
            "fn f(inout xs: ChunkSeq[State]) eff [Fail[RuntimeError]] { swap(inout xs[0], inout xs[1]) }",
            "E0301",
            "overlapping",
        ),
        // A view of an `in` sequence cannot be lent `inout`.
        (
            "fn wr(inout s: State) { s.n = 1; }
fn f(in xs: ChunkSeq[State]) eff [Fail[RuntimeError]] { wr(inout xs[0]) }",
            "E0307",
            "",
        ),
        // A call may not change the sequence a view argument borrows.
        (
            "fn rd(in s: State, n: u64) -> u64 { s.n }
fn grow(inout xs: ChunkSeq[State]) -> u64 { xs.clear(); 1 }
fn f(inout xs: ChunkSeq[State]) -> u64 eff [Fail[RuntimeError]] { rd(in xs[0], grow(inout xs)) }",
            "E0301",
            "",
        ),
        // The arms bind the element, nothing deeper.
        (
            "fn f(inout xs: ChunkSeq[State]) -> u64 { match xs.try_get(0) { Some(State { slots, n }) => n, _ => 0 } }",
            "E1522",
            "arms of an element view match",
        ),
    ] {
        let c = checked(&format!("{STATE}{body}\nfn main() -> u64 {{ 0 }}"));
        assert!(
            c.diagnostics
                .iter()
                .any(|d| d.code == code && d.message.contains(needle)),
            "{body}: expected {code} ({needle}): {:#?}",
            c.diagnostics
        );
    }
}

/// Review findings on #1221: an element argument whose read the bounds
/// hoister turned into `Hoisted(Get)` (optimized JIT), `Box` elements bound
/// through a view (freed while the sequence still owned them), and a narrow
/// `inout` view written by a callee that then raises (the write must survive
/// the handler).
#[test]
fn hoisted_boxed_and_raising_views() {
    const HOISTED: &str = r#"struct State { slots: ChunkSeq[u32], n: u64 }
fn add(inout s: State, v: u32) { s.slots.push(v); s.n = wrapping_add(s.n, 1); }
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut xs = ChunkSeq::new[State]();
    xs.push(State { slots: ChunkSeq::new[u32](), n: 0 });
    xs.push(State { slots: ChunkSeq::new[u32](), n: 0 });
    let mut i: u64 = 0;
    loop {
        if i >= xs.len() { break; }
        add(inout xs[i], 3);
        i = wrapping_add(i, 1);
    }
    add(inout xs[1], 4);
    add(inout xs[1], 5);
    let mut t: u64 = 0;
    i = 0;
    loop {
        if i >= xs.len() { break; }
        t = match xs.try_get(i) { Some(s) => wrapping_add(t, wrapping_add(s.n, s.slots.len())), None => t };
        i = wrapping_add(i, 1);
    }
    t
}
fn main() -> u64 {
    handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 999, }
}
"#;
    const BOXED: &str = r#"enum E { Lit(u64), Neg(Box[E]) }
fn value(in e: E) -> u64 { match e { E::Lit(v) => v, E::Neg(x) => match x { Box(a) => wrapping_add(value(in a), 100) } } }
fn main() -> u64 {
    let mut xs = ChunkSeq::new[E]();
    xs.push(E::Neg(Box::new(E::Neg(Box::new(E::Lit(7))))));
    xs.push(E::Lit(9));
    let a = match xs.try_get(0) { Some(b) => value(in b), None => 0 };
    let c = match xs.try_get(0) { Some(b) => match b { E::Neg(x) => match x { Box(y) => value(in y) }, E::Lit(v) => v }, None => 0 };
    let d = match xs.try_get(0) { Some(b) => value(in b), None => 0 };
    let mut k: u64 = 0;
    let mut i: u64 = 0;
    loop {
        if i >= 50 { break; }
        xs.push(E::Neg(Box::new(E::Lit(i))));
        k = match xs.try_get(0) { Some(b) => match b { E::Neg(x) => match x { Box(y) => wrapping_add(k, value(in y)) }, E::Lit(v) => v }, None => 0 };
        i = wrapping_add(i, 1);
    }
    wrapping_add(wrapping_add(wrapping_add(a, c), d), k)
}
"#;
    const NARROW: &str = r#"fn bump(inout v: u32) eff [Fail[RuntimeError]] { v = wrapping_add(v, 5); let xs = ChunkSeq::new[u32](); let z = xs[3]; }
fn inner(inout xs: ChunkSeq[u32]) -> u64 {
    handle Fail[RuntimeError] in {
        match xs.try_get_mut(0) { Some(v) => { bump(inout v); 1 }, None => 2 }
    } with { return x => x, raise(e, k) => 7, }
}
fn main() -> u64 {
    let mut xs = ChunkSeq::new[u32]();
    xs.push(10);
    let r = inner(inout xs);
    let v = match xs.try_get(0) { Some(v) => widen_u64(v), None => 0 };
    wrapping_add(wrapping_mul(r, 1000), v)
}
"#;
    assert_eq!(differential("hoisted", HOISTED), 8);
    assert_eq!(differential("boxed", BOXED), 5871);
    assert_eq!(differential("narrow raise", NARROW), 7015);
}
