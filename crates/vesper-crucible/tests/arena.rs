//! Acceptance: the 64 KiB reset-per-execution `VirtualArena`.

use vesper_crucible::{
    ARENA_LINEAR_LEAK_KIND, ArbitraryIn, ArenaSetupError, Invalid, PropError, Region, RegionBytes,
    RegionVec, VIRTUAL_ARENA_BYTES, Verdict, VirtualArena, draw_region_bytes, draw_region_vec,
};

#[test]
fn every_execution_starts_from_an_empty_arena_and_old_handles_go_stale() {
    let mut arena = VirtualArena::new().unwrap();
    assert_eq!(arena.bytes(), VIRTUAL_ARENA_BYTES);
    let mut kept: Option<RegionVec<u64>> = None;
    for seed in 0..50u64 {
        let epoch = arena.epoch();
        let verdict = arena.run_random(&[], seed, 1 << 16, |src, region| {
            assert_eq!(region.stats().used_bytes, 0, "arena was reset");
            if let Some(old) = &kept {
                // A handle minted by an earlier execution is stale now.
                assert!(region.vec_as_slice(old).is_err());
            }
            let v = RegionVec::<u64>::arbitrary_in(src, region)?;
            let b = RegionBytes::arbitrary_in(src, region)?;
            assert!(region.vec_len(&v).unwrap() <= 1024);
            assert!(region.bytes(&b).unwrap().len() <= 1024);
            kept = Some(v);
            Ok(())
        });
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(arena.used_bytes(), 0);
        assert_eq!(arena.epoch(), epoch + 1);
    }
    let stats = arena.stats();
    assert_eq!(stats.executions, 50);
    assert!(stats.high_water_bytes > 0 && stats.high_water_bytes <= VIRTUAL_ARENA_BYTES);
}

#[test]
fn replaying_the_realized_tape_regenerates_the_same_values() {
    let mut arena = VirtualArena::new().unwrap();
    let mut first: Vec<u8> = Vec::new();
    let generate = |src: &mut vesper_crucible::Source, region: &mut Region, out: &mut Vec<u8>| {
        let b = draw_region_bytes(src, region, 300)?;
        let v = draw_region_vec::<u16>(src, region, 40)?;
        out.clear();
        out.extend_from_slice(region.bytes(&b).unwrap());
        for x in region.vec_as_slice(&v).unwrap() {
            out.extend_from_slice(&x.to_le_bytes());
        }
        Ok::<(), PropError>(())
    };
    assert_eq!(
        arena.run_random(&[], 7, 1 << 16, |s, r| generate(s, r, &mut first)),
        Verdict::Pass
    );
    let tape = arena.realized().to_vec();
    assert!(!tape.is_empty());
    let mut second = Vec::new();
    assert_eq!(
        arena.run_replay(&tape, |s, r| generate(s, r, &mut second)),
        Verdict::Pass
    );
    assert_eq!(first, second);
    assert_eq!(arena.realized(), tape.as_slice());
    // The same tape decodes identically to the heap generator's encoding.
    let mut src = vesper_crucible::Source::replay(&vesper_crucible::ChoiceTape::new(tape));
    let heap = src.draw_bytes(300).unwrap();
    assert_eq!(heap, first[..heap.len()]);
}

#[test]
fn inputs_larger_than_the_arena_are_invalid_and_the_next_run_recovers() {
    let mut arena = VirtualArena::with_bytes(4096).unwrap();
    // A tape forcing 5000 bytes: a continuation flag (1) and a byte per
    // element, then a stop flag.
    let mut tape = Vec::new();
    for i in 0..5000u64 {
        tape.push(1);
        tape.push(i % 251);
    }
    tape.push(0);
    let verdict = arena.run_replay(&tape, |src, region| {
        draw_region_bytes(src, region, 10_000)?;
        Ok(())
    });
    assert_eq!(verdict, Verdict::Invalid(Invalid::ArenaExhausted));
    assert_eq!(arena.stats().exhausted, 1);
    assert_eq!(arena.used_bytes(), 0);
    // A small input fits again.
    let verdict = arena.run_replay(&[1, 9, 1, 8, 0], |src, region| {
        let b = draw_region_bytes(src, region, 10)?;
        assert_eq!(region.bytes(&b).unwrap(), &[9, 8]);
        Ok(())
    });
    assert_eq!(verdict, Verdict::Pass);
    assert_eq!(
        VirtualArena::with_bytes(0).unwrap_err(),
        ArenaSetupError::ZeroSize
    );
}

/// A linear payload (an obligation that must be consumed).
struct Obligation;

#[test]
fn a_leaked_linear_payload_fails_the_execution_and_is_counted() {
    let mut arena = VirtualArena::new().unwrap();
    let verdict = arena.run_replay(&[], |_, region| {
        let _leaked = region.alloc_linear(Obligation).ok();
        Ok(())
    });
    match verdict {
        Verdict::Fail(f) => assert_eq!(f.kind, ARENA_LINEAR_LEAK_KIND),
        other => panic!("expected a leak failure, got {other:?}"),
    }
    assert_eq!(arena.stats().linear_leaks, 1);
    // The arena was rebuilt; the next execution is clean and passes.
    let verdict = arena.run_replay(&[], |_, region| {
        assert_eq!(region.stats().live_linear, 0);
        let b = region.alloc_linear(Obligation).unwrap();
        region.take(b).unwrap();
        Ok(())
    });
    assert_eq!(verdict, Verdict::Pass);
    // Strict replay past the tape is still `Overrun`, not success.
    let verdict = arena.run_replay(&[1], |src, region| {
        draw_region_bytes(src, region, 10)?;
        Ok(())
    });
    assert_eq!(verdict, Verdict::Invalid(Invalid::Overrun));
}

#[test]
fn an_execution_that_unwinds_does_not_leak_into_the_next() {
    // Review regression: a panicking property skipped the reset.
    let mut arena = VirtualArena::with_bytes(64).unwrap();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        arena.run_replay(&[], |_, region| {
            region.alloc_bytes(64, 1).unwrap();
            panic!("property panicked");
        })
    }));
    assert!(caught.is_err());
    let verdict = arena.run_replay(&[1, 7, 0], |src, region| {
        assert_eq!(region.stats().used_bytes, 0);
        let b = draw_region_bytes(src, region, 4)?;
        assert_eq!(region.bytes(&b).unwrap(), &[7]);
        Ok(())
    });
    assert_eq!(verdict, Verdict::Pass);
}
