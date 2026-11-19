//! The ported entry points are the Vesper library (port wave P3b lane 6,
//! #1068), which frees what it allocates.
use vesper_chorus::check::{ExploreLimits, explore};
use vesper_chorus::{IMPLEMENTATION, parse_choreography, port_live_allocations, project_all};

#[test]
fn the_vesper_library_is_the_build() {
    assert_eq!(IMPLEMENTATION, "vesper");
}

#[test]
fn ported_calls_free_their_allocations() {
    let src = "roles A, B, C; rec X { A -> B { Go(u64) { B -> C: Fwd(u64); continue X; } Stop() { B -> C: Done(); } } }";
    let before = port_live_allocations();
    for _ in 0..3 {
        let c = parse_choreography(src).unwrap();
        project_all(&c).unwrap();
        explore(&c, ExploreLimits::default()).unwrap();
        assert!(parse_choreography("roles A; A -> A: X();").is_err());
    }
    assert_eq!(port_live_allocations(), before);
}

#[test]
fn wide_descending_choice_sorts_in_n_log_n() {
    // 20,000 arms whose labels arrive in descending order: a quadratic
    // sort would take about 200 million swaps per endpoint.
    let n = 20_000;
    let mut src = String::from("roles A, B; A -> B {\n");
    for i in (0..n).rev() {
        src.push_str(&format!("L{i:05}() {{ }}\n"));
    }
    src.push('}');
    let c = parse_choreography(&src).unwrap();
    let t = std::time::Instant::now();
    let all = project_all(&c).unwrap();
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    for l in &all {
        let (vesper_chorus::LocalType::Select { branches, .. }
        | vesper_chorus::LocalType::Offer { branches, .. }) = l
        else {
            panic!("{l:?}");
        };
        assert_eq!(branches.len(), n);
        assert!(branches.windows(2).all(|w| w[0].label < w[1].label));
        assert_eq!(branches[0].label, "L00000");
    }
}
