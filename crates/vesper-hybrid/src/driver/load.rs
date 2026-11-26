//! The Vesper driver's own read of the package graph (#1986 slice 2): the
//! driver reads `Loom.toml`, walks `src/` and reads the `.vsys` and object
//! files itself (modules/driver/src/load.ves), and asks these steps for what
//! still runs over the Rust parsers: the manifest decode, the dependency
//! version match and the `.vsys` type names. `Graph` hands the graph it read
//! (the assembler's input, the `.vsys` libraries and the objects) to the
//! session for `hd_assembled`.
//!
//! Every argument and result is a byte stream of little-endian `u64`s and
//! texts (the length, then the bytes zero-padded to eight), the layout of
//! the assembler's input (packages/vesper-check-names-packages/src/load.ves).
//!
//! ```text
//! LoadBegin  -> 0 the Rust loader reads the graph | 1, text: wasi, host
//!               target (VSYS_TARGETS index, 4 for none), the parser's
//!               byte budget, the source-directory OS
//! Manifest   Loom.toml -> 0 a manifest error | the manifest's index + 1,
//!               text: name, n vsys paths, n object paths, n dependencies
//!               (name, optional, has path, path), in name order
//! VersionMatches  manifest, found manifest, dependency name -> 0 | 1
//! VsysNames  target, then the .vsys text -> 0 no parse | 1 | 2 static,
//!               text: n type names
//! Graph      input length, the assembler's input, n vsys (package,
//!               display, text, n targets (target, display, text)),
//!               n objects (package, display, n files (target or 4, path))
//! ```

use super::*;
use vesper::packages::driver_host::{self, DriverManifest};

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

impl<'a> In<'a> {
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
    pub(crate) fn s(&mut self) -> Result<&'a str, String> {
        let len = self.count()?;
        let bytes = self
            .b
            .get(self.at..self.at + len)
            .ok_or("driver: short load stream text")?;
        self.at += len.next_multiple_of(8);
        std::str::from_utf8(bytes).map_err(|e| format!("driver: load stream text: {e}"))
    }
}

impl Session {
    pub(super) fn load_step(&mut self, step: Step, arg: &[u8]) -> Result<i64, String> {
        match step {
            Step::LoadBegin => {
                self.manifests.clear();
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
            Step::Manifest => {
                let Some(m) = std::str::from_utf8(arg)
                    .ok()
                    .and_then(DriverManifest::parse)
                else {
                    return Ok(0);
                };
                let mut out = Out::default();
                out.s(m.name());
                for list in [m.vsys(), m.objects()] {
                    out.n(list.len() as u64);
                    for path in list {
                        out.s(path);
                    }
                }
                let deps: Vec<_> = m.dependencies().collect();
                out.n(deps.len() as u64);
                for d in deps {
                    out.s(d.name);
                    out.n(u64::from(d.optional));
                    out.n(u64::from(d.path.is_some()));
                    out.s(d.path.unwrap_or(""));
                }
                self.text = out.0;
                self.manifests.push(m);
                Ok(count(self.manifests.len()))
            }
            Step::VersionMatches => {
                let mut r = In { b: arg, at: 0 };
                let (m, found, name) = (r.count()?, r.count()?, r.s()?);
                let (Some(m), Some(found)) = (self.manifests.get(m), self.manifests.get(found))
                else {
                    return Err("driver: no such manifest".into());
                };
                Ok(i64::from(m.accepts(name, found)))
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
                self.manifests.clear();
                self.reading = false;
                self.graph = Some(crate::vesper_hybrid::driver_graph(arg)?);
                Ok(0)
            }
            _ => Err(format!("driver: step {} is not a load step", step as u32)),
        }
    }
}
