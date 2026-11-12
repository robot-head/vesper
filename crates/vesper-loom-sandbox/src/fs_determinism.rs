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
//! * `stat`/`stat-at`: all three timestamps become the configured wall clock,
//!   link counts are one, and directory sizes are zero (host inode metadata
//!   must not become an undeclared build input).
//! * `metadata-hash`/`metadata-hash-at`: host hashes are renumbered in order
//!   of first observation within the run, preserving "same object" equality.
//! * `read-directory`: entries are read eagerly and yielded sorted by name.
//!
//! Permission checks are untouched: every wrapper delegates to the stock
//! host implementation first.

use std::collections::{HashMap, VecDeque};

use wasmtime::StoreContextMut;
use wasmtime::component::{Linker, Resource, ResourceType};
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
        s.link_count = 1;
        if s.type_ == fs::DescriptorType::Directory {
            s.size = 0;
        }
        s.data_access_timestamp = self.timestamp;
        s.data_modification_timestamp = self.timestamp;
        s.status_change_timestamp = self.timestamp;
        s
    }

    fn next_directory_entry(
        &mut self,
        rep: u32,
    ) -> wasmtime::Result<Result<Option<DirectoryEntry>, ErrorCode>> {
        let Some(queue) = self.sorted_dirs.get_mut(&rep) else {
            wasmtime::bail!("unknown directory-entry-stream");
        };
        // An exhausted stream remains valid until the guest drops it. In
        // particular, repeated reads after EOF must continue returning None.
        Ok(queue.pop_front().transpose())
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
            Ok((store
                .data_mut()
                .normalizer()
                .next_directory_entry(stream.rep())?,))
        },
    )?;
    inst.resource(
        "directory-entry-stream",
        ResourceType::host::<fs::DirectoryEntryStream>(),
        |mut store, rep| {
            HostDirectoryEntryStream::drop(
                &mut store.data_mut().filesystem(),
                Resource::new_own(rep),
            )?;
            store.data_mut().normalizer().sorted_dirs.remove(&rep);
            Ok(())
        },
    )?;
    linker.allow_shadowing(false);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inode_metadata_is_not_an_observable_build_input() {
        let fs = FsNormalizer::new(123);
        let stat = |type_, size, link_count| DescriptorStat {
            type_,
            size,
            link_count,
            data_access_timestamp: None,
            data_modification_timestamp: None,
            status_change_timestamp: None,
        };
        for (size, links) in [(4096, 2), (16384, 7)] {
            let normalized = fs.stat(stat(fs::DescriptorType::Directory, size, links));
            assert_eq!(normalized.size, 0);
            assert_eq!(normalized.link_count, 1);
            assert_eq!(normalized.data_access_timestamp.unwrap().nanoseconds, 123);
            assert_eq!(
                normalized.data_modification_timestamp.unwrap().nanoseconds,
                123
            );
            assert_eq!(normalized.status_change_timestamp.unwrap().nanoseconds, 123);
        }
        let file = fs.stat(stat(fs::DescriptorType::RegularFile, 42, 8));
        assert_eq!(file.size, 42);
        assert_eq!(file.link_count, 1);
    }

    #[test]
    fn directory_stream_remains_exhausted_until_dropped() {
        let mut fs = FsNormalizer::default();
        fs.sorted_dirs.insert(
            7,
            VecDeque::from([Ok(DirectoryEntry {
                type_: fs::DescriptorType::RegularFile,
                name: "input.txt".into(),
            })]),
        );
        assert_eq!(
            fs.next_directory_entry(7).unwrap().unwrap().unwrap().name,
            "input.txt"
        );
        for _ in 0..3 {
            assert!(fs.next_directory_entry(7).unwrap().unwrap().is_none());
        }
        fs.sorted_dirs.remove(&7);
        assert!(fs.next_directory_entry(7).is_err());
    }
}
