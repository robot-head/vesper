//! The port-backed package links the Vesper port, and every tape, parse,
//! shrinker, fuzzer and schedule search the port allocates is given back.
//! (Port package only: the seed copy does not list this test.)
mod common;

use common::{build, cells_key, random_program_ext};
use vesper_crucible::{
    ChoiceTape, ExploreConfig, FuzzConfig, IMPLEMENTATION, PropError, Search, ShrinkConfig, Source,
    explore_cached, explore_dpor, fuzz, port_live_allocations, shrink,
};

#[test]
fn the_port_backed_package_links_the_port() {
    assert_eq!(IMPLEMENTATION, "vesper");
}

#[test]
fn tapes_parses_shrinkers_and_searches_release_every_port_allocation() {
    let work = |seed: u64| {
        let tape = ChoiceTape::new((0..200).map(|i| i * seed).collect());
        let text = tape.to_text();
        assert_eq!(ChoiceTape::from_text(&text).unwrap(), tape);
        assert!(ChoiceTape::from_text("nope\n1\n").is_err());
        assert!(ChoiceTape::from_text(&format!("{}\nzz\n", vesper_crucible::TAPE_HEADER)).is_err());
        let mut prop = |src: &mut Source| {
            let v = src.draw_vec(64, |s| s.draw_u64_bounded(100))?;
            if v.iter().sum::<u64>() > 150 {
                Err(PropError::fail("sum", "big"))
            } else {
                Ok(())
            }
        };
        let start: Vec<u64> = (0..20)
            .flat_map(|i| [1, 50 + (i * seed) % 50])
            .chain([0])
            .collect();
        shrink(&mut prop, &ChoiceTape::new(start), &ShrinkConfig::default()).unwrap();
        let config = FuzzConfig {
            seed,
            max_execs: 400,
            ..FuzzConfig::default()
        };
        fuzz(&config, |src: &mut Source| {
            let data = src.draw_bytes(16)?;
            let p = src.probe();
            p.edge(data.len() as u32);
            if p.eq_bytes(1, &data[..data.len().min(2)], b"ok") && p.eq(2, data.len() as u64, 9) {
                return Err(PropError::fail("ok", "found"));
            }
            Ok(())
        });
        let program = random_program_ext(seed, 2, 3);
        let observe = |w: &common::Cells| Ok(w.clone());
        for bound in [None, Some(1)] {
            let cfg = ExploreConfig {
                max_executions: 400,
                preemption_bound: bound,
                ..ExploreConfig::default()
            };
            let _ = explore_dpor(&cfg, || build(&program), observe);
            let _ = explore_cached(Search::Dpor, &cfg, || build(&program), observe, cells_key);
        }
    };
    work(3);
    let before = port_live_allocations();
    for seed in 0..4 {
        work(seed);
    }
    assert_eq!(port_live_allocations(), before);
}
