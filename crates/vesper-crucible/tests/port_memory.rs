//! The port-backed package links the Vesper port, and every tape, parse and
//! shrinker the port allocates is given back. (Port package only: the seed
//! copy does not list this test.)
use vesper_crucible::{
    ChoiceTape, IMPLEMENTATION, PropError, ShrinkConfig, Source, port_live_allocations, shrink,
};

#[test]
fn the_port_backed_package_links_the_port() {
    assert_eq!(IMPLEMENTATION, "vesper");
}

#[test]
fn tapes_parses_and_shrinkers_release_every_port_allocation() {
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
    };
    work(3);
    let before = port_live_allocations();
    for seed in 0..4 {
        work(seed);
    }
    assert_eq!(port_live_allocations(), before);
}
