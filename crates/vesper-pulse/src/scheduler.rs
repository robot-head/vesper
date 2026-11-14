//! A one-shot completion scheduler. Every queue transition moves its owned
//! continuation, file and buffer. Driver failures retain all pending owners.
//! Native continuation code generation supplies implementations of `Resume`.
//!
//! The scheduler depends only on the backend-neutral [`Driver`] protocol.
//! Every [`CompletionDriver`] (io_uring, epoll, kqueue, IOCP, or the runtime
//! choice [`AnyDriver`](crate::select::AnyDriver)) implements it.
use crate::driver::{Completion, CompletionDriver};
pub use crate::driver::{Operation, Request};
use std::{collections::VecDeque, fs::File, io};

pub struct IoEvent {
    pub file: File,
    pub buffer: Vec<u8>,
    pub result: io::Result<usize>,
}
pub enum Event {
    Start,
    Yield,
    Io(IoEvent),
    /// Preserve the interrupted event's owned data for cleanup.
    Cancelled(Box<Event>),
}
pub enum Step<C> {
    Complete(Vec<u8>),
    Failed(String),
    Yield(C),
    Io(Request<C>),
}
/// Taking self prevents the scheduler from invoking a continuation twice.
pub trait Resume: Sized {
    fn resume(self, event: Event) -> Step<Self>;
}
pub struct Finished<C> {
    pub request: Request<C>,
    pub result: io::Result<usize>,
}
pub struct PollError<C> {
    pub error: io::Error,
    pub completed: Vec<Finished<C>>,
}
pub trait Driver<C> {
    fn submit(&mut self, request: Request<C>) -> Result<(), (io::Error, Request<C>)>;
    fn poll(&mut self, wait: bool) -> Result<Vec<Finished<C>>, PollError<C>>;
    fn pending(&self) -> usize;
    fn cancel_all(&mut self) -> io::Result<()>;
}
fn finished<C>(events: Vec<Completion<C>>) -> Vec<Finished<C>> {
    events
        .into_iter()
        .map(|e| Finished {
            request: e.request,
            result: e.result,
        })
        .collect()
}
/// Adapt a [`CompletionDriver`] to the scheduler's [`Driver`] protocol.
macro_rules! completion_driver {
    ($($(#[$cfg:meta])* $ty:ty),* $(,)?) => {$(
        $(#[$cfg])*
        impl<C> Driver<C> for $ty {
            fn submit(&mut self, request: Request<C>) -> Result<(), (io::Error, Request<C>)> {
                CompletionDriver::submit(self, request).map(|_| ())
            }
            fn poll(&mut self, wait: bool) -> Result<Vec<Finished<C>>, PollError<C>> {
                CompletionDriver::poll(self, wait).map(finished).map_err(|e| PollError {
                    error: e.error,
                    completed: finished(e.completed),
                })
            }
            fn pending(&self) -> usize {
                CompletionDriver::pending(self)
            }
            fn cancel_all(&mut self) -> io::Result<()> {
                CompletionDriver::cancel_all(self)
            }
        }
    )*};
}
completion_driver!(
    #[cfg(target_os = "linux")]
    crate::uring::IoDriver<C>,
    #[cfg(target_os = "linux")]
    crate::epoll::EpollDriver<C>,
    #[cfg(target_os = "macos")]
    crate::kqueue::KqueueDriver<C>,
    #[cfg(windows)]
    crate::iocp::IocpDriver<C>,
    crate::select::AnyDriver<C>,
);
/// The driver remains accessible after errors, so callers can retry polling or
/// perform its explicit cancellation/shutdown protocol without losing fibers.
pub struct Scheduler<C, D> {
    driver: D,
    ready: VecDeque<(C, Event)>,
    completed: VecDeque<Vec<u8>>,
    failures: VecDeque<String>,
}
pub struct SchedulerParts<C, D> {
    pub driver: D,
    pub ready: VecDeque<(C, Event)>,
    pub completed: VecDeque<Vec<u8>>,
    pub failures: VecDeque<String>,
}
impl<C: Resume, D: Driver<C>> Scheduler<C, D> {
    pub fn new(driver: D) -> Self {
        Self {
            driver,
            ready: VecDeque::new(),
            completed: VecDeque::new(),
            failures: VecDeque::new(),
        }
    }
    pub fn spawn(&mut self, continuation: C) {
        self.ready.push_back((continuation, Event::Start));
    }
    pub fn driver(&mut self) -> &mut D {
        &mut self.driver
    }
    pub fn is_idle(&self) -> bool {
        self.ready.is_empty() && self.driver.pending() == 0
    }
    pub fn take_completed(&mut self) -> Option<Vec<u8>> {
        self.completed.pop_front()
    }
    pub fn take_failure(&mut self) -> Option<String> {
        self.failures.pop_front()
    }
    pub fn into_parts(self) -> SchedulerParts<C, D> {
        SchedulerParts {
            driver: self.driver,
            ready: self.ready,
            completed: self.completed,
            failures: self.failures,
        }
    }
    pub fn cancel_all(&mut self) -> io::Result<()> {
        for (_, event) in &mut self.ready {
            if !matches!(event, Event::Cancelled(_)) {
                let owned = std::mem::replace(event, Event::Yield);
                *event = Event::Cancelled(Box::new(owned));
            }
        }
        self.driver.cancel_all()
    }
    fn enqueue(&mut self, completed: Vec<Finished<C>>) {
        for event in completed {
            let request = event.request;
            self.ready.push_back((
                request.continuation,
                Event::Io(IoEvent {
                    file: request.file,
                    buffer: request.buffer,
                    result: event.result,
                }),
            ));
        }
    }
    /// Resume at most one computation. A runnable queue never blocks waiting
    /// for I/O. A yielded task goes to the back, allowing sibling progress.
    pub fn tick(&mut self, wait: bool) -> io::Result<bool> {
        let poll = self.driver.poll(wait && self.ready.is_empty());
        match poll {
            Ok(events) => self.enqueue(events),
            Err(e) => {
                self.enqueue(e.completed);
                return Err(e.error);
            }
        }
        let Some((continuation, event)) = self.ready.pop_front() else {
            return Ok(false);
        };
        match continuation.resume(event) {
            Step::Complete(value) => self.completed.push_back(value),
            Step::Failed(error) => self.failures.push_back(error),
            Step::Yield(next) => self.ready.push_back((next, Event::Yield)),
            Step::Io(request) => {
                if let Err((error, request)) = self.driver.submit(request) {
                    self.enqueue(vec![Finished {
                        request,
                        result: Err(error),
                    }]);
                }
            }
        }
        Ok(true)
    }
    /// Resume CQEs returned by explicit driver shutdown, including ECANCELED.
    pub fn accept_completions(&mut self, completed: Vec<Completion<C>>) {
        self.enqueue(finished(completed));
    }
}

/// A scope owns its children until an explicit join. An error returns the
/// entire still-owned scope so cancellation and completion can be retried.
pub struct Nursery<C, D> {
    scheduler: Scheduler<C, D>,
}
pub struct JoinError<C, D> {
    pub error: io::Error,
    pub nursery: Nursery<C, D>,
}
impl<C, D> std::fmt::Debug for JoinError<C, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl<C: Resume, D: Driver<C>> Nursery<C, D> {
    pub fn new(driver: D) -> Self {
        Self {
            scheduler: Scheduler::new(driver),
        }
    }
    pub fn spawn(&mut self, child: C) {
        self.scheduler.spawn(child);
    }
    pub fn cancel(&mut self) -> io::Result<()> {
        self.scheduler.cancel_all()
    }
    pub fn scheduler(&mut self) -> &mut Scheduler<C, D> {
        &mut self.scheduler
    }
    pub fn join(mut self) -> Result<Vec<Vec<u8>>, JoinError<C, D>> {
        loop {
            if let Some(error) = self.scheduler.take_failure() {
                return Err(JoinError {
                    error: io::Error::other(error),
                    nursery: self,
                });
            }
            if self.scheduler.is_idle() {
                break;
            }
            if let Err(error) = self.scheduler.tick(true) {
                return Err(JoinError {
                    error,
                    nursery: self,
                });
            }
        }
        Ok(self.scheduler.completed.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    fn null_file() -> File {
        File::open(if cfg!(windows) { "NUL" } else { "/dev/null" }).unwrap()
    }
    struct Counter {
        resumed: Arc<AtomicUsize>,
        stage: usize,
    }
    impl Resume for Counter {
        fn resume(mut self, event: Event) -> Step<Self> {
            self.resumed.fetch_add(1, Ordering::SeqCst);
            if let Event::Cancelled(interrupted) = event {
                if let Event::Io(event) = *interrupted {
                    assert_eq!(event.buffer, vec![42]);
                }
                return Step::Complete(vec![7]);
            }
            match (self.stage, event) {
                (0, Event::Start) => {
                    self.stage = 1;
                    Step::Yield(self)
                }
                (1, Event::Yield) => {
                    self.stage = 2;
                    Step::Io(Request {
                        file: null_file(),
                        buffer: vec![42],
                        continuation: self,
                        operation: Operation::Read,
                        offset: 0,
                    })
                }
                (2, Event::Io(event)) => {
                    let code = if event.result.is_ok() {
                        event.buffer[0]
                    } else {
                        7
                    };
                    Step::Complete(vec![code])
                }
                _ => panic!("a continuation received the wrong event or ran twice"),
            }
        }
    }
    #[derive(Default)]
    struct MockDriver {
        requests: Vec<Request<Counter>>,
        fail_submit: bool,
        fail_poll: bool,
        cancelled: bool,
    }
    impl Driver<Counter> for MockDriver {
        fn submit(
            &mut self,
            request: Request<Counter>,
        ) -> Result<(), (io::Error, Request<Counter>)> {
            if self.fail_submit {
                return Err((io::Error::other("submit failed"), request));
            }
            self.requests.push(request);
            Ok(())
        }
        fn poll(&mut self, _: bool) -> Result<Vec<Finished<Counter>>, PollError<Counter>> {
            let completed = self
                .requests
                .drain(..)
                .map(|request| Finished {
                    request,
                    result: if self.cancelled {
                        Err(io::Error::from_raw_os_error(125))
                    } else {
                        Ok(1)
                    },
                })
                .collect();
            if self.fail_poll {
                self.fail_poll = false;
                Err(PollError {
                    error: io::Error::other("poll failed after harvest"),
                    completed,
                })
            } else {
                Ok(completed)
            }
        }
        fn pending(&self) -> usize {
            self.requests.len()
        }
        fn cancel_all(&mut self) -> io::Result<()> {
            self.cancelled = true;
            Ok(())
        }
    }
    #[test]
    fn each_completion_resumes_its_one_owned_continuation() {
        let resumed = Arc::new(AtomicUsize::new(0));
        let mut scheduler = Scheduler::new(MockDriver::default());
        for _ in 0..2 {
            scheduler.spawn(Counter {
                resumed: resumed.clone(),
                stage: 0,
            });
        }
        while !scheduler.is_idle() {
            scheduler.tick(false).unwrap();
        }
        assert_eq!(scheduler.take_completed(), Some(vec![42]));
        assert_eq!(scheduler.take_completed(), Some(vec![42]));
        assert_eq!(scheduler.take_completed(), None);
        assert_eq!(resumed.load(Ordering::SeqCst), 6);
    }
    #[test]
    fn failed_submission_returns_owned_io_to_the_continuation() {
        let resumed = Arc::new(AtomicUsize::new(0));
        let mut scheduler = Scheduler::new(MockDriver {
            fail_submit: true,
            ..Default::default()
        });
        scheduler.spawn(Counter {
            resumed: resumed.clone(),
            stage: 0,
        });
        while !scheduler.is_idle() {
            scheduler.tick(false).unwrap();
        }
        assert_eq!(scheduler.take_completed(), Some(vec![7]));
        assert_eq!(resumed.load(Ordering::SeqCst), 3);
    }
    #[test]
    fn harvested_completions_survive_a_poll_failure() {
        let resumed = Arc::new(AtomicUsize::new(0));
        let mut scheduler = Scheduler::new(MockDriver::default());
        scheduler.spawn(Counter {
            resumed: resumed.clone(),
            stage: 0,
        });
        scheduler.tick(false).unwrap();
        scheduler.tick(false).unwrap();
        scheduler.driver().fail_poll = true;
        assert!(scheduler.tick(false).is_err());
        assert!(!scheduler.is_idle());
        scheduler.tick(false).unwrap();
        assert_eq!(scheduler.take_completed(), Some(vec![42]));
        assert_eq!(resumed.load(Ordering::SeqCst), 3);
    }
    #[test]
    fn nursery_joins_every_child_and_can_retry_after_driver_failure() {
        let resumed = Arc::new(AtomicUsize::new(0));
        let mut nursery = Nursery::new(MockDriver::default());
        for _ in 0..2 {
            nursery.spawn(Counter {
                resumed: resumed.clone(),
                stage: 0,
            });
        }
        for _ in 0..3 {
            nursery.scheduler().tick(false).unwrap();
        }
        nursery.scheduler().driver().fail_poll = true;
        let error = nursery.join().unwrap_err();
        assert_eq!(resumed.load(Ordering::SeqCst), 3);
        assert_eq!(error.nursery.join().unwrap(), vec![vec![42], vec![42]]);
        assert_eq!(resumed.load(Ordering::SeqCst), 6);
    }
    #[test]
    fn nursery_cancellation_drains_owned_io_before_joining() {
        let resumed = Arc::new(AtomicUsize::new(0));
        let mut nursery = Nursery::new(MockDriver::default());
        nursery.spawn(Counter {
            resumed: resumed.clone(),
            stage: 0,
        });
        nursery.scheduler().tick(false).unwrap();
        nursery.scheduler().tick(false).unwrap();
        assert_eq!(nursery.scheduler().driver().pending(), 1);
        nursery.cancel().unwrap();
        assert_eq!(nursery.join().unwrap(), vec![vec![7]]);
        assert_eq!(resumed.load(Ordering::SeqCst), 3);
    }
    #[test]
    fn nursery_cancellation_resumes_queued_children_once() {
        let resumed = Arc::new(AtomicUsize::new(0));
        let mut nursery = Nursery::new(MockDriver::default());
        for _ in 0..2 {
            nursery.spawn(Counter {
                resumed: resumed.clone(),
                stage: 0,
            });
        }
        nursery.cancel().unwrap();
        nursery.cancel().unwrap();
        assert_eq!(nursery.join().unwrap(), vec![vec![7], vec![7]]);
        assert_eq!(resumed.load(Ordering::SeqCst), 2);
    }
}
