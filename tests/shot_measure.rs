//! Render time and heap use of one `vesper shot` card, measured, not asserted
//! (debug builds are far slower than release). Run with
//! `cargo test --release --test shot_measure -- --nocapture` for current measurements.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vesper_highlight::shot::{
    self, RasterOptions, SceneOptions, ShotFont, ShotLimits, ShotTheme, SvgOptions,
};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const FONT: &[u8] = include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf");

fn spans(display: &str) -> Vec<vesper_highlight::HighlightSpan> {
    vesper_highlight::highlight_tier0(display).unwrap()
}

/// Mean time and peak heap above the starting live bytes for `f`.
fn measure(runs: u32, mut f: impl FnMut() -> usize) -> (Duration, usize, usize) {
    let mut out = f(); // warm up
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..runs {
        out = f();
    }
    (
        start.elapsed() / runs,
        PEAK.load(Ordering::Relaxed) - base,
        out,
    )
}

#[test]
fn render_time_and_heap_per_card() {
    let src = std::fs::read_to_string(
        std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))
            .unwrap()
            .join("tests/fixtures/shot/wal_txn.vsp"),
    )
    .unwrap();
    let font = ShotFont::parse(FONT).unwrap();
    let doc = shot::parse_shot_source(&src, ShotLimits::default()).unwrap();
    let spans = spans(&doc.display);
    let pins = shot::resolve_callouts(&doc, &spans, |_, _, _| None).unwrap();
    let palette = ShotTheme::Dusk.palette();
    let build = |dpi| {
        shot::build_code_card_scene(
            "wal_txn.vsp",
            &doc.display,
            &spans,
            &pins,
            &palette,
            &font,
            SceneOptions {
                dpi_scale: dpi,
                ..SceneOptions::default()
            },
        )
        .unwrap()
    };
    let scene = build(1);
    eprintln!(
        "card {}x{} CSS px, {} lines, {} primitives, profile {}",
        scene.width_px,
        scene.height_px,
        doc.display.lines().count(),
        scene.primitives.len(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    let (t, peak, _) = measure(if cfg!(debug_assertions) { 2 } else { 50 }, || {
        build(1).primitives.len()
    });
    eprintln!("scene only:          {t:>12.3?}  peak heap {peak} B");
    let (t, peak, n) = measure(if cfg!(debug_assertions) { 2 } else { 50 }, || {
        shot::render_svg(&build(1), &font, SvgOptions::default()).len()
    });
    eprintln!("scene + SVG:         {t:>12.3?}  peak heap {peak} B  ({n} B out)");
    for dpi in [1u32, 2, 4] {
        let runs = if cfg!(debug_assertions) {
            1
        } else if dpi == 4 {
            5
        } else {
            20
        };
        let (t, peak, n) = measure(runs, || {
            shot::render_png(&build(dpi), &font, RasterOptions::default())
                .unwrap()
                .len()
        });
        eprintln!("scene + PNG {dpi}x:      {t:>12.3?}  peak heap {peak} B  ({n} B out)");
    }
}
