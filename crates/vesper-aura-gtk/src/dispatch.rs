//! A bounded, `Send` dispatcher from any thread to the UI thread.
//!
//! [`channel`] returns a cloneable [`Dispatcher`] (`Send + Sync`) and the one
//! [`Inbox`] (`!Send`, it stays on the UI thread). [`Dispatcher::send`] never
//! blocks: it enqueues the message or hands it back with a typed reason
//! ([`DispatchError`]), and wakes the inbox's registered [`Waker`]. With the
//! `gtk` feature, `UiLoop` (see the crate root) spawns the inbox on the GLib
//! main context, so that waker is GLib's own task waker and a send from a
//! Pulse worker or any other thread schedules the UI-thread handler.
//!
//! This module is plain `std` and is tested without GTK.

use std::{
    collections::VecDeque,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard},
    task::{Context, Poll, Waker},
};

/// Why a message was not enqueued. The message is always handed back.
#[derive(Debug, PartialEq, Eq)]
pub enum DispatchError<T> {
    /// The inbox already holds `capacity` undelivered messages.
    Full(T),
    /// The inbox was closed or dropped; nothing will receive the message.
    Closed(T),
}

impl<T> DispatchError<T> {
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(t) | Self::Closed(t) => t,
        }
    }
}

/// Counters a host can report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchStats {
    pub sent: u64,
    pub rejected_full: u64,
    pub rejected_closed: u64,
    pub wakes: u64,
}

struct State<T> {
    queue: VecDeque<T>,
    capacity: usize,
    waker: Option<Waker>,
    closed: bool,
    stats: DispatchStats,
}

struct Shared<T> {
    state: Mutex<State<T>>,
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        // A panic while holding the lock cannot leave the queue half-updated
        // (every critical section is a single push/pop), so recover.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The sending half. Cloneable, `Send` and `Sync` when `T: Send`.
pub struct Dispatcher<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for Dispatcher<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> std::fmt::Debug for Dispatcher<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher").finish_non_exhaustive()
    }
}

/// The receiving half. It is `!Send`, so it stays on the thread that created
/// the channel (the UI thread).
///
/// ```compile_fail
/// fn send<T: Send>() {}
/// send::<vesper_aura_gtk::dispatch::Inbox<u32>>();
/// ```
pub struct Inbox<T> {
    shared: Arc<Shared<T>>,
    _local: PhantomData<Rc<()>>,
}

impl<T> std::fmt::Debug for Inbox<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inbox").finish_non_exhaustive()
    }
}

/// A bounded channel. `capacity` must be at least 1.
pub fn channel<T>(capacity: usize) -> Result<(Dispatcher<T>, Inbox<T>), DispatchError<()>> {
    if capacity == 0 {
        return Err(DispatchError::Full(()));
    }
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queue: VecDeque::new(),
            capacity,
            waker: None,
            closed: false,
            stats: DispatchStats::default(),
        }),
    });
    Ok((
        Dispatcher {
            shared: Arc::clone(&shared),
        },
        Inbox {
            shared,
            _local: PhantomData,
        },
    ))
}

impl<T> Dispatcher<T> {
    /// Enqueue without blocking and wake the inbox.
    pub fn send(&self, message: T) -> Result<(), DispatchError<T>> {
        let waker = {
            let mut state = self.shared.lock();
            if state.closed {
                state.stats.rejected_closed += 1;
                return Err(DispatchError::Closed(message));
            }
            if state.queue.len() >= state.capacity {
                state.stats.rejected_full += 1;
                return Err(DispatchError::Full(message));
            }
            state.queue.push_back(message);
            state.stats.sent += 1;
            let waker = state.waker.take();
            if waker.is_some() {
                state.stats.wakes += 1;
            }
            waker
        };
        // Wake outside the lock: GLib's waker may run arbitrary scheduling.
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    /// Whether the inbox is still open.
    pub fn is_open(&self) -> bool {
        !self.shared.lock().closed
    }
}

impl<T> Inbox<T> {
    /// Take one message if any is queued.
    pub fn try_recv(&self) -> Option<T> {
        self.shared.lock().queue.pop_front()
    }

    /// Take up to `max` queued messages, or register `cx`'s waker and return
    /// `Pending` when none is queued. `Ready(empty)` means closed and drained.
    pub fn poll_batch(&self, cx: &mut Context<'_>, max: usize) -> Poll<Vec<T>> {
        let mut state = self.shared.lock();
        if state.queue.is_empty() {
            if state.closed {
                return Poll::Ready(Vec::new());
            }
            state.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let n = state.queue.len().min(max.max(1));
        Poll::Ready(state.queue.drain(..n).collect())
    }

    /// Number of undelivered messages.
    pub fn len(&self) -> usize {
        self.shared.lock().queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> DispatchStats {
        self.shared.lock().stats
    }

    /// Refuse further sends and return the undelivered messages so the UI
    /// thread can account for them; they are never silently dropped.
    pub fn close(&self) -> Vec<T> {
        let (waker, undelivered) = {
            let mut state = self.shared.lock();
            state.closed = true;
            let undelivered: Vec<T> = state.queue.drain(..).collect();
            (state.waker.take(), undelivered)
        };
        // Wake a pending receiver (outside the lock) so it observes
        // `Ready(empty)`, the closed-and-drained signal.
        if let Some(waker) = waker {
            waker.wake();
        }
        undelivered
    }
}

impl<T> Drop for Inbox<T> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.closed = true;
        state.waker = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        task::Wake,
    };

    struct Count(AtomicUsize);
    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn sends_from_other_threads_wake_the_registered_waker() {
        let (tx, rx) = channel::<u32>(1024).unwrap();
        let count = Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&count));
        let mut cx = Context::from_waker(&waker);
        assert!(rx.poll_batch(&mut cx, 8).is_pending());
        let workers: Vec<_> = (0..4)
            .map(|t| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        tx.send(t * 1000 + i).unwrap();
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        // The first send consumed the registered waker.
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        let mut got = Vec::new();
        while let Poll::Ready(batch) = rx.poll_batch(&mut cx, 64) {
            assert!(!batch.is_empty() && batch.len() <= 64);
            got.extend(batch);
        }
        assert_eq!(got.len(), 400);
        // Per-sender order is preserved.
        for t in 0..4 {
            let mine: Vec<u32> = got.iter().copied().filter(|v| v / 1000 == t).collect();
            assert_eq!(mine, (0..100).map(|i| t * 1000 + i).collect::<Vec<_>>());
        }
        tx.send(7).unwrap();
        assert_eq!(count.0.load(Ordering::SeqCst), 2);
        assert_eq!(rx.stats().sent, 401);
        // Closing wakes a pending receiver, which then sees closed-and-empty.
        assert_eq!(rx.poll_batch(&mut cx, 8), Poll::Ready(vec![7]));
        assert!(rx.poll_batch(&mut cx, 8).is_pending());
        assert!(rx.close().is_empty());
        assert_eq!(count.0.load(Ordering::SeqCst), 3);
        assert_eq!(rx.poll_batch(&mut cx, 8), Poll::Ready(vec![]));
    }

    #[test]
    fn full_and_closed_return_the_message() {
        assert!(channel::<u8>(0).is_err());
        let (tx, rx) = channel::<String>(2).unwrap();
        tx.send("a".into()).unwrap();
        tx.send("b".into()).unwrap();
        assert_eq!(tx.send("c".into()), Err(DispatchError::Full("c".into())));
        assert_eq!(rx.close(), vec!["a".to_string(), "b".to_string()]);
        assert!(!tx.is_open());
        assert_eq!(tx.send("d".into()), Err(DispatchError::Closed("d".into())));
        let stats = rx.stats();
        assert_eq!((stats.rejected_full, stats.rejected_closed), (1, 1));
        let (tx, rx) = channel::<u8>(1).unwrap();
        drop(rx);
        assert_eq!(tx.send(1), Err(DispatchError::Closed(1)));
    }
}
