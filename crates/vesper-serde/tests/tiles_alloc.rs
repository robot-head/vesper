//! "0 Alloc" check: a counting global allocator over a warmed-up steady state.
//! Allocations are counted only on the measuring thread.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use vesper_serde::simd::Isa;
use vesper_serde::tiles::{TelemetryIngestor, TelemetryTiles};
use vesper_serde::{Limits, SimdParser};

struct Counting;
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(0));
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    COUNT.with(Cell::get)
}

fn batch(seed: usize, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| {
            let i = i + seed;
            format!(
                "{{\"ts_unix_ms\":{},\"latency_us\":{},\"status_code\":{},\"flags\":{},\"meta\":{{\"k\":\"v\\n{}\"}},\"trace_id_hex\":\"{:032x}\"}}\n",
                1_700_000_000_000u64 + i as u64,
                i % 100_000,
                200 + i % 300,
                i % 7,
                i,
                i as u128 * 0x9e37_79b9
            )
        })
        .collect::<String>()
        .into_bytes()
}

#[test]
fn steady_state_ingestion_and_parsing_make_no_allocation() {
    for isa in Isa::available() {
        // Warm-up records are at least as long as the steady-state ones.
        let warm = batch(90_000, 256);
        let steady = batch(1000, 256);
        let mut ingestor = TelemetryIngestor::with_isa(isa, Limits::default()).unwrap();
        let mut tiles = TelemetryTiles::with_capacity(256).unwrap();
        // Warm-up grows the parser's index and tape to their working size.
        let first = allocations(|| {
            ingestor.ingest(&warm, &mut tiles).unwrap();
        });
        assert!(first > 0, "{isa:?}: warm-up is expected to allocate");
        for round in 0..3 {
            tiles.clear();
            let n = allocations(|| {
                assert_eq!(ingestor.ingest(&steady, &mut tiles).unwrap(), 256);
            });
            assert_eq!(n, 0, "{isa:?} round {round}: steady-state allocations");
        }
        // A failing record is also allocation-free once warm.
        let mut bad = steady.clone();
        bad.extend_from_slice(b"{\"ts_unix_ms\":1,\"latency_us\":x}\n");
        tiles.clear();
        let n = allocations(|| {
            assert!(ingestor.ingest(&bad, &mut tiles).is_err());
        });
        assert_eq!(n, 0, "{isa:?}: error path allocations");

        let mut parser = SimdParser::with_isa(isa).unwrap();
        let doc = br#"{"a":[1,2,3,{"b":"c\u00e9"}],"d":true}"#;
        parser.parse(doc, Limits::default(), |_| ()).unwrap();
        let n = allocations(|| {
            for _ in 0..10 {
                let id = parser
                    .parse(doc, Limits::default(), |d| {
                        d.root().field("d", 100).unwrap().unwrap().as_bool()
                    })
                    .unwrap();
                assert_eq!(id, Ok(true));
            }
        });
        assert_eq!(n, 0, "{isa:?}: reused SimdParser allocations");
    }
}
