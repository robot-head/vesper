//! GLib main loop integration: a waker-driven inbox for the [`Dispatcher`]
//! and two ways to run a Vesper Pulse scheduler beside GTK (§18.4).
//!
//! * [`spawn_inbox`] runs a UI-thread handler for a [`dispatch`] channel as a
//!   local task on GLib's default main context. Its waker is GLib's task
//!   waker, so [`Dispatcher::send`] from any thread schedules the handler on
//!   the GTK main thread; nothing polls.
//! * [`co_schedule`] runs a `vesper_pulse::scheduler::Scheduler` *on* the GTK
//!   main thread as a local GLib task: each dispatch resumes at most `slice`
//!   continuations, then yields back to GLib so input and redraws interleave
//!   with Pulse work. While only I/O is in flight it re-polls the driver
//!   with a bounded exponential backoff, because Pulse's drivers do not yet
//!   expose a pollable descriptor that could be added to the `GMainContext`.
//! * [`spawn_pulse_worker`] runs a scheduler on its own thread with blocking
//!   waits and posts every result through a [`Dispatcher`]. That worker has
//!   no `UiThread` token, so it can only reach the UI by message, the
//!   runtime counterpart of §18.4's "background shards lack UiThread".
//!
//! [`dispatch`]: crate::dispatch

use crate::{
    GtkError,
    dispatch::{DispatchError, Dispatcher, Inbox},
};
use gtk4::glib;
use std::{
    cell::{Cell, RefCell},
    future::Future,
    io,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
    thread,
    time::{Duration, Instant},
};
use vesper_aura::UiThread;
use vesper_pulse::scheduler::{Driver, Resume, Scheduler};

fn on_gtk_thread(_thread: &UiThread) -> Result<glib::MainContext, GtkError> {
    if !gtk4::is_initialized_main_thread() {
        return Err(GtkError::WrongThread);
    }
    Ok(glib::MainContext::default())
}

/// A running inbox handler. Dropping it leaves the task running; call
/// [`InboxTask::close`] to stop it and collect undelivered messages.
#[must_use = "close the inbox task to account for undelivered messages"]
pub struct InboxTask<T> {
    inbox: Rc<Inbox<T>>,
    handle: glib::JoinHandle<()>,
    delivered: Rc<Cell<u64>>,
}

impl<T> InboxTask<T> {
    /// Messages handed to the handler so far.
    pub fn delivered(&self) -> u64 {
        self.delivered.get()
    }
    /// Stop the handler, refuse further sends, and return what was queued.
    pub fn close(self) -> Vec<T> {
        self.handle.abort();
        self.inbox.close()
    }
}

struct InboxFuture<T> {
    inbox: Rc<Inbox<T>>,
    batch: usize,
}

impl<T> Future for InboxFuture<T> {
    type Output = Vec<T>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Vec<T>> {
        self.inbox.poll_batch(cx, self.batch)
    }
}

/// Run `handler` on the GTK main thread for every batch (at most `batch`
/// messages) sent through the inbox's dispatcher.
pub fn spawn_inbox<T: 'static>(
    thread: &UiThread,
    inbox: Inbox<T>,
    batch: usize,
    mut handler: impl FnMut(Vec<T>) + 'static,
) -> Result<InboxTask<T>, GtkError> {
    let context = on_gtk_thread(thread)?;
    let inbox = Rc::new(inbox);
    let delivered = Rc::new(Cell::new(0u64));
    let (task_inbox, task_delivered) = (Rc::clone(&inbox), Rc::clone(&delivered));
    let handle = context.spawn_local(async move {
        loop {
            let messages = InboxFuture {
                inbox: Rc::clone(&task_inbox),
                batch,
            }
            .await;
            if messages.is_empty() {
                return; // closed and drained
            }
            task_delivered.set(task_delivered.get() + messages.len() as u64);
            handler(messages);
        }
    });
    Ok(InboxTask {
        inbox,
        handle,
        delivered,
    })
}

/// One result of a Pulse continuation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PulseOutput {
    Completed(Vec<u8>),
    Failed(String),
}

/// How [`co_schedule`] shares the GTK main thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoSchedule {
    /// Continuations resumed per GLib dispatch before yielding (at least 1).
    pub slice: usize,
    /// First and largest wait between driver polls while only I/O is in
    /// flight.
    pub min_wait: Duration,
    pub max_wait: Duration,
}

impl Default for CoSchedule {
    fn default() -> Self {
        Self {
            slice: 32,
            min_wait: Duration::from_micros(250),
            max_wait: Duration::from_millis(8),
        }
    }
}

/// Counters of a co-scheduled Pulse task.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoStats {
    /// Continuations resumed.
    pub resumed: u64,
    /// GLib dispatches of the task (each runs at most `slice` resumes).
    pub dispatches: u64,
    /// Times the task yielded to GLib with runnable work left.
    pub yields: u64,
    /// Timed waits while only I/O was in flight.
    pub io_waits: u64,
    /// Largest number of resumes in one dispatch.
    pub max_slice: usize,
}

struct CoShared<C, D> {
    scheduler: RefCell<Option<Scheduler<C, D>>>,
    stats: Cell<CoStats>,
    error: RefCell<Option<io::Error>>,
    done: Cell<bool>,
}

/// A Pulse scheduler running on the GTK main thread.
#[must_use = "a co-scheduled Pulse task owns continuations; finish or cancel it"]
pub struct PulseTask<C, D> {
    shared: Rc<CoShared<C, D>>,
    handle: Option<glib::JoinHandle<()>>,
}

impl<C, D> PulseTask<C, D> {
    pub fn is_finished(&self) -> bool {
        self.shared.done.get()
    }
    pub fn stats(&self) -> CoStats {
        self.shared.stats.get()
    }
    /// The driver error that stopped the task, if any.
    pub fn take_error(&self) -> Option<io::Error> {
        self.shared.error.borrow_mut().take()
    }
    /// Stop co-scheduling and return the scheduler with every still-owned
    /// continuation (so the caller can run Pulse's own cancellation).
    pub fn stop(mut self) -> Option<Scheduler<C, D>> {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
        self.shared.scheduler.borrow_mut().take()
    }
}

/// Yield to GLib once: wake immediately, return `Pending` the first time.
struct YieldNow(bool);
impl Future for YieldNow {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Run `scheduler` on the GTK main thread until it is idle, sending every
/// completion and failure to `output` (also on the GTK main thread).
pub fn co_schedule<C, D>(
    thread: &UiThread,
    scheduler: Scheduler<C, D>,
    config: CoSchedule,
    mut output: impl FnMut(PulseOutput) + 'static,
) -> Result<PulseTask<C, D>, GtkError>
where
    C: Resume + 'static,
    D: Driver<C> + 'static,
{
    let context = on_gtk_thread(thread)?;
    if config.slice == 0 || config.min_wait.is_zero() || config.max_wait < config.min_wait {
        return Err(GtkError::Unsupported("invalid co-schedule configuration"));
    }
    let shared = Rc::new(CoShared {
        scheduler: RefCell::new(Some(scheduler)),
        stats: Cell::new(CoStats::default()),
        error: RefCell::new(None),
        done: Cell::new(false),
    });
    let task = Rc::clone(&shared);
    let handle = context.spawn_local(async move {
        let mut wait = config.min_wait;
        loop {
            // Synchronous slice: the scheduler is never borrowed across await.
            let (progressed, idle) = {
                let mut guard = task.scheduler.borrow_mut();
                let Some(s) = guard.as_mut() else { return };
                let mut stats = task.stats.get();
                stats.dispatches += 1;
                let mut resumed = 0;
                let mut failed = None;
                while resumed < config.slice {
                    match s.tick(false) {
                        Ok(true) => resumed += 1,
                        Ok(false) => break,
                        Err(e) => {
                            failed = Some(e);
                            break;
                        }
                    }
                }
                while let Some(value) = s.take_completed() {
                    output(PulseOutput::Completed(value));
                }
                while let Some(error) = s.take_failure() {
                    output(PulseOutput::Failed(error));
                }
                stats.resumed += resumed as u64;
                stats.max_slice = stats.max_slice.max(resumed);
                task.stats.set(stats);
                if let Some(e) = failed {
                    *task.error.borrow_mut() = Some(e);
                    task.done.set(true);
                    return;
                }
                (resumed > 0, s.is_idle())
            };
            if idle {
                task.done.set(true);
                return;
            }
            let mut stats = task.stats.get();
            if progressed {
                stats.yields += 1;
                task.stats.set(stats);
                wait = config.min_wait;
                YieldNow(false).await;
            } else {
                stats.io_waits += 1;
                task.stats.set(stats);
                glib::timeout_future(wait).await;
                wait = (wait * 2).min(config.max_wait);
            }
        }
    });
    Ok(PulseTask {
        shared,
        handle: Some(handle),
    })
}

/// Run a Pulse scheduler on a worker thread with blocking driver waits and
/// post each result to the UI through `dispatcher`. `make` runs on the worker
/// (drivers need not be `Send`). A full inbox is retried with a short sleep;
/// a closed inbox cancels the scheduler's in-flight I/O and stops the worker
/// with `BrokenPipe`.
pub fn spawn_pulse_worker<C, D, F>(
    make: F,
    dispatcher: Dispatcher<PulseOutput>,
) -> thread::JoinHandle<io::Result<u64>>
where
    C: Resume,
    D: Driver<C>,
    F: FnOnce() -> io::Result<Scheduler<C, D>> + Send + 'static,
{
    thread::spawn(move || {
        let mut scheduler = make()?;
        let mut posted = 0u64;
        let mut post = |scheduler: &mut Scheduler<C, D>, mut message: PulseOutput| loop {
            match dispatcher.send(message) {
                Ok(()) => {
                    posted += 1;
                    return Ok(());
                }
                Err(DispatchError::Full(m)) => {
                    message = m;
                    thread::sleep(Duration::from_micros(200));
                }
                Err(DispatchError::Closed(_)) => {
                    let _ = scheduler.cancel_all();
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
            }
        };
        while !scheduler.is_idle() {
            scheduler.tick(true)?;
            while let Some(value) = scheduler.take_completed() {
                post(&mut scheduler, PulseOutput::Completed(value))?;
            }
            while let Some(error) = scheduler.take_failure() {
                post(&mut scheduler, PulseOutput::Failed(error))?;
            }
        }
        Ok(posted)
    })
}

/// Iterate the default main context (non-blocking, with short sleeps) until
/// `done` returns true or `timeout` expires.
pub fn pump_until(
    thread: &UiThread,
    timeout: Duration,
    mut done: impl FnMut() -> bool,
) -> Result<(), GtkError> {
    let context = on_gtk_thread(thread)?;
    let deadline = Instant::now() + timeout;
    loop {
        while context.iteration(false) {}
        if done() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(GtkError::Timeout("main-loop condition"));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// Run `work` on a helper thread while the GTK main thread keeps iterating
/// GLib, and return its result. Needed for clients (such as AT-SPI queries)
/// that talk to this process's own GTK over D-Bus: a blocking call made on
/// the GTK thread would wait for an answer that thread itself must send.
pub fn run_off_thread<R: Send + 'static>(
    thread: &UiThread,
    timeout: Duration,
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, GtkError> {
    on_gtk_thread(thread)?;
    let helper = thread::spawn(work);
    pump_until(thread, timeout, || helper.is_finished())?;
    helper
        .join()
        .map_err(|_| GtkError::Desynchronized("helper thread panicked"))
}
