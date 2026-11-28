//! The Vesper driver's own read of the package graph (#1986 slice 2): the
//! driver reads and decodes every `Loom.toml` (slice 2b), walks `src/` and
//! reads the `.vsys` and object files itself (modules/driver/src/load.ves),
//! with each `.vsys` library's link mode and type names from @vesper/vsys
//! and the read's settings (the target's source-directory OS, the host's
//! `.vsys` target, the parser's byte budget) its own (#2760). `LoadBegin`
//! says whether the driver reads the graph (the `load` swap) and `Graph`
//! hands the graph it read (the assembler's input, the `.vsys` libraries and
//! the objects) to the session for `hd_assembled`.
//!
//! `Graph`'s argument is a byte stream of little-endian `u64`s and texts
//! (the length, then the bytes zero-padded to eight), the layout of the
//! assembler's input (packages/vesper-check-names-packages/src/load.ves).
//!
//! ```text
//! LoadBegin  -> 0 the Rust loader reads the graph | 1 the driver does
//! Graph      input length, the assembler's input, n vsys (package,
//!               display, text, n targets (target, display, text)),
//!               n objects (package, display, n files (target or 4, path))
//! ```

use super::*;

impl Session {
    pub(super) fn load_step(&mut self, step: Step, arg: &[u8]) -> Result<i64, String> {
        match step {
            Step::LoadBegin => {
                self.graph = None;
                self.reading = false;
                if !crate::vesper_hybrid::driver_reads(self.o.include_tests) {
                    return Ok(0);
                }
                self.reading = true;
                crate::vesper_hybrid::driver_read_begin();
                Ok(1)
            }
            Step::Graph => {
                self.reading = false;
                self.graph = Some(crate::vesper_hybrid::driver_graph(arg)?);
                Ok(0)
            }
            _ => Err(format!("driver: step {} is not a load step", step as u32)),
        }
    }
}
