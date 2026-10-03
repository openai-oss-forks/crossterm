use std::{
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    task::{Context, Poll},
    thread,
    time::Duration,
};

use futures_core::stream::Stream;

use crate::event::{
    filter::{EventFilter, Filter},
    internal::{self, InternalEvent},
    sys::Waker,
    Event,
};

/// A stream of `Result<Event>`.
///
/// **This type is not available by default. You have to use the `event-stream` feature flag
/// to make it available.**
///
/// It implements the [Stream](futures_core::stream::Stream)
/// trait and allows you to receive [`Event`]s with [`smol`](https://crates.io/crates/smol)
/// or [`tokio`](https://crates.io/crates/tokio) crates.
///
/// Check the [examples](https://github.com/crossterm-rs/crossterm/tree/master/examples) folder to see how to use
/// it (`event-stream-*`).
#[derive(Debug)]
pub struct EventStream {
    filter: StreamFilter,
    poll_internal_waker: Waker,
    stream_wake_task_executed: Arc<AtomicBool>,
    stream_wake_task_should_shutdown: Arc<AtomicBool>,
    task_sender: SyncSender<Task>,
}

impl Default for EventStream {
    fn default() -> Self {
        Self::with_filter(StreamFilter::Input)
    }
}

impl EventStream {
    fn with_filter(filter: StreamFilter) -> Self {
        let (task_sender, receiver) = mpsc::sync_channel::<Task>(1);

        thread::spawn(move || {
            while let Ok(task) = receiver.recv() {
                loop {
                    if let Ok(true) = internal::poll(None, &filter) {
                        break;
                    }

                    if task.stream_wake_task_should_shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                }
                task.stream_wake_task_executed
                    .store(false, Ordering::SeqCst);
                task.stream_waker.wake();
            }
        });

        EventStream {
            filter,
            poll_internal_waker: internal::lock_event_reader().waker(),
            stream_wake_task_executed: Arc::new(AtomicBool::new(false)),
            stream_wake_task_should_shutdown: Arc::new(AtomicBool::new(false)),
            task_sender,
        }
    }
}

impl EventStream {
    /// Constructs a new instance of `EventStream`.
    pub fn new() -> EventStream {
        EventStream::default()
    }
}

struct Task {
    stream_waker: std::task::Waker,
    stream_wake_task_executed: Arc<AtomicBool>,
    stream_wake_task_should_shutdown: Arc<AtomicBool>,
}

// Note to future me
//
// We need two wakers in order to implement EventStream correctly.
//
// 1. futures::Stream waker
//
// Stream::poll_next can return Poll::Pending which means that there's no
// event available. We are going to spawn a thread with the
// internal::poll(None, &EventFilter) call. This call blocks until an
// event is available and then we have to wake up the executor with notification
// that the task can be resumed.
//
// 2. poll_internal waker
//
// There's no event available, Poll::Pending was returned, stream waker thread
// is up and sitting in the poll_internal. User wants to drop the EventStream.
// We have to wake up the poll_internal (force it to return Ok(false)) and quit
// the thread before we drop.
impl Stream for EventStream {
    type Item = io::Result<Event>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.poll_internal_event(cx).map(|event| {
            event.map(|event| {
                event.map(|event| match event {
                    InternalEvent::Event(event) => event,
                    _ => unreachable!(),
                })
            })
        })
    }
}

impl EventStream {
    fn poll_internal_event(&self, cx: &mut Context<'_>) -> Poll<Option<io::Result<InternalEvent>>> {
        let result = match internal::poll(Some(Duration::from_secs(0)), &self.filter) {
            Ok(true) => Poll::Ready(Some(internal::read(&self.filter))),
            Ok(false) => {
                if !self
                    .stream_wake_task_executed
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    // https://github.com/rust-lang/rust/issues/80486#issuecomment-752244166
                    .unwrap_or_else(|x| x)
                {
                    let stream_waker = cx.waker().clone();
                    let stream_wake_task_executed = self.stream_wake_task_executed.clone();
                    let stream_wake_task_should_shutdown =
                        self.stream_wake_task_should_shutdown.clone();

                    stream_wake_task_should_shutdown.store(false, Ordering::SeqCst);

                    let _ = self.task_sender.send(Task {
                        stream_waker,
                        stream_wake_task_executed,
                        stream_wake_task_should_shutdown,
                    });
                }
                Poll::Pending
            }
            Err(e) => Poll::Ready(Some(Err(e))),
        };
        result
    }
}

#[derive(Clone, Copy, Debug)]
enum StreamFilter {
    Input,
    Terminal,
    Responses,
}

impl Filter for StreamFilter {
    fn eval(&self, event: &InternalEvent) -> bool {
        if matches!(self, Self::Terminal | Self::Responses)
            && matches!(
                event,
                InternalEvent::OscColor { .. }
                    | InternalEvent::ColorSchemeChanged
                    | InternalEvent::OperatingStatus
            )
        {
            return true;
        }
        !matches!(self, Self::Responses) && EventFilter.eval(event)
    }
}

/// Input and terminal palette responses delivered by [`TerminalEventStream`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Ordinary keyboard, paste, mouse, focus, or resize input.
    Input(Event),
    /// An OSC color response. An unrecognized color is reported as `None`.
    Color {
        slot: u8,
        color: Option<crate::style::Color>,
    },
    /// A DEC mode 2031 notification; query OSC 10/11 to obtain the new colors.
    ColorSchemeChanged,
    /// Reply to a operating-status query, useful as a response-ordering barrier.
    OperatingStatus,
}

/// An opt-in event stream that delivers OSC color replies and DEC mode 2031 notifications.
///
/// Uses the same single input reader as [`EventStream`]. Applications can write color queries
/// without waiting for a reply or blocking keyboard input. Do not use this concurrently with
/// another event reader or the synchronous color-query helpers. Notifications must be enabled
/// separately by the application; this stream does not change terminal modes.
#[derive(Debug)]
pub struct TerminalEventStream(EventStream);

impl Default for TerminalEventStream {
    fn default() -> Self {
        Self(EventStream::with_filter(StreamFilter::Terminal))
    }
}

impl Stream for TerminalEventStream {
    type Item = io::Result<TerminalEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_internal_event(cx).map(|event| {
            event.map(|event| {
                event.map(|event| match event {
                    InternalEvent::Event(event) => TerminalEvent::Input(event),
                    InternalEvent::OscColor { slot, payload } => TerminalEvent::Color {
                        slot,
                        color: match payload {
                            super::internal::OscColorPayload::Rgb { r, g, b } => {
                                Some(crate::style::Color::Rgb { r, g, b })
                            }
                            super::internal::OscColorPayload::Unrecognized(_) => None,
                        },
                    },
                    InternalEvent::OperatingStatus => TerminalEvent::OperatingStatus,
                    InternalEvent::ColorSchemeChanged => TerminalEvent::ColorSchemeChanged,
                    #[cfg(unix)]
                    _ => unreachable!(),
                })
            })
        })
    }
}

/// Drain terminal responses through the next operating-status reply.
///
/// Drop the event stream before calling this function. Ordinary input stays in the shared
/// reader's queue. A timeout does not establish that outstanding queries have completed.
pub fn drain_terminal_responses(timeout: Duration) -> io::Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if !internal::poll(Some(remaining), &StreamFilter::Responses)? {
            if std::time::Instant::now() < deadline {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "terminal query still pending",
            ));
        }
        if matches!(
            internal::read(&StreamFilter::Responses)?,
            InternalEvent::OperatingStatus
        ) {
            return Ok(());
        }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.stream_wake_task_should_shutdown
            .store(true, Ordering::SeqCst);
        let _ = self.poll_internal_waker.wake();
    }
}

#[cfg(all(test, unix))]
#[path = "stream_tests.rs"]
mod tests;
