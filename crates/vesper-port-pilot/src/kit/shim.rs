//! The crate-provided side of the `.vsys` import in `vesper/kit.ves`
//! (`sys library Clock`): a C-ABI function the Vesper object calls. The
//! generated binding refers to it, so it links with the Vesper object.

/// The pilot's clock: a fixed tick count, so tests can predict
/// [`super::stamp`].
#[unsafe(no_mangle)]
pub extern "C" fn pilot_clock_ticks() -> u64 {
    TICKS
}

/// What [`pilot_clock_ticks`] returns.
pub const TICKS: u64 = 1_000;
