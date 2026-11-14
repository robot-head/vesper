//! Layout randomization: plans, real stack/heap placement and the survival test.
use std::convert::Infallible;
use vesper_bench::{
    BenchCase, Invocation, PairedSample, RunConfig,
    layout::{
        HeapShim, LayoutVerdict, OffsetBuffer, env_padding_value, layout_plan, layout_survival,
        run_randomized_layouts, with_stack_padding,
    },
};

#[test]
fn plans_are_deterministic_bounded_and_varied() {
    let mut heaps = std::collections::BTreeSet::new();
    let mut stacks = std::collections::BTreeSet::new();
    let mut envs = std::collections::BTreeSet::new();
    for run in 0..64 {
        let plan = layout_plan(7, run);
        assert_eq!(plan, layout_plan(7, run));
        assert!(plan.heap_offset_bytes < 4096 && plan.heap_offset_bytes.is_multiple_of(16));
        assert!(plan.stack_padding_bytes < 4096 && plan.stack_padding_bytes.is_multiple_of(64));
        assert!(plan.env_padding_bytes < 4096);
        heaps.insert(plan.heap_offset_bytes);
        stacks.insert(plan.stack_padding_bytes);
        envs.insert(plan.env_padding_bytes);
    }
    assert!(heaps.len() > 30 && stacks.len() > 20 && envs.len() > 50);
    assert_ne!(layout_plan(7, 0), layout_plan(8, 0));
    assert_eq!(env_padding_value(5), "xxxxx");
    assert_eq!(env_padding_value(10_000).len(), 4095);
}

#[inline(never)]
fn stack_address() -> usize {
    let marker = std::hint::black_box(0u8);
    &marker as *const u8 as usize
}

#[test]
fn stack_padding_moves_the_benchmark_frame_by_at_least_the_padding() {
    let base = with_stack_padding(0, stack_address);
    for bytes in [64u32, 640, 4032] {
        let padded = with_stack_padding(bytes, stack_address);
        // Stacks grow down on every supported target.
        assert!(
            base >= padded + bytes as usize,
            "padding {bytes}: base {base:#x}, padded {padded:#x}"
        );
    }
    assert_eq!(with_stack_padding(128, || 41 + 1), 42);
}

#[test]
fn offset_buffers_place_data_at_the_planned_page_offset() {
    for offset in [0u32, 16, 1008, 4080] {
        let mut buffer = OffsetBuffer::new(100, offset).unwrap();
        assert_eq!(buffer.as_slice().as_ptr() as usize % 4096, offset as usize);
        assert_eq!(buffer.as_slice().len(), 100);
        buffer.as_mut_slice()[99] = 7;
        assert_eq!(buffer.as_slice()[99], 7);
    }
    assert!(OffsetBuffer::new(1, 4096).is_err());
    let shim = HeapShim::new(512).unwrap();
    assert_eq!(shim.len(), 512);
    assert!(HeapShim::new(0).unwrap().is_empty());
}

fn pairs(ratios: &[(u64, u64)]) -> Vec<PairedSample> {
    ratios
        .iter()
        .map(|&(b, c)| PairedSample::new(b, c, 10).unwrap())
        .collect()
}

#[test]
fn survival_requires_the_interval_to_exclude_no_difference() {
    let slower = pairs(&[(100, 120); 8]);
    assert_eq!(
        layout_survival(&slower).unwrap().verdict,
        LayoutVerdict::CandidateSlower
    );
    let faster = pairs(&[
        (100, 80),
        (100, 85),
        (100, 90),
        (100, 82),
        (100, 88),
        (100, 81),
    ]);
    let result = layout_survival(&faster).unwrap();
    assert_eq!(result.verdict, LayoutVerdict::CandidateFaster);
    assert!(result.verdict.survives());
    // A layout artifact: big wins in some layouts, losses in others.
    let mixed = pairs(&[
        (100, 70),
        (100, 130),
        (100, 75),
        (100, 125),
        (100, 72),
        (100, 128),
        (100, 101),
        (100, 99),
    ]);
    assert_eq!(
        layout_survival(&mixed).unwrap().verdict,
        LayoutVerdict::DoesNotSurvive
    );
    assert_eq!(
        layout_survival(&pairs(&[(100, 120); 5])).unwrap().verdict,
        LayoutVerdict::InsufficientLayouts
    );
    assert!(layout_survival(&[]).is_err());
    assert!(layout_survival(&pairs(&[(1, 1); 65])).is_err());
}

struct Spin(u64);
impl BenchCase for Spin {
    type Input = u64;
    type Output = u64;
    type Error = Infallible;
    fn setup(&mut self, _: Invocation) -> Result<u64, Infallible> {
        Ok(1)
    }
    fn run(&mut self, mut n: u64) -> Result<u64, Infallible> {
        for _ in 0..self.0 {
            n = std::hint::black_box(n.wrapping_mul(31).wrapping_add(7));
        }
        Ok(n)
    }
}

#[test]
fn randomized_runs_detect_a_real_algorithmic_difference() {
    let config = RunConfig {
        epochs: 6,
        warmup_epochs: 1,
        operations_per_window: 8,
        ..RunConfig::default()
    };
    // The candidate does eight times the work: no layout can hide that.
    let report = run_randomized_layouts(config, 8, 3, &mut Spin(2_000), &mut Spin(16_000)).unwrap();
    assert_eq!(report.runs.len(), 8);
    for (i, run) in report.runs.iter().enumerate() {
        assert_eq!(run.plan, layout_plan(3, i as u32));
        assert_eq!(run.report.epochs().len(), 6);
    }
    assert_eq!(report.survival.verdict, LayoutVerdict::CandidateSlower);
    assert!(run_randomized_layouts(config, 0, 3, &mut Spin(1), &mut Spin(1)).is_err());
    assert!(run_randomized_layouts(config, 65, 3, &mut Spin(1), &mut Spin(1)).is_err());
}
