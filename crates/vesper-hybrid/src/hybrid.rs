//! The swap-in glue: each swapped module's Vesper binding, wrapped as the
//! module's Rust function table and installed before `main` runs.
//!
//! Installing from a static constructor keeps the included CLI's `main`
//! untouched (the same std start-up, the same CLI thread). The table only
//! stores function pointers, so it needs nothing from std yet.
//!
//! Set `VESPER_HYBRID_STATS=1` to print, at exit, how many calls each
//! swapped module answered from Vesper (the differential harness uses it to
//! show the pilot really ran).
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/layout.rs"]
mod layout_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/parse.rs"]
mod parse_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/check_layout.rs"]
mod check_layout_binding;

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../port/ir_check.rs"]
mod ir_check_binding;

#[path = "parse_words.rs"]
mod parse_words;

// The checker swaps' input encoder and result decoders (#1177), shared with
// the root package's tests/hybrid_layout.rs and tests/hybrid_ir_check.rs.
#[path = "../../../tests/common/check_words.rs"]
mod check_words;

/// Calls answered by the Vesper word-layout module.
static LAYOUT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Sources parsed by the Vesper parse module.
static PARSE_CALLS: AtomicU64 = AtomicU64::new(0);
/// `Layout::of` calls answered by the Vesper check-layout module.
static CHECK_LAYOUT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Programs checked by the Vesper IR-check module.
static IR_CHECK_CALLS: AtomicU64 = AtomicU64::new(0);

fn count() {
    LAYOUT_CALLS.fetch_add(1, Relaxed);
}

/// `vesper::layout::WordLayout` from `modules/layout`.
const VESPER_WORD_LAYOUT: vesper::layout::WordLayout = vesper::layout::WordLayout {
    first_offset: |is_enum| {
        count();
        layout_binding::first_offset(is_enum)
    },
    next_offset: |offset, size| {
        count();
        let next = layout_binding::next_offset(offset, size);
        (next != layout_binding::no_fit()).then_some(next as i32)
    },
    product_size: |end| {
        count();
        layout_binding::product_size(end)
    },
    enum_size: |size, end| {
        count();
        layout_binding::enum_size(size, end)
    },
    word_offset: |index| {
        count();
        layout_binding::word_offset(index)
    },
};

/// `vesper_syntax::parser::ParseTable` from `modules/parse`: the front end
/// v2 and the AST of `@vesper/ast`, read back by `parse_words::decode`.
const VESPER_PARSE: vesper_syntax::parser::ParseTable = vesper_syntax::parser::ParseTable {
    parse: |source| {
        PARSE_CALLS.fetch_add(1, Relaxed);
        parse_words::decode(source, &parse_binding::parse_words(source))
    },
};

/// `vesper::layout::LayoutTable` from `modules/check_layout`: the layout of
/// `@vesper/check-layout`, over the ADTs the type can reach. A type whose
/// literals the decoder cannot read (over 64 bits) stays in Rust.
const VESPER_LAYOUT: vesper::layout::LayoutTable = vesper::layout::LayoutTable {
    of: |world, ty| {
        let Some(input) = check_words::layout_input(world, ty) else {
            return vesper::layout::rust_layout_of(world, ty);
        };
        CHECK_LAYOUT_CALLS.fetch_add(1, Relaxed);
        check_words::decode_layout(&check_layout_binding::layout_of_words(&input))
    },
};

/// `vesper::ir_check::IrCheckTable` from `modules/ir_check`: the IR check
/// of `@vesper/ir-check`. A program whose literals the decoder cannot read
/// stays in Rust.
const VESPER_IR_CHECK: vesper::ir_check::IrCheckTable = vesper::ir_check::IrCheckTable {
    validate: |world, program| {
        // `ir_input` refuses what `@vesper/ir-check` does not cover yet,
        // among them every world with element view matches (#1221): those
        // go to `rust_validate` (test: `element_views_stay_on_rust` in
        // tests/hybrid_ir_check.rs).
        let Some(input) = check_words::ir_input(world, program) else {
            return vesper::ir_check::rust_validate(world, program);
        };
        IR_CHECK_CALLS.fetch_add(1, Relaxed);
        check_words::decode_diagnostics(&ir_check_binding::validate_words(&input))
    },
};

extern "C" fn install() {
    // Nothing runs before this constructor, so the slots are empty.
    if vesper::layout::install_word_layout(VESPER_WORD_LAYOUT).is_err() {
        std::process::abort();
    }
    if vesper_syntax::parser::install_parse(VESPER_PARSE).is_err() {
        std::process::abort();
    }
    if vesper::layout::install_layout(VESPER_LAYOUT).is_err() {
        std::process::abort();
    }
    if vesper::ir_check::install_ir_check(VESPER_IR_CHECK).is_err() {
        std::process::abort();
    }
    if std::env::var_os("VESPER_HYBRID_STATS").is_some_and(|v| v == "1") {
        unsafe extern "C" {
            fn atexit(f: extern "C" fn()) -> std::ffi::c_int;
        }
        // SAFETY: `report` is a plain `extern "C" fn` that only formats
        // and writes to stderr.
        unsafe { atexit(report) };
    }
}

extern "C" fn report() {
    eprintln!(
        "vesper-hybrid: layout answered {} calls from Vesper",
        LAYOUT_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: parse answered {} calls from Vesper",
        PARSE_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: check-layout answered {} calls from Vesper",
        CHECK_LAYOUT_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: ir-check answered {} calls from Vesper",
        IR_CHECK_CALLS.load(Relaxed)
    );
    eprintln!(
        "vesper-hybrid: the driver ran {} passes from Vesper",
        crate::driver::PASSES.load(Relaxed)
    );
}

// Run `install` before `main` (the platform's static-constructor section).
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_vendor = "apple",
    windows
)))]
compile_error!(
    "vesper-hybrid installs its modules from a static constructor; add this target's section"
);

#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_os = "freebsd"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
#[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
static INSTALL: extern "C" fn() = install;
