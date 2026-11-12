//! Bridge from the reference effect machine to owned completion scheduling.
//! This is an evaluator embedding API, not a native backend fallback.
use crate::{
    io_driver::{Operation, Request},
    runtime::{Execution, Runtime, Suspension, Value},
    scheduler::{Event, IoEvent, Resume, Step},
};
use std::fs::File;

pub enum Action<P> {
    Return(Value),
    Io {
        file: File,
        buffer: Vec<u8>,
        operation: Operation,
        offset: u64,
        context: P,
    },
}
/// Host handlers must implement the checked effect signatures. In particular,
/// errors and cancellation must produce a value and resume a linear operation.
pub trait Handler {
    type Context;
    fn request(
        &mut self,
        continuation: &mut Suspension<'_>,
        cancelled: bool,
    ) -> Action<Self::Context>;
    fn complete(
        &mut self,
        context: Self::Context,
        event: IoEvent,
        continuation: &mut Suspension<'_>,
        cancelled: bool,
    ) -> Value;
    fn output(&mut self, value: Value) -> Vec<u8> {
        value.to_string().into_bytes()
    }
}
enum State<'a, P> {
    Start {
        runtime: Runtime<'a>,
        entry: String,
        arguments: Vec<Value>,
    },
    Parked {
        continuation: Suspension<'a>,
        context: P,
    },
}
pub struct Session<'a, H: Handler> {
    state: State<'a, H::Context>,
    handler: H,
    cancelled: bool,
}
impl<'a, H: Handler> Session<'a, H> {
    pub fn new(
        runtime: Runtime<'a>,
        entry: impl Into<String>,
        arguments: Vec<Value>,
        handler: H,
    ) -> Self {
        Self {
            state: State::Start {
                runtime,
                entry: entry.into(),
                arguments,
            },
            handler,
            cancelled: false,
        }
    }
    fn advance(mut handler: H, mut execution: Execution<'a>, cancelled: bool) -> Step<Self> {
        loop {
            match execution {
                Execution::Complete(value) => return Step::Complete(handler.output(value)),
                Execution::Suspended(mut continuation) => {
                    match handler.request(&mut continuation, cancelled) {
                        Action::Return(value) => match continuation.resume(value) {
                            Ok(next) => execution = next,
                            Err(error) => return Step::Failed(error),
                        },
                        Action::Io {
                            file,
                            buffer,
                            operation,
                            offset,
                            context,
                        } => {
                            return Step::Io(Request {
                                file,
                                buffer,
                                operation,
                                offset,
                                continuation: Self {
                                    state: State::Parked {
                                        continuation,
                                        context,
                                    },
                                    handler,
                                    cancelled,
                                },
                            });
                        }
                    }
                }
            }
        }
    }
}
impl<H: Handler> Resume for Session<'_, H> {
    fn resume(mut self, mut event: Event) -> Step<Self> {
        while let Event::Cancelled(interrupted) = event {
            self.cancelled = true;
            event = *interrupted;
        }
        let execution = match (self.state, event) {
            (
                State::Start {
                    runtime,
                    entry,
                    arguments,
                },
                Event::Start,
            ) => runtime.begin_external(&entry, arguments),
            (
                State::Parked {
                    mut continuation,
                    context,
                },
                Event::Io(event),
            ) => {
                let value =
                    self.handler
                        .complete(context, event, &mut continuation, self.cancelled);
                continuation.resume(value)
            }
            _ => return Step::Failed("I/O session received an invalid scheduler event".into()),
        };
        match execution {
            Ok(execution) => Self::advance(self.handler, execution, self.cancelled),
            Err(error) => Step::Failed(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        check,
        io_driver::IoDriver,
        parser,
        scheduler::{Driver, Finished, Nursery, PollError},
    };
    const SOURCE: &str = "linear struct Ticket { id: u64 } linear effect IO { fn read() -> Result[u64, u32]; } fn main() -> u64 eff [IO] { let ticket = Ticket { id: 41 }; let count: u64 = match IO::read() { Ok(n) => n, Err(_) => 0, }; let Ticket { id } = ticket; id + count }";
    fn world() -> check::World {
        let parsed = parser::parse(SOURCE);
        assert!(parsed.diagnostics.is_empty());
        let checked = check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        checked.world
    }
    struct Count {
        file: Option<File>,
    }
    fn response(result: std::io::Result<usize>) -> Value {
        let (constructor, value) = match result {
            Ok(n) => ("Ok", Value::UInt(n as u64, 64)),
            Err(e) => ("Err", Value::UInt(e.raw_os_error().unwrap_or(1) as u64, 32)),
        };
        Value::Data {
            ty: "Result".into(),
            constructor: constructor.into(),
            fields: [("0".into(), value)].into(),
        }
    }
    impl Handler for Count {
        type Context = ();
        fn request(&mut self, pending: &mut Suspension<'_>, cancelled: bool) -> Action<()> {
            assert_eq!(pending.request().effect.name, "IO");
            assert_eq!(pending.request().operation, "read");
            if cancelled {
                return Action::Return(response(Err(std::io::Error::from_raw_os_error(125))));
            }
            Action::Io {
                file: self.file.take().unwrap(),
                buffer: vec![0; 1],
                operation: Operation::Read,
                offset: 0,
                context: (),
            }
        }
        fn complete(&mut self, _: (), event: IoEvent, _: &mut Suspension<'_>, _: bool) -> Value {
            response(event.result)
        }
    }
    struct Mock<C> {
        requests: Vec<Request<C>>,
        cancelled: bool,
        failed: bool,
    }
    impl<C> Default for Mock<C> {
        fn default() -> Self {
            Self {
                requests: vec![],
                cancelled: false,
                failed: false,
            }
        }
    }
    impl<C> Driver<C> for Mock<C> {
        fn submit(&mut self, request: Request<C>) -> Result<(), (std::io::Error, Request<C>)> {
            self.requests.push(request);
            Ok(())
        }
        fn pending(&self) -> usize {
            self.requests.len()
        }
        fn cancel_all(&mut self) -> std::io::Result<()> {
            self.cancelled = true;
            Ok(())
        }
        fn poll(&mut self, _: bool) -> Result<Vec<Finished<C>>, PollError<C>> {
            Ok(self
                .requests
                .drain(..)
                .map(|request| Finished {
                    request,
                    result: if self.cancelled || self.failed {
                        Err(std::io::Error::from_raw_os_error(125))
                    } else {
                        Ok(1)
                    },
                })
                .collect())
        }
    }
    #[test]
    fn source_linear_obligation_survives_completion_errors_and_cancellation() {
        let world = world();
        for (failed, cancelled, before_start) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, true, true),
        ] {
            let mut nursery = Nursery::new(Mock {
                failed,
                ..Default::default()
            });
            nursery.spawn(Session::new(
                Runtime::new(&world),
                "main",
                vec![],
                Count {
                    file: Some(File::open("/dev/zero").unwrap()),
                },
            ));
            if cancelled {
                if !before_start {
                    nursery.scheduler().tick(false).unwrap();
                }
                nursery.cancel().unwrap();
            }
            let outputs = nursery.join().unwrap();
            assert_eq!(
                outputs,
                vec![if failed || cancelled {
                    b"41".to_vec()
                } else {
                    b"42".to_vec()
                }]
            );
        }
    }
    #[test]
    fn kernel_completion_resumes_a_source_program() {
        let world = world();
        let driver = match IoDriver::new(8) {
            Ok(driver) => driver,
            Err(error) if matches!(error.raw_os_error(), Some(1 | 38 | 95)) => {
                eprintln!("kernel source I/O test skipped: {error}");
                return;
            }
            Err(error) => panic!("io_uring setup failed: {error}"),
        };
        let mut nursery = Nursery::new(driver);
        nursery.spawn(Session::new(
            Runtime::new(&world),
            "main",
            vec![],
            Count {
                file: Some(File::open("/dev/zero").unwrap()),
            },
        ));
        assert_eq!(nursery.join().unwrap(), vec![b"42".to_vec()]);
    }
}
