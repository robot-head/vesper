// SH7 scalar machine code, using the Vesper module on an explicit opt-in.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/native_backend.rs"]
mod binding;

use super::{Registry, Swap, must, on, open};
use vesper::native::machine;

static ENCODED: AtomicU64 = AtomicU64::new(0);

pub(super) const SWAPS: &[Swap] = &[open(
    "native_backend",
    "experimental x86-64 integer blocks, requires VESPER_HYBRID_NATIVE_BACKEND=1",
)];

pub(super) fn register(_r: &mut Registry) {
    if on("native_backend")
        && std::env::var_os("VESPER_HYBRID_NATIVE_BACKEND").is_some_and(|value| value == "1")
    {
        must(machine::install_integer_encoder(|words| {
            let bytes = binding::x86_64_integer_block(words);
            if bytes.is_empty() {
                return None;
            }
            ENCODED.fetch_add(1, Relaxed);
            Some(bytes)
        }));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: native_backend emitted {} x86-64 integer blocks",
        ENCODED.load(Relaxed),
    );
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
#[path = "native_backend/differential.rs"]
mod differential;
