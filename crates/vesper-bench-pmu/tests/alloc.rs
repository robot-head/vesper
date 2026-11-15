//! Zero-allocation evidence through the counting global allocator.
use vesper_bench::{BenchCase, Invocation, RunConfig, run_abba_observed};
use vesper_bench_pmu::{
    AllocationCounts, AllocationObserver, CountingAllocator, counting_allocator_installed,
};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct Arith;
impl BenchCase for Arith {
    type Input = u64;
    type Output = u64;
    type Error = String;
    fn setup(&mut self, i: Invocation) -> Result<u64, String> {
        Ok(i.epoch as u64)
    }
    fn run(&mut self, x: u64) -> Result<u64, String> {
        Ok(std::hint::black_box(x).wrapping_mul(31).rotate_left(7))
    }
}
struct Allocating;
impl BenchCase for Allocating {
    type Input = ();
    type Output = usize;
    type Error = String;
    fn setup(&mut self, _: Invocation) -> Result<(), String> {
        Ok(())
    }
    fn run(&mut self, _: ()) -> Result<usize, String> {
        Ok(std::hint::black_box(vec![1u8; 32]).len())
    }
}

#[test]
fn counting_allocator_distinguishes_zero_alloc_from_allocating_windows() {
    assert!(counting_allocator_installed());
    let before = AllocationCounts::thread_now();
    let process = AllocationCounts::now();
    let boxed = std::hint::black_box(Box::new([0u8; 48]));
    let after = AllocationCounts::thread_now().since(before).unwrap();
    drop(boxed);
    assert_eq!((after.allocations, after.bytes), (1, 48));
    assert!(AllocationCounts::now().since(process).unwrap().allocations >= 1);
    // Another thread's allocations are not attributed to this one.
    let before = AllocationCounts::thread_now();
    std::thread::spawn(|| std::hint::black_box(vec![0u8; 4096]).len())
        .join()
        .unwrap();
    let spawned = AllocationCounts::thread_now().since(before).unwrap();
    assert!(spawned.bytes < 4096, "{spawned:?}");

    let config = RunConfig {
        epochs: 3,
        warmup_epochs: 1,
        operations_per_window: 16,
        ..RunConfig::default()
    };
    let mut observer = AllocationObserver::open().unwrap();
    let run = run_abba_observed(config, &mut Arith, &mut Arith, &mut observer).unwrap();
    let (_, windows) = run.into_parts();
    assert_eq!(windows.len(), 16);
    for w in windows.iter().filter(|w| !w.invocation.warmup) {
        assert!(
            w.observation.counts.is_zero_allocation(),
            "{:?}",
            w.observation.counts
        );
    }

    let mut observer = AllocationObserver::open().unwrap();
    let run = run_abba_observed(config, &mut Arith, &mut Allocating, &mut observer).unwrap();
    let (_, windows) = run.into_parts();
    for w in windows.iter().filter(|w| !w.invocation.warmup) {
        let counts = w.observation.counts;
        if w.invocation.arm == vesper_bench::Arm::Candidate {
            assert!(
                counts.allocations >= 16 && counts.bytes >= 16 * 32,
                "{counts:?}"
            );
        } else {
            assert!(counts.is_zero_allocation(), "{counts:?}");
        }
    }
}
