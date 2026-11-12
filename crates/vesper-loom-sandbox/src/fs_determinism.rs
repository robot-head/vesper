//! Deterministic views of host filesystem metadata.
//!
//! `wasmtime-wasi` passes host metadata through unchanged: real timestamps
//! (including the creation time of files a step writes), inode/device
//! derived `metadata-hash` values, and the host's directory iteration order.
//! Each would let identical invocations observe different values. These
//! shadowing definitions replace the affected `wasi:filesystem/types`
//! functions with wrappers that call the stock implementation and then
//! normalize what the guest sees:
//!
//! * `stat`/`stat-at`: all three timestamps become the configured wall clock.
//! * `metadata-hash`/`metadata-hash-at`: host hashes are renumbered in order
//!   of first observation within the run, preserving "same object" equality.
//! * `read-directory`: entries are read eagerly and yielded sorted by name.
//!
//! Permission checks are untouched: every wrapper delegates to the stock
//! host implementation first.

use std::collections::{HashMap, VecDeque};

use wasmtime::StoreContextMut;
use wasmtime::component::{Linker, Resource};
use wasmtime_wasi::filesystem::WasiFilesystemView;
use wasmtime_wasi::p2::FsResult;
use wasmtime_wasi::p2::bindings::filesystem::types::{
    self as fs, Datetime, DescriptorStat, DirectoryEntry, ErrorCode, HostDescriptor,
    HostDirectoryEntryStream, MetadataHashValue, PathFlags,
};

/// Must name the same package version `wasmtime_wasi::p2::add_to_linker_async`
/// registers, or the definitions would not shadow it (checked by tests).
pub(crate) const FILESYSTEM_TYPES: &str = "wasi:filesystem/types@0.2.12";

/// Per-run state backing the normalized views.
#[derive(Default)]
pub(crate) struct FsNormalizer {
    pub timestamp: Option<Datetime>,
    identities: HashMap<(u64, u64), u64>,
    sorted_dirs: HashMap<u32, VecDeque<Result<DirectoryEntry, ErrorCode>>>,
}

impl FsNormalizer {
    pub(crate) fn new(wall_clock_ns: u64) -> Self {
        Self {
            timestamp: Some(Datetime {
                seconds: wall_clock_ns / 1_000_000_000,
                nanoseconds: (wall_clock_ns % 1_000_000_000) as u32,
            }),
            ..Self::default()
        }
    }

    fn stat(&self, mut s: DescriptorStat) -> DescriptorStat {
        s.data_access_timestamp = self.timestamp;
        s.data_modification_timestamp = self.timestamp;
        s.status_change_timestamp = self.timestamp;
        s
    }

    fn identity(&mut self, h: MetadataHashValue) -> MetadataHashValue {
        let next = self.identities.len() as u64 + 1;
        let id = *self.identities.entry((h.upper, h.lower)).or_insert(next);
        MetadataHashValue {
            lower: id,
            upper: 0,
        }
    }
}

pub(crate) trait HasFsNormalizer: WasiFilesystemView + 'static {
    fn normalizer(&mut self) -> &mut FsNormalizer;
}

fn lower<T>(r: FsResult<T>) -> wasmtime::Result<Result<T, ErrorCode>> {
    match r {
        Ok(v) => Ok(Ok(v)),
        Err(e) => e.downcast().map(Err),
    }
}

type Desc = Resource<fs::Descriptor>;

pub(crate) fn add_to_linker<T: HasFsNormalizer>(linker: &mut Linker<T>) -> wasmtime::Result<()> {
    linker.allow_shadowing(true);
    let mut inst = linker.instance(FILESYSTEM_TYPES)?;

    inst.func_wrap_async(
        "[method]descriptor.stat",
        |mut store: StoreContextMut<'_, T>, (fd,): (Desc,)| {
            Box::new(async move {
                let r = lower(HostDescriptor::stat(&mut store.data_mut().filesystem(), fd).await)?;
                let n = store.data_mut().normalizer();
                Ok((r.map(|s| n.stat(s)),))
            })
        },
    )?;
    inst.func_wrap_async(
        "[method]descriptor.stat-at",
        |mut store: StoreContextMut<'_, T>, (fd, flags, path): (Desc, PathFlags, String)| {
            Box::new(async move {
                let r = lower(
                    HostDescriptor::stat_at(&mut store.data_mut().filesystem(), fd, flags, path)
                        .await,
                )?;
                let n = store.data_mut().normalizer();
                Ok((r.map(|s| n.stat(s)),))
            })
        },
    )?;
    inst.func_wrap_async(
        "[method]descriptor.metadata-hash",
        |mut store: StoreContextMut<'_, T>, (fd,): (Desc,)| {
            Box::new(async move {
                let r = lower(
                    HostDescriptor::metadata_hash(&mut store.data_mut().filesystem(), fd).await,
                )?;
                let n = store.data_mut().normalizer();
                Ok((r.map(|h| n.identity(h)),))
            })
        },
    )?;
    inst.func_wrap_async(
        "[method]descriptor.metadata-hash-at",
        |mut store: StoreContextMut<'_, T>, (fd, flags, path): (Desc, PathFlags, String)| {
            Box::new(async move {
                let r = lower(
                    HostDescriptor::metadata_hash_at(
                        &mut store.data_mut().filesystem(),
                        fd,
                        flags,
                        path,
                    )
                    .await,
                )?;
                let n = store.data_mut().normalizer();
                Ok((r.map(|h| n.identity(h)),))
            })
        },
    )?;
    inst.func_wrap_async(
        "[method]descriptor.read-directory",
        |mut store: StoreContextMut<'_, T>, (fd,): (Desc,)| {
            Box::new(async move {
                let mut view = store.data_mut().filesystem();
                let stream = match lower(HostDescriptor::read_directory(&mut view, fd).await)? {
                    Ok(s) => s,
                    Err(e) => return Ok((Err(e),)),
                };
                let rep = stream.rep();
                let mut entries = Vec::new();
                let mut failure = None;
                loop {
                    let next = HostDirectoryEntryStream::read_directory_entry(
                        &mut view,
                        Resource::new_borrow(rep),
                    )
                    .await;
                    match lower(next)? {
                        Ok(Some(e)) => entries.push(e),
                        Ok(None) => break,
                        Err(e) => {
                            failure = Some(e);
                            break;
                        }
                    }
                }
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                let mut queue: VecDeque<_> = entries.into_iter().map(Ok).collect();
                if let Some(e) = failure {
                    queue.push_back(Err(e));
                }
                store.data_mut().normalizer().sorted_dirs.insert(rep, queue);
                Ok((Ok(stream),))
            })
        },
    )?;
    inst.func_wrap(
        "[method]directory-entry-stream.read-directory-entry",
        |mut store: StoreContextMut<'_, T>,
         (stream,): (Resource<fs::DirectoryEntryStream>,)|
         -> wasmtime::Result<(Result<Option<DirectoryEntry>, ErrorCode>,)> {
            let dirs = &mut store.data_mut().normalizer().sorted_dirs;
            let Some(queue) = dirs.get_mut(&stream.rep()) else {
                // Every stream is created by the wrapper above.
                wasmtime::bail!("unknown directory-entry-stream");
            };
            Ok((match queue.pop_front() {
                Some(Ok(e)) => Ok(Some(e)),
                Some(Err(e)) => Err(e),
                None => {
                    dirs.remove(&stream.rep());
                    Ok(None)
                }
            },))
        },
    )?;
    linker.allow_shadowing(false);
    Ok(())
}
