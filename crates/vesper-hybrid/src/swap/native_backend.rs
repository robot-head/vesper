// SH7 scalar machine code, using the Vesper module on an explicit opt-in.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/native_backend.rs"]
mod binding;

use super::{Registry, Swap, must, on, open};
use vesper::native::{elf, machine};

static ENCODED: AtomicU64 = AtomicU64::new(0);
static AARCH64_BLOCKS: AtomicU64 = AtomicU64::new(0);
static ELF_OBJECTS: AtomicU64 = AtomicU64::new(0);

pub(super) const SWAPS: &[Swap] = &[
    open(
        "native_backend",
        "experimental x86-64 blocks and AArch64 integer blocks, requires VESPER_HYBRID_NATIVE_BACKEND=1",
    ),
    open(
        "native_elf",
        "experimental x86-64 ELF writer, requires VESPER_HYBRID_NATIVE_ELF=1",
    ),
];

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
        must(machine::install_relocation_encoder(|words| {
            let packet = binding::x86_64_relocatable_block(words);
            if packet.is_empty() {
                return None;
            }
            ENCODED.fetch_add(1, Relaxed);
            Some(packet)
        }));
        must(machine::install_windows_encoder(|words| {
            let packet = binding::x86_64_windows_leaf(words);
            if packet.is_empty() {
                return None;
            }
            ENCODED.fetch_add(1, Relaxed);
            Some(packet)
        }));
        must(machine::install_aarch64_encoder(|words| {
            let bytes = binding::aarch64_relocatable_block(words);
            if bytes.is_empty() {
                return None;
            }
            AARCH64_BLOCKS.fetch_add(1, Relaxed);
            Some(bytes)
        }));
    }
    if on("native_elf")
        && std::env::var_os("VESPER_HYBRID_NATIVE_ELF").is_some_and(|value| value == "1")
    {
        must(elf::install_object_encoder(|words, data| {
            let bytes = binding::x86_64_elf_object(words, data);
            if bytes.is_empty() {
                return None;
            }
            ELF_OBJECTS.fetch_add(1, Relaxed);
            Some(bytes)
        }));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: native_backend emitted {} x86-64 integer blocks",
        ENCODED.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: native_backend emitted {} AArch64 integer blocks",
        AARCH64_BLOCKS.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: native_elf wrote {} x86-64 ELF objects",
        ELF_OBJECTS.load(Relaxed),
    );
}
