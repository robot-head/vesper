//! Sync fds for timeline points (§19.9).
//!
//! A [`SyncFd`] is a pollable file descriptor that becomes readable once a
//! timeline semaphore of a [`VulkanBackend`](super::VulkanBackend) reaches a
//! value, so a Pulse driver can wait for GPU work next to sockets and files
//! (see [`crate::pulse`]). Two implementations exist:
//!
//! * **Native** (`VK_KHR_external_semaphore_fd`, handle type `SYNC_FD`): a
//!   submission on the timeline's queue waits for the value and signals a
//!   binary semaphore, which is exported as a Linux `sync_file`. A sync_file
//!   polls readable (`EPOLLIN`) when its fence signals; reading it then fails
//!   with `EINVAL`, which is how the waiter recognises the signal. An export
//!   of an already signalled payload may return `-1`; that point is
//!   represented by a pipe that is readable from the start. A fence on the
//!   submission tells [`SyncFd::close`] when the semaphore can be destroyed.
//! * **Bridged**, where the extension is missing (lavapipe exposes it only
//!   when `/dev/udmabuf` exists): a thread blocks in `vkWaitSemaphores` and
//!   writes one byte into a pipe whose read end is the fd. `Auto` also uses
//!   the bridge for points not submitted yet: a native export of such a
//!   point would wait on the same queue ahead of its own signal.
//!
//! The exporter is taken from the backend before the backend moves into a
//! `Runtime`. Every exported fd is a linear owner: the backend refuses to be
//! destroyed while one is open, because both implementations use its
//! timeline semaphores.

use super::device::{Inner, count_leak};
use crate::error::VulkanError;
use ash::vk;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Which implementation [`SyncFdExporter::export`] uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncFdMode {
    /// Native when the device supports it, bridged otherwise.
    Auto,
    /// Native only; [`VulkanError::Unsupported`] when unavailable.
    Native,
    /// Always bridged.
    Bridged,
}

/// The implementation behind a [`SyncFd`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncFdKind {
    /// A `sync_file` exported from a binary semaphore.
    Native,
    /// The export reported an already signalled payload (fd `-1`); the fd is
    /// a pipe that is already readable.
    NativeSignaled,
    /// A pipe written by a waiting thread.
    Bridged,
}

#[derive(Debug, Default)]
pub(crate) struct ExportState {
    open: usize,
    closed: bool,
    /// Per timeline: the highest value whose signal has been submitted.
    pub(crate) submitted: Vec<u64>,
}

/// Shared between a backend and its exporters.
pub(crate) type Exports = Arc<Mutex<ExportState>>;

/// Opens and counts exports while the backend lives.
pub(crate) fn exports_open(exports: &Exports) -> usize {
    exports.lock().unwrap_or_else(|p| p.into_inner()).open
}

/// Marks the backend destroyed if no export is open.
pub(crate) fn close_exports(exports: &Exports) -> bool {
    let mut s = exports.lock().unwrap_or_else(|p| p.into_inner());
    if s.open > 0 {
        return false;
    }
    s.closed = true;
    true
}

/// Creates sync fds for the timelines of one backend. See the module docs.
pub struct SyncFdExporter {
    pub(crate) inner: Arc<Inner>,
    /// Per runtime timeline index: its semaphore and queue.
    pub(crate) timelines: Vec<(vk::Semaphore, vk::Queue)>,
    pub(crate) exports: Exports,
}

impl std::fmt::Debug for SyncFdExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncFdExporter")
            .field("timelines", &self.timelines.len())
            .field("native", &self.native_available())
            .finish_non_exhaustive()
    }
}

/// Bridge threads re-check for cancellation this often.
const BRIDGE_SLICE_NS: u64 = 20_000_000;

impl SyncFdExporter {
    /// Whether the native `SYNC_FD` path is available on this device.
    pub fn native_available(&self) -> bool {
        self.inner.semaphore_fd.is_some()
    }

    /// An fd that becomes readable once timeline `timeline` (the runtime's
    /// timeline index) reaches `value`. A point whose signal has not been
    /// submitted yet is bridged (`Auto`) or rejected (`Native`); a point that
    /// is never signalled leaves the fd unreadable.
    pub fn export(
        &self,
        timeline: usize,
        value: u64,
        mode: SyncFdMode,
    ) -> Result<SyncFd, VulkanError> {
        let (semaphore, queue) = *self
            .timelines
            .get(timeline)
            .ok_or(VulkanError::Limit("timeline index"))?;
        if value == 0 {
            return Err(VulkanError::InvalidDesc("sync fd for timeline value 0"));
        }
        self.inner.ensure_live("vkGetSemaphoreFdKHR")?;
        // A native export submits a wait on the timeline's own queue. If the
        // signal of `value` is not submitted yet, it would be submitted after
        // that wait on the same queue and could never run (no forward
        // progress), so only submitted points are exported natively.
        let native = {
            let mut s = self.exports.lock().unwrap_or_else(|p| p.into_inner());
            if s.closed {
                return Err(VulkanError::Unavailable(
                    "the backend of this exporter was destroyed".into(),
                ));
            }
            let submitted = s.submitted.get(timeline).copied().unwrap_or(0) >= value;
            let native = match mode {
                SyncFdMode::Auto => self.native_available() && submitted,
                SyncFdMode::Native if !submitted => {
                    return Err(VulkanError::Unsupported(
                        "a native sync fd for a timeline point not yet submitted",
                    ));
                }
                SyncFdMode::Native => true,
                SyncFdMode::Bridged => false,
            };
            s.open += 1;
            native
        };
        let made = if native {
            self.native(semaphore, queue, value)
        } else {
            self.bridged(semaphore, value).map_err(|e| (e, false))
        };
        match made {
            Ok((fd, kind, reclaim)) => Ok(SyncFd {
                inner: self.inner.clone(),
                exports: self.exports.clone(),
                fd,
                kind,
                reclaim,
                live: true,
            }),
            Err((e, still_used)) => {
                // A submission that could not be reclaimed keeps the export
                // counted (see `Reclaim::reclaim_soon`).
                if !still_used {
                    self.exports.lock().unwrap_or_else(|p| p.into_inner()).open -= 1;
                }
                Err(e)
            }
        }
    }

    fn native(
        &self,
        timeline: vk::Semaphore,
        queue: vk::Queue,
        value: u64,
    ) -> Result<(OwnedFd, SyncFdKind, Reclaim), (VulkanError, bool)> {
        let fd_fns = self.inner.semaphore_fd.as_ref().ok_or((
            VulkanError::Unsupported("VK_KHR_external_semaphore_fd (SYNC_FD)"),
            false,
        ))?;
        let d = &self.inner.device;
        let mut export = vk::ExportSemaphoreCreateInfo::default()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
        let info = vk::SemaphoreCreateInfo::default().push_next(&mut export);
        // SAFETY: valid create info whose chain borrows a local.
        let semaphore = self
            .inner
            .check("vkCreateSemaphore", unsafe {
                d.create_semaphore(&info, None)
            })
            .map_err(|e| (e, false))?;
        // SAFETY: valid create info.
        let fence = match self.inner.check("vkCreateFence", unsafe {
            d.create_fence(&vk::FenceCreateInfo::default(), None)
        }) {
            Ok(f) => f,
            Err(e) => {
                // SAFETY: never submitted.
                unsafe { d.destroy_semaphore(semaphore, None) };
                return Err((e, false));
            }
        };
        let waits = [vk::SemaphoreSubmitInfo::default()
            .semaphore(timeline)
            .value(value)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let signals = [vk::SemaphoreSubmitInfo::default()
            .semaphore(semaphore)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let submit = vk::SubmitInfo2::default()
            .wait_semaphore_infos(&waits)
            .signal_semaphore_infos(&signals);
        // SAFETY: live handles; the queue is externally synchronized by the
        // device's submit lock; the arrays borrow locals.
        let r = {
            let _guard = self
                .inner
                .submit_lock
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            unsafe { d.queue_submit2(queue, &[submit], fence) }
        };
        if let Err(e) = self.inner.check("vkQueueSubmit2", r) {
            // SAFETY: the submission failed, so neither object is in use.
            unsafe {
                d.destroy_fence(fence, None);
                d.destroy_semaphore(semaphore, None);
            }
            return Err((e, false));
        }
        let reclaim = Reclaim::Native { semaphore, fence };
        let get = vk::SemaphoreGetFdInfoKHR::default()
            .semaphore(semaphore)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
        // SAFETY: the semaphore was created exportable as SYNC_FD and its
        // signal operation has been submitted, as SYNC_FD export requires.
        let raw = match self.inner.check("vkGetSemaphoreFdKHR", unsafe {
            fd_fns.get_semaphore_fd(&get)
        }) {
            Ok(raw) => raw,
            Err(e) => return Err((e, !reclaim.reclaim_soon(&self.inner))),
        };
        if raw < 0 {
            // Already signalled: an immediately readable stand-in.
            let fd = match ready_pipe() {
                Ok(fd) => fd,
                Err(e) => return Err((e, !reclaim.reclaim_soon(&self.inner))),
            };
            return Ok((fd, SyncFdKind::NativeSignaled, reclaim));
        }
        // SAFETY: vkGetSemaphoreFdKHR transfers ownership of a new fd.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok((fd, SyncFdKind::Native, reclaim))
    }

    fn bridged(
        &self,
        semaphore: vk::Semaphore,
        value: u64,
    ) -> Result<(OwnedFd, SyncFdKind, Reclaim), VulkanError> {
        let (reader, mut writer) = std::io::pipe().map_err(|e| VulkanError::Io(e.to_string()))?;
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let device = self.inner.device.clone();
        let thread = std::thread::Builder::new()
            .name("ignis-sync-fd".into())
            .spawn(move || {
                let semaphores = [semaphore];
                let values = [value];
                let info = vk::SemaphoreWaitInfo::default()
                    .semaphores(&semaphores)
                    .values(&values);
                loop {
                    // SAFETY: the backend owning the semaphore is not
                    // destroyed while this export is open, and `close`
                    // joins this thread before the export is released.
                    match unsafe { device.wait_semaphores(&info, BRIDGE_SLICE_NS) } {
                        Ok(()) => {
                            return writer
                                .write_all(&[1])
                                .map_err(|e| VulkanError::Io(e.to_string()));
                        }
                        Err(vk::Result::TIMEOUT) if !stop.load(Ordering::Acquire) => {}
                        Err(vk::Result::TIMEOUT) => return Ok(()),
                        Err(e) => {
                            return Err(VulkanError::Vk {
                                call: "vkWaitSemaphores",
                                result: e,
                            });
                        }
                    }
                }
            })
            .map_err(|e| VulkanError::Io(e.to_string()))?;
        Ok((
            reader.into(),
            SyncFdKind::Bridged,
            Reclaim::Bridged {
                cancel,
                thread: Some(thread),
            },
        ))
    }
}

fn ready_pipe() -> Result<OwnedFd, VulkanError> {
    let (reader, mut writer) = std::io::pipe().map_err(|e| VulkanError::Io(e.to_string()))?;
    writer
        .write_all(&[1])
        .map_err(|e| VulkanError::Io(e.to_string()))?;
    Ok(reader.into())
}

enum Reclaim {
    Native {
        semaphore: vk::Semaphore,
        fence: vk::Fence,
    },
    Bridged {
        cancel: Arc<AtomicBool>,
        thread: Option<JoinHandle<Result<(), VulkanError>>>,
    },
    Done,
}

impl Reclaim {
    /// On a failed export: releases the submission's objects if it
    /// completes within a second, and otherwise abandons them (a counted
    /// leak). Returns whether they were released.
    fn reclaim_soon(&self, inner: &Inner) -> bool {
        match self.try_finish(inner, 1_000_000_000) {
            Ok(true) => true,
            _ => {
                count_leak();
                false
            }
        }
    }

    /// Waits up to `timeout_ns` for the export's GPU work and releases it.
    /// `Ok(false)` on timeout (nothing released).
    fn try_finish(&self, inner: &Inner, timeout_ns: u64) -> Result<bool, VulkanError> {
        if let Reclaim::Native { semaphore, fence } = self {
            let d = &inner.device;
            // SAFETY: a live fence of this device.
            match unsafe { d.wait_for_fences(&[*fence], true, timeout_ns) } {
                Err(vk::Result::TIMEOUT) => return Ok(false),
                r => inner.check("vkWaitForFences", r)?,
            }
            // SAFETY: the fence proves the submission that used both objects
            // has completed.
            unsafe {
                d.destroy_fence(*fence, None);
                d.destroy_semaphore(*semaphore, None);
            }
        }
        Ok(true)
    }
}

/// A pollable fd for one timeline point. See the module docs.
#[must_use = "a SyncFd must be released with close()"]
pub struct SyncFd {
    inner: Arc<Inner>,
    exports: Exports,
    fd: OwnedFd,
    kind: SyncFdKind,
    reclaim: Reclaim,
    live: bool,
}

impl std::fmt::Debug for SyncFd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncFd")
            .field("fd", &self.fd)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl AsFd for SyncFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl SyncFd {
    pub fn kind(&self) -> SyncFdKind {
        self.kind
    }

    /// A duplicate of the descriptor as a `File`, for handing to a Pulse
    /// driver (which owns the file of a request until it completes).
    pub fn try_clone_file(&self) -> Result<File, VulkanError> {
        self.fd
            .try_clone()
            .map(File::from)
            .map_err(|e| VulkanError::Io(e.to_string()))
    }

    /// Releases the export after waiting up to `timeout_ns` for its GPU
    /// work (native) or stopping its thread (bridged). Duplicates handed out
    /// by [`try_clone_file`](Self::try_clone_file) stay valid descriptors.
    pub fn close(mut self, timeout_ns: u64) -> Result<(), CloseError> {
        let outcome = match &mut self.reclaim {
            Reclaim::Native { .. } => match self.reclaim.try_finish(&self.inner, timeout_ns) {
                Ok(true) => Ok(()),
                Ok(false) => return Err(CloseError::Pending(self)),
                Err(e) => {
                    // Nothing proves the submission finished (device lost):
                    // keep its objects and keep the export counted, so the
                    // backend's semaphores are not destroyed under it.
                    self.reclaim = Reclaim::Done;
                    self.live = false;
                    return Err(CloseError::Failed(e));
                }
            },
            Reclaim::Bridged { cancel, thread } => {
                cancel.store(true, Ordering::Release);
                match thread.take().map(JoinHandle::join) {
                    Some(Ok(r)) => r,
                    Some(Err(_)) => Err(VulkanError::Io("sync fd bridge thread panicked".into())),
                    None => Ok(()),
                }
            }
            Reclaim::Done => Ok(()),
        };
        self.reclaim = Reclaim::Done;
        self.exports.lock().unwrap_or_else(|p| p.into_inner()).open -= 1;
        self.live = false;
        outcome.map_err(CloseError::Failed)
    }
}

/// Why [`SyncFd::close`] did not complete cleanly.
#[derive(Debug)]
pub enum CloseError {
    /// The native export's submission has not completed within the timeout;
    /// the fd is returned unchanged.
    Pending(SyncFd),
    /// The export was released (bridged) or abandoned (device lost), and
    /// this error occurred.
    Failed(VulkanError),
}

impl std::fmt::Display for CloseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending(_) => f.write_str("sync fd submission still pending"),
            Self::Failed(e) => write!(f, "sync fd close failed: {e}"),
        }
    }
}
impl std::error::Error for CloseError {}

impl Drop for SyncFd {
    fn drop(&mut self) {
        if self.live {
            count_leak();
            // The export stays counted, so the backend refuses to destroy
            // the semaphores a submission or bridge thread may still use;
            // a bridge thread is told to stop and detached.
            if let Reclaim::Bridged { cancel, .. } = &self.reclaim {
                cancel.store(true, Ordering::Release);
            }
        }
    }
}
