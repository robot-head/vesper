//! Bounded generics by monomorphization (port wave P0, lane 5; F7a): trait
//! bounds on functions, methods, types and conformances, the builtin `Eq`,
//! `Ord` and `Hash` traits with derived conformances, evaluator/JIT
//! agreement, AArch64 objects and Lean extraction of the instances.
use std::path::Path;
use vesper::{check, lean, native, parser, runtime::Runtime};

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "check: {:?}", c.diagnostics);
    c.world
}

/// Evaluator and JIT agree; an AArch64 object and Lean are emitted.
fn execute(source: &str) -> String {
    let w = world(source);
    let interpreted = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
    let jit = native::jit(&w, "main").unwrap().to_string();
    assert_eq!(jit, interpreted, "JIT and evaluator disagree");
    let (arm, _) =
        native::object_for_target(&w, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(&arm[..4], b"\x7fELF");
    let lean = lean::extract(&w).expect("Lean accepts the monomorphized core");
    assert!(!lean.contains("bound$"), "no stub reaches Lean");
    interpreted
}

fn rejects(source: &str, code: &str) -> Vec<vesper::ast::Diagnostic> {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code}; got {:?}",
        c.diagnostics
    );
    c.diagnostics
}

#[test]
fn generic_max_runs_at_scalars_and_structs() {
    let src = include_str!("fixtures/bounded_generics/max.ves");
    assert_eq!(execute(src), "721");
}

#[test]
fn sorted_insert_over_a_struct_key() {
    let src = include_str!("fixtures/bounded_generics/sorted.ves");
    // (1,2) (1,5) (2,0) (2,1)
    assert_eq!(execute(src), "12152021");
}

#[test]
fn bounded_container_impl_and_models() {
    let src = include_str!("fixtures/bounded_generics/container.ves");
    // lo 3, hi 5, inside, smallest 2, reversed 8, tuple Eq/Hash ok.
    assert_eq!(execute(src), "835121");
}

/// A bounded inherent impl block with joined bounds (`K: mod copy + Ord`).
/// Inline rather than a `.ves` fixture: the self-hosted front end's corpus
/// covers every repository fixture and does not parse `+` bounds yet.
const INTERVAL: &str = "copy struct Interval[K: Ord] { lo: K, hi: K }
impl[K: mod copy + Ord] Interval[K] {
    fn widen(in self, x: K) -> Interval[K] {
        let lo = match x.cmp(self.lo) { Less => x, _ => self.lo };
        let hi = match x.cmp(self.hi) { Greater => x, _ => self.hi };
        Interval { lo: lo, hi: hi }
    }
    fn contains(in self, x: K) -> bool {
        match x.cmp(self.lo) { Less => false, _ => match x.cmp(self.hi) { Greater => false, _ => true } }
    }
}
fn main() -> i64 {
    let i = Interval { lo: 3, hi: 5 };
    let i = i.widen(9);
    let i = i.widen(1);
    let inside = if i.contains(7) { 1 } else { 0 };
    let outside = if i.contains(10) { 1 } else { 0 };
    wrapping_add(wrapping_add(wrapping_mul(i.lo, 1000), wrapping_mul(i.hi, 100)), wrapping_add(wrapping_mul(inside, 10), outside))
}";

#[test]
fn bounded_inherent_impl_blocks_instantiate_their_methods() {
    assert_eq!(execute(INTERVAL), "1910");
    let w = world(INTERVAL);
    assert!(
        w.functions.keys().any(|n| n.starts_with("Interval::widen$mono$")),
        "{:?}",
        w.functions.keys()
    );
    assert!(!w.functions.contains_key("Interval::widen"));
    // The impl's bound is checked where the type is used.
    let head = &INTERVAL[..INTERVAL.find("fn main").unwrap()];
    rejects(
        &format!("{head} fn main() -> i64 {{ let i = Interval {{ lo: 3.0, hi: 5.0 }}; let j = i.widen(1.0); 0 }}"),
        "E0608",
    );
}

#[test]
fn bounds_and_models_flow_into_nested_instances() {
    let src = include_str!("fixtures/bounded_generics/nested.ves");
    assert_eq!(execute(src), "111");
    let w = world(src);
    // Only instances reach the World: no generic original, no stub.
    for name in w.functions.keys() {
        assert!(!name.starts_with("bound$"), "{name}");
    }
    assert!(!w.functions.contains_key("less") && !w.functions.contains_key("sorted"));
    let instances: Vec<&String> = w
        .functions
        .keys()
        .filter(|n| n.contains("$mono$"))
        .collect();
    assert!(
        instances.iter().any(|n| n.ends_with("$Reverse")),
        "the model selected at the outer call names the instance: {instances:?}"
    );
    // Instances are private generated functions.
    for n in instances {
        assert!(!w.elaboration.generated[n].public);
    }
    // Builtin conformances never called are pruned.
    assert!(
        !w.functions.keys().any(|n| n.contains("Hash")),
        "{:?}",
        w.functions.keys()
    );
}

/// The reference fold of the builtin `Hash` (FNV-1a constants, addition).
fn mix(h: u64, v: u64) -> u64 {
    h.wrapping_add(v).wrapping_mul(1_099_511_628_211)
}
const OFFSET: u64 = 14_695_981_039_346_656_037;

#[test]
fn builtin_hash_matches_its_reference() {
    let src = include_str!("fixtures/bounded_generics/hash.ves");
    let scalar = |v: u64| mix(OFFSET, v);
    let point = mix(mix(OFFSET, scalar(u64::MAX)), scalar(7));
    let tuple = mix(mix(OFFSET, scalar(1)), scalar(3));
    let expected = scalar(u64::MAX)
        .wrapping_add(point)
        .wrapping_add(tuple)
        .wrapping_add(scalar(42));
    assert_eq!(execute(src), expected.to_string());
}

#[test]
fn reject_fixtures_report_their_diagnostics() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bounded_generics/reject");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ves"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 10);
    for path in paths {
        let src = std::fs::read_to_string(&path).unwrap();
        let code = src
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("// expect "))
            .and_then(|l| l.split(':').next())
            .unwrap_or_else(|| panic!("{}: missing `// expect CODE:` header", path.display()));
        let ds = rejects(&src, code);
        for d in &ds {
            assert!(
                d.span.end <= src.len(),
                "{}: span {:?} outside the source",
                path.display(),
                d.span
            );
        }
    }
}

#[test]
fn parser_records_bound_owners_and_joined_bounds() {
    let m = parser::parse(
        "copy struct S[K: Ord] { k: K } impl[K: mod copy + Ord + Hash] S[K] { fn a(in self) -> i64 { 0 } fn b(in self) -> i64 { 0 } } fn f[T](in x: T) -> i64 where T: Eq { 0 }",
    );
    assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
    let owners: Vec<(String, String, String)> = m
        .module
        .trait_bounds
        .iter()
        .map(|b| (b.owner.clone(), b.param.clone(), b.trait_ref.name.clone()))
        .collect();
    let has = |o: &str, p: &str, t: &str| owners.contains(&(o.into(), p.into(), t.into()));
    assert!(has("S", "K", "Ord"), "{owners:?}");
    for m in ["S::a", "S::b"] {
        assert!(has(m, "K", "Ord") && has(m, "K", "Hash"), "{owners:?}");
    }
    assert!(has("f", "T", "Eq"), "{owners:?}");
    let Some(vesper::ast::Item::Impl { generics, .. }) = m.module.items.get(1) else {
        panic!("impl")
    };
    assert_eq!(generics[0].bound, vesper::ast::Mode::Copy);
}
