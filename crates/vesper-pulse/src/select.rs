//! Runtime driver selection with a typed report of what ran and why.
//!
//! [`open`] tries the host's drivers in preference order (Linux: io_uring,
//! then epoll; macOS: kqueue; Windows: IOCP) and returns the first that
//! starts, together with a [`DriverReport`] recording every driver that was
//! skipped and the OS error that ruled it out. `VESPER_PULSE_DRIVER=<name>`
//! ([`Preference::from_env`]) forces one driver; forcing a driver that is not
//! built for this OS, or that fails to start, is a typed [`SelectError`],
//! never a silent fallback. The deterministic SimPulse path does not go
//! through this module.
use crate::driver::{
    Completion, CompletionDriver, DriverKind, PollError, Rejected, Request, RequestId,
};
use std::{fmt, io};

/// Environment variable read by [`Preference::from_env`].
pub const DRIVER_ENV: &str = "VESPER_PULSE_DRIVER";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preference {
    /// The host's preference order, falling back on start-up failure.
    Auto,
    /// Exactly this driver, or an error.
    Only(DriverKind),
}

impl Preference {
    /// `Auto` when the variable is unset or empty (or `auto`); otherwise the
    /// named driver. An unknown name is a typed error.
    pub fn from_env() -> Result<Self, SelectError> {
        match std::env::var(DRIVER_ENV) {
            Ok(v) => Self::parse(&v),
            Err(std::env::VarError::NotPresent) => Ok(Preference::Auto),
            Err(std::env::VarError::NotUnicode(v)) => {
                Err(SelectError::UnknownDriver(v.to_string_lossy().into_owned()))
            }
        }
    }
    pub fn parse(value: &str) -> Result<Self, SelectError> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("auto") {
            return Ok(Preference::Auto);
        }
        DriverKind::from_name(value)
            .map(Preference::Only)
            .ok_or_else(|| SelectError::UnknownDriver(value.to_owned()))
    }
}

/// The host's drivers, most preferred first.
pub fn host_order() -> &'static [DriverKind] {
    if cfg!(target_os = "linux") {
        &[DriverKind::IoUring, DriverKind::Epoll]
    } else if cfg!(target_os = "macos") {
        &[DriverKind::Kqueue]
    } else if cfg!(windows) {
        &[DriverKind::Iocp]
    } else {
        &[]
    }
}

/// A driver that was tried and could not start.
#[derive(Debug)]
pub struct Skipped {
    pub kind: DriverKind,
    pub error: io::Error,
}

/// Which driver runs, and the drivers ruled out before it.
#[derive(Debug)]
pub struct DriverReport {
    pub selected: DriverKind,
    pub preference: Preference,
    pub skipped: Vec<Skipped>,
}

impl fmt::Display for DriverReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pulse driver: {}", self.selected)?;
        if let Preference::Only(_) = self.preference {
            write!(f, " (forced)")?;
        }
        for s in &self.skipped {
            write!(f, "; {} unavailable: {}", s.kind, s.error)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum SelectError {
    /// The requested name is not a driver.
    UnknownDriver(String),
    /// The driver exists but is not built for this operating system.
    NotCompiled(DriverKind),
    /// The forced driver failed to start.
    Unavailable(Skipped),
    /// No host driver started (or this OS has none).
    NoDriver(Vec<Skipped>),
    /// The queue size is invalid (zero).
    InvalidEntries,
}

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectError::UnknownDriver(n) => write!(
                f,
                "unknown pulse driver {n:?} (expected auto, io_uring, epoll, kqueue or iocp)"
            ),
            SelectError::NotCompiled(k) => {
                write!(f, "pulse driver {k} is not available on this OS")
            }
            SelectError::Unavailable(s) => {
                write!(f, "pulse driver {} failed to start: {}", s.kind, s.error)
            }
            SelectError::NoDriver(skipped) => {
                write!(f, "no pulse driver could start")?;
                for s in skipped {
                    write!(f, "; {}: {}", s.kind, s.error)?;
                }
                Ok(())
            }
            SelectError::InvalidEntries => write!(f, "I/O queue must have entries"),
        }
    }
}

impl std::error::Error for SelectError {}

/// Any host driver, chosen at run time.
pub enum AnyDriver<T> {
    #[cfg(target_os = "linux")]
    IoUring(crate::uring::IoDriver<T>),
    #[cfg(target_os = "linux")]
    Epoll(crate::epoll::EpollDriver<T>),
    #[cfg(target_os = "macos")]
    Kqueue(crate::kqueue::KqueueDriver<T>),
    #[cfg(windows)]
    Iocp(crate::iocp::IocpDriver<T>),
    #[doc(hidden)]
    _Never(std::convert::Infallible, std::marker::PhantomData<T>),
}

macro_rules! each {
    ($self:expr, $d:ident => $body:expr) => {
        match $self {
            #[cfg(target_os = "linux")]
            AnyDriver::IoUring($d) => $body,
            #[cfg(target_os = "linux")]
            AnyDriver::Epoll($d) => $body,
            #[cfg(target_os = "macos")]
            AnyDriver::Kqueue($d) => $body,
            #[cfg(windows)]
            AnyDriver::Iocp($d) => $body,
            AnyDriver::_Never(never, _) => match *never {},
        }
    };
}

impl<T> AnyDriver<T> {
    /// Start exactly this driver.
    pub fn start(kind: DriverKind, entries: u32) -> Result<Self, SelectError> {
        if entries == 0 {
            return Err(SelectError::InvalidEntries);
        }
        let started: io::Result<Self> = match kind {
            #[cfg(target_os = "linux")]
            DriverKind::IoUring => crate::uring::IoDriver::new(entries).map(AnyDriver::IoUring),
            #[cfg(target_os = "linux")]
            DriverKind::Epoll => crate::epoll::EpollDriver::new(entries).map(AnyDriver::Epoll),
            #[cfg(target_os = "macos")]
            DriverKind::Kqueue => crate::kqueue::KqueueDriver::new(entries).map(AnyDriver::Kqueue),
            #[cfg(windows)]
            DriverKind::Iocp => crate::iocp::IocpDriver::new(entries).map(AnyDriver::Iocp),
            #[allow(unreachable_patterns)]
            other => return Err(SelectError::NotCompiled(other)),
        };
        started.map_err(|error| SelectError::Unavailable(Skipped { kind, error }))
    }
}

/// Start a driver according to `preference`.
pub fn open<T>(
    entries: u32,
    preference: Preference,
) -> Result<(AnyDriver<T>, DriverReport), SelectError> {
    let order: Vec<DriverKind> = match preference {
        Preference::Only(kind) => {
            let driver = AnyDriver::start(kind, entries)?;
            return Ok((
                driver,
                DriverReport {
                    selected: kind,
                    preference,
                    skipped: vec![],
                },
            ));
        }
        Preference::Auto => host_order().to_vec(),
    };
    let mut skipped = vec![];
    for kind in order {
        match AnyDriver::start(kind, entries) {
            Ok(driver) => {
                return Ok((
                    driver,
                    DriverReport {
                        selected: kind,
                        preference,
                        skipped,
                    },
                ));
            }
            Err(SelectError::Unavailable(s)) => skipped.push(s),
            Err(e) => return Err(e),
        }
    }
    Err(SelectError::NoDriver(skipped))
}

/// [`open`] with the preference read from `VESPER_PULSE_DRIVER`.
pub fn open_from_env<T>(entries: u32) -> Result<(AnyDriver<T>, DriverReport), SelectError> {
    open(entries, Preference::from_env()?)
}

impl<T> CompletionDriver<T> for AnyDriver<T> {
    fn kind(&self) -> DriverKind {
        each!(self, d => d.kind())
    }
    fn pending(&self) -> usize {
        each!(self, d => CompletionDriver::pending(d))
    }
    fn submit(&mut self, request: Request<T>) -> Result<RequestId, Rejected<T>> {
        each!(self, d => CompletionDriver::submit(d, request))
    }
    fn cancel(&mut self, id: RequestId) -> io::Result<bool> {
        each!(self, d => CompletionDriver::cancel(d, id))
    }
    fn cancel_all(&mut self) -> io::Result<()> {
        each!(self, d => CompletionDriver::cancel_all(d))
    }
    fn poll(&mut self, wait: bool) -> Result<Vec<Completion<T>>, PollError<T>> {
        each!(self, d => CompletionDriver::poll(d, wait))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_parse_to_typed_values() {
        assert_eq!(Preference::parse("").unwrap(), Preference::Auto);
        assert_eq!(Preference::parse("AUTO").unwrap(), Preference::Auto);
        assert_eq!(
            Preference::parse("epoll").unwrap(),
            Preference::Only(DriverKind::Epoll)
        );
        assert!(matches!(
            Preference::parse("poll(2)"),
            Err(SelectError::UnknownDriver(n)) if n == "poll(2)"
        ));
    }
    #[test]
    fn foreign_and_zero_sized_drivers_are_typed_errors() {
        for kind in DriverKind::ALL.into_iter().filter(|k| !k.compiled()) {
            assert!(matches!(
                AnyDriver::<()>::start(kind, 8),
                Err(SelectError::NotCompiled(k)) if k == kind
            ));
        }
        assert!(matches!(
            open::<()>(0, Preference::Auto),
            Err(SelectError::InvalidEntries)
        ));
    }
    #[test]
    fn auto_selects_a_host_driver_and_reports_skips() {
        let (driver, report) = open::<()>(8, Preference::Auto).unwrap();
        assert_eq!(driver.kind(), report.selected);
        assert!(host_order().contains(&report.selected));
        let expected: Vec<_> = host_order()
            .iter()
            .copied()
            .take_while(|k| *k != report.selected)
            .collect();
        let skipped: Vec<_> = report.skipped.iter().map(|s| s.kind).collect();
        assert_eq!(skipped, expected);
        eprintln!("{report}");
    }
}
