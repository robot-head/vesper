//! The Vesper driver's own read of the package graph (#1986 slice 2): the
//! driver reads and decodes every `Loom.toml` (slice 2b), walks `src/` and
//! reads the `.vsys` and object files itself (modules/driver/src/load.ves),
//! and asks `VsysNames` for a `.vsys` library's type names, which still run
//! over `vesper_ffi::sys`. `LoadBegin` hands it the settings and `Graph`
//! hands the graph it read (the assembler's input, the `.vsys` libraries and
//! the objects) to the session for `hd_assembled`.
//!
//! Every argument and result is a byte stream of little-endian `u64`s and
//! texts (the length, then the bytes zero-padded to eight), the layout of
//! the assembler's input (packages/vesper-check-names-packages/src/load.ves).
//!
//! ```text
//! LoadBegin  -> 0 the Rust loader reads the graph | 1, text: wasi, host
//!               target (VSYS_TARGETS index, 4 for none), the parser's
//!               byte budget, the source-directory OS
//! VsysNames  target, then the .vsys text -> 0 no parse | 1 | 2 static,
//!               text: n type names
//! Graph      input length, the assembler's input, n vsys (package,
//!               display, text, n targets (target, display, text)),
//!               n objects (package, display, n files (target or 4, path))
//! ```

use super::*;
use vesper::packages::driver_host;

/// The byte stream of the steps' results.
#[derive(Default)]
pub(crate) struct Out(pub(crate) Vec<u8>);

impl Out {
    pub(crate) fn n(&mut self, v: u64) {
        self.0.extend(v.to_le_bytes());
    }
    pub(crate) fn s(&mut self, s: &str) {
        self.n(s.len() as u64);
        self.0.extend(s.as_bytes());
        self.0.resize(self.0.len().next_multiple_of(8), 0);
    }
}

/// The byte stream of the steps' arguments.
pub(crate) struct In<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) at: usize,
}

impl In<'_> {
    pub(crate) fn n(&mut self) -> Result<u64, String> {
        let w = self
            .b
            .get(self.at..self.at + 8)
            .ok_or("driver: short load stream")?;
        self.at += 8;
        Ok(u64::from_le_bytes(w.try_into().expect("eight bytes")))
    }
    pub(crate) fn count(&mut self) -> Result<usize, String> {
        usize::try_from(self.n()?).map_err(|_| "driver: load stream count".to_owned())
    }
}

impl Session {
    pub(super) fn load_step(&mut self, step: Step, arg: &[u8]) -> Result<i64, String> {
        match step {
            Step::LoadBegin => {
                self.graph = None;
                self.reading = false;
                if !crate::vesper_hybrid::driver_reads(self.o.include_tests) {
                    return Ok(0);
                }
                let (os, wasi) = driver_host::target_os(self.o.target());
                let mut out = Out::default();
                out.n(u64::from(wasi));
                out.n(driver_host::host_target().map_or(4, |i| i as u64));
                out.n(driver_host::max_source_bytes() as u64);
                out.s(os);
                self.text = out.0;
                self.reading = true;
                crate::vesper_hybrid::driver_read_begin();
                Ok(1)
            }
            Step::VsysNames => {
                let mut r = In { b: arg, at: 0 };
                let target = r.count()?;
                let text = utf8(&arg[r.at..])?;
                let Some((statically, names)) = driver_host::vsys_names(target, text) else {
                    self.text.clear();
                    return Ok(0);
                };
                let mut out = Out::default();
                out.n(names.len() as u64);
                for n in &names {
                    out.s(n);
                }
                self.text = out.0;
                Ok(if statically { 2 } else { 1 })
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
