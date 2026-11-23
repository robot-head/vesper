//! Differential tests comparing the Vesper port (`vesper_crucible`) against
//! the frozen Rust seed (`vesper_crucible_rust`) across all 6 ported areas:
//! RNG, choice source, tape codec & shrink keys, shrinker, fuzzer, and DPOR.

use std::collections::BTreeMap;

#[test]
fn port_matches_rust_seed_across_all_areas() {
    let mut diffs = 0usize;
    let mut cases = 0usize;

    // 1. SplitMix64 (next_u64, below_inclusive, chance)
    for seed in 0..64u64 {
        let mut port = vesper_crucible::SplitMix64(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 1);
        let mut rust =
            vesper_crucible_rust::SplitMix64(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 1);
        for bound in [0u64, 1, 7, 255, 1023, 1_000_000, u64::MAX] {
            cases += 1;
            if port.next_u64() != rust.next_u64() {
                diffs += 1;
            }
            cases += 1;
            if port.below_inclusive(bound) != rust.below_inclusive(bound) {
                diffs += 1;
            }
            cases += 1;
            if port.chance(bound % 1025) != rust.chance(bound % 1025) {
                diffs += 1;
            }
        }
    }

    // 2. Source (random, replay, with_prefix, draw_bytes, draw_range_i64, draw_run_now)
    for seed in 0..64u64 {
        let mut p_src = vesper_crucible::Source::random(seed);
        let mut r_src = vesper_crucible_rust::Source::random(seed);
        for i in 0..32u64 {
            cases += 1;
            if p_src.draw_u64_bounded(i * 37).ok() != r_src.draw_u64_bounded(i * 37).ok() {
                diffs += 1;
            }
        }
        cases += 1;
        if p_src.draw_bytes(48).ok() != r_src.draw_bytes(48).ok() {
            diffs += 1;
        }
        cases += 1;
        if p_src.draw_run_now().ok() != r_src.draw_run_now().ok() {
            diffs += 1;
        }
        cases += 1;
        if p_src.draw_range_i64(-100, 100).ok() != r_src.draw_range_i64(-100, 100).ok() {
            diffs += 1;
        }
        cases += 1;
        if p_src.realized_values() != r_src.realized_values() {
            diffs += 1;
        }

        let tape_vals: Vec<u64> = (0..16).map(|k| (seed * 13 + k * 7) % 500).collect();
        let mut p_rep =
            vesper_crucible::Source::replay(&vesper_crucible::ChoiceTape::new(tape_vals.clone()));
        let mut r_rep =
            vesper_crucible_rust::Source::replay(&vesper_crucible_rust::ChoiceTape::new(tape_vals));
        for b in [10u64, 255, 1, 100, 0] {
            cases += 1;
            if p_rep.draw_u64_bounded(b).ok() != r_rep.draw_u64_bounded(b).ok() {
                diffs += 1;
            }
        }
        cases += 1;
        if p_rep.draw_bytes(8).ok() != r_rep.draw_bytes(8).ok() {
            diffs += 1;
        }
        cases += 1;
        if p_rep.realized_values() != r_rep.realized_values() {
            diffs += 1;
        }
    }

    // 3. ChoiceTape & ShrinkKey
    for seed in 0..64u64 {
        let mut rng = vesper_crucible_rust::SplitMix64(seed + 1000);
        let len = (seed as usize * 7) % 64;
        let vals: Vec<u64> = (0..len).map(|_| rng.next_u64() >> (rng.0 & 63)).collect();
        let pt = vesper_crucible::ChoiceTape::new(vals.clone());
        let rt = vesper_crucible_rust::ChoiceTape::new(vals);
        let pk = pt.key();
        let rk = rt.key();
        cases += 1;
        if (pk.len, pk.sum) != (rk.len, rk.sum) {
            diffs += 1;
        }
        let p_txt = pt.to_text();
        let r_txt = rt.to_text();
        cases += 1;
        if p_txt != r_txt {
            diffs += 1;
        }
        let p_back = vesper_crucible::ChoiceTape::from_text(&p_txt);
        let r_back = vesper_crucible_rust::ChoiceTape::from_text(&r_txt);
        cases += 1;
        if p_back.as_ref().map(|t| &t.values).ok() != r_back.as_ref().map(|t| &t.values).ok() {
            diffs += 1;
        }
    }
    for bad in ["", "wrong-header\n0\n", "vesper-crucible-tape/1\nXYZ\n"] {
        cases += 1;
        if vesper_crucible::ChoiceTape::from_text(bad).is_err()
            != vesper_crucible_rust::ChoiceTape::from_text(bad).is_err()
        {
            diffs += 1;
        }
    }

    // 4. Shrinker
    for seed in 0..32u64 {
        let mut rng = vesper_crucible_rust::SplitMix64(seed + 5000);
        let mut tape = Vec::new();
        for _ in 0..24 {
            tape.push(1);
            tape.push(30 + rng.below_inclusive(70));
        }
        tape.push(0);

        let p_res = vesper_crucible::shrink(
            &mut |src| {
                let v = src.draw_vec(24, |s| s.draw_u64_bounded(100))?;
                if v.iter().sum::<u64>() > 400 {
                    Err(vesper_crucible::PropError::fail("sum", "exceeds 400"))
                } else {
                    Ok(())
                }
            },
            &vesper_crucible::ChoiceTape::new(tape.clone()),
            &vesper_crucible::ShrinkConfig::default(),
        )
        .unwrap();

        let r_res = vesper_crucible_rust::shrink(
            &mut |src| {
                let v = src.draw_vec(24, |s| s.draw_u64_bounded(100))?;
                if v.iter().sum::<u64>() > 400 {
                    Err(vesper_crucible_rust::PropError::fail("sum", "exceeds 400"))
                } else {
                    Ok(())
                }
            },
            &vesper_crucible_rust::ChoiceTape::new(tape),
            &vesper_crucible_rust::ShrinkConfig::default(),
        )
        .unwrap();

        cases += 1;
        if p_res.tape.values != r_res.tape.values
            || p_res.stats.runs != r_res.stats.runs
            || p_res.stats.accepted != r_res.stats.accepted
            || (p_res.stats.shrunk.len, p_res.stats.shrunk.sum)
                != (r_res.stats.shrunk.len, r_res.stats.shrunk.sum)
        {
            diffs += 1;
        }
    }

    // 5. Fuzzer (Guided + Random)
    for seed in 0..20u64 {
        for strategy_idx in [0u8, 1u8] {
            let p_cfg = vesper_crucible::FuzzConfig {
                seed,
                max_execs: 400,
                strategy: if strategy_idx == 0 {
                    vesper_crucible::Strategy::Guided
                } else {
                    vesper_crucible::Strategy::Random
                },
                ..vesper_crucible::FuzzConfig::default()
            };
            let r_cfg = vesper_crucible_rust::FuzzConfig {
                seed,
                max_execs: 400,
                strategy: if strategy_idx == 0 {
                    vesper_crucible_rust::Strategy::Guided
                } else {
                    vesper_crucible_rust::Strategy::Random
                },
                ..vesper_crucible_rust::FuzzConfig::default()
            };
            let p_rep = vesper_crucible::fuzz(&p_cfg, |src| {
                let data = src.draw_bytes(16)?;
                if data.len() >= 2 {
                    let w = u16::from_le_bytes([data[0], data[1]]);
                    if src.probe().eq(1, u64::from(w), 0xbeef) {
                        return Err(vesper_crucible::PropError::fail("magic", "hit"));
                    }
                }
                Ok(())
            });
            let r_rep = vesper_crucible_rust::fuzz(&r_cfg, |src| {
                let data = src.draw_bytes(16)?;
                if data.len() >= 2 {
                    let w = u16::from_le_bytes([data[0], data[1]]);
                    if src.probe().eq(1, u64::from(w), 0xbeef) {
                        return Err(vesper_crucible_rust::PropError::fail("magic", "hit"));
                    }
                }
                Ok(())
            });
            cases += 1;
            if p_rep.execs != r_rep.execs
                || p_rep.invalid_runs != r_rep.invalid_runs
                || p_rep.features != r_rep.features
                || p_rep.solver_runs != r_rep.solver_runs
                || p_rep.solver_hits != r_rep.solver_hits
                || p_rep.counterexample.as_ref().map(|c| &c.tape.values)
                    != r_rep.counterexample.as_ref().map(|c| &c.tape.values)
            {
                diffs += 1;
            }
        }
    }

    // 6. DPOR + Cached DPOR
    #[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
    struct World {
        cells: [u64; 3],
    }

    macro_rules! run_dpor_pair {
        ($bound:expr, $seed:expr) => {{
            struct PortFiber {
                pc: u8,
                slot: usize,
                val: u64,
            }
            impl vesper_crucible::SimFiber<World> for PortFiber {
                fn footprint(&self) -> vesper_crucible::Footprint {
                    vesper_crucible::Footprint::none().write(self.slot as u64)
                }
                fn step(
                    &mut self,
                    w: &mut World,
                    cx: &mut vesper_crucible::StepCx,
                ) -> Result<vesper_crucible::SimStep, vesper_crucible::PropError> {
                    cx.write(self.slot as u64);
                    w.cells[self.slot] = w.cells[self.slot].wrapping_add(self.val);
                    self.pc += 1;
                    if self.pc < 2 {
                        Ok(vesper_crucible::SimStep::Yield)
                    } else {
                        Ok(vesper_crucible::SimStep::Done)
                    }
                }
            }

            struct RustFiber {
                pc: u8,
                slot: usize,
                val: u64,
            }
            impl vesper_crucible_rust::SimFiber<World> for RustFiber {
                fn footprint(&self) -> vesper_crucible_rust::Footprint {
                    vesper_crucible_rust::Footprint::none().write(self.slot as u64)
                }
                fn step(
                    &mut self,
                    w: &mut World,
                    cx: &mut vesper_crucible_rust::StepCx,
                ) -> Result<vesper_crucible_rust::SimStep, vesper_crucible_rust::PropError>
                {
                    cx.write(self.slot as u64);
                    w.cells[self.slot] = w.cells[self.slot].wrapping_add(self.val);
                    self.pc += 1;
                    if self.pc < 2 {
                        Ok(vesper_crucible_rust::SimStep::Yield)
                    } else {
                        Ok(vesper_crucible_rust::SimStep::Done)
                    }
                }
            }

            let s0 = ($seed as usize) % 3;
            let s1 = ($seed as usize + 1) % 3;
            let s2 = ($seed as usize + 1) % 3;
            let p_cfg = vesper_crucible::ExploreConfig {
                preemption_bound: $bound,
                ..vesper_crucible::ExploreConfig::default()
            };
            let r_cfg = vesper_crucible_rust::ExploreConfig {
                preemption_bound: $bound,
                ..vesper_crucible_rust::ExploreConfig::default()
            };
            let p_exp = vesper_crucible::explore_cached(
                vesper_crucible::Search::Dpor,
                &p_cfg,
                || {
                    (
                        World::default(),
                        vec![
                            PortFiber {
                                pc: 0,
                                slot: s0,
                                val: 1,
                            },
                            PortFiber {
                                pc: 0,
                                slot: s1,
                                val: 2,
                            },
                            PortFiber {
                                pc: 0,
                                slot: s2,
                                val: 4,
                            },
                        ],
                    )
                },
                |w| Ok(w.cells),
                |w, fs| (w.clone(), fs.iter().map(|f| f.pc).collect::<Vec<_>>()),
            )
            .unwrap();
            let r_exp = vesper_crucible_rust::explore_cached(
                vesper_crucible_rust::Search::Dpor,
                &r_cfg,
                || {
                    (
                        World::default(),
                        vec![
                            RustFiber {
                                pc: 0,
                                slot: s0,
                                val: 1,
                            },
                            RustFiber {
                                pc: 0,
                                slot: s1,
                                val: 2,
                            },
                            RustFiber {
                                pc: 0,
                                slot: s2,
                                val: 4,
                            },
                        ],
                    )
                },
                |w| Ok(w.cells),
                |w, fs| (w.clone(), fs.iter().map(|f| f.pc).collect::<Vec<_>>()),
            )
            .unwrap();

            let p_outs: BTreeMap<_, _> = p_exp
                .outcomes
                .keys()
                .map(|k| (k.clone().ok(), ()))
                .collect();
            let r_outs: BTreeMap<_, _> = r_exp
                .outcomes
                .keys()
                .map(|k| (k.clone().ok(), ()))
                .collect();

            cases += 1;
            if p_outs != r_outs
                || p_exp.executions != r_exp.executions
                || p_exp.sleep_blocked != r_exp.sleep_blocked
                || p_exp.transitions != r_exp.transitions
                || p_exp.cache_hits != r_exp.cache_hits
                || p_exp.bound_pruned != r_exp.bound_pruned
            {
                diffs += 1;
            }
        }};
    }

    for seed in 0..16u64 {
        run_dpor_pair!(None, seed);
        run_dpor_pair!(Some(1), seed);
    }

    println!("flip-gate differential differences={diffs} cases={cases}");
    assert_eq!(
        diffs, 0,
        "found {diffs} differential differences across {cases} cases"
    );
}
