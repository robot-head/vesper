use std::{
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    thread::{self, ThreadId},
};

/// A local-thread capability. Portable Rust cannot establish that the caller is
/// the OS GUI main thread. An actual toolkit adapter must acquire this on its
/// required UI thread. The token and owning Session are !Send and !Sync.
///
/// ```compile_fail
/// use vesper_aura::UiThread;
/// let token = UiThread::claim_current();
/// std::thread::spawn(move || drop(token));
/// ```
/// ```compile_fail
/// use vesper_aura::UiThread;
/// fn require_sync<T: Sync>() {}
/// require_sync::<UiThread>();
/// ```
#[derive(Debug)]
pub struct UiThread {
    thread: ThreadId,
    _local: PhantomData<Rc<()>>,
}
impl UiThread {
    pub fn claim_current() -> Self {
        Self {
            thread: thread::current().id(),
            _local: PhantomData,
        }
    }
    pub(crate) fn id(&self) -> ThreadId {
        self.thread
    }
    pub(crate) fn check(&self, expected: ThreadId) -> Result<(), WrongThread> {
        if self.thread == expected && thread::current().id() == expected {
            Ok(())
        } else {
            Err(WrongThread)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrongThread;

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name {
            pub(crate) session: u64,
            pub(crate) serial: u64,
        }
    };
}
opaque_id!(WidgetId);
opaque_id!(SlotId);
opaque_id!(SubscriptionId);

pub(crate) fn new_session() -> Option<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        .ok()
}

#[derive(Debug)]
pub(crate) struct Ids {
    pub session: u64,
    pub serial: u64,
}
impl Ids {
    fn next(&mut self) -> Option<u64> {
        let n = self.serial;
        self.serial = n.checked_add(1)?;
        Some(n)
    }
    pub fn widget(&mut self) -> Option<WidgetId> {
        Some(WidgetId {
            session: self.session,
            serial: self.next()?,
        })
    }
    pub fn slot(&mut self) -> Option<SlotId> {
        Some(SlotId {
            session: self.session,
            serial: self.next()?,
        })
    }
    pub fn subscription(&mut self) -> Option<SubscriptionId> {
        Some(SubscriptionId {
            session: self.session,
            serial: self.next()?,
        })
    }
}
