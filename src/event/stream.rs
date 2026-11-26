use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    task::{Context, Poll},
    thread,
    time::Duration,
};

use futures_core::stream::Stream;

use crate::event::{
    Event,
    filter::{EventFilter, Filter},
    internal::{self, InternalEvent},
    source::ReadGoal,
    sys::Waker,
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
    worker: Arc<Worker>,
    task_sender: SyncSender<std::task::Waker>,
}

impl Default for EventStream {
    fn default() -> Self {
        Self::with_filter(StreamFilter::Input)
    }
}

impl EventStream {
    fn with_filter(filter: StreamFilter) -> Self {
        let (task_sender, receiver) = mpsc::sync_channel::<std::task::Waker>(1);
        let worker = Arc::new(Worker::default());
        let state = worker.clone();
        thread::spawn(move || {
            while let Ok(waker) = receiver.recv() {
                loop {
                    // A handoff may have consumed the cancellation wake while this
                    // worker waited for the reader lock.
                    let mut reader = internal::lock_event_reader();
                    if state.shutdown.load(Ordering::SeqCst)
                        || matches!(reader.poll_until(None, &filter, state.goal()), Ok(true))
                    {
                        break;
                    }
                }
                #[cfg(unix)]
                state.waiting_for_query.store(false, Ordering::SeqCst);
                state.active.store(false, Ordering::SeqCst);
                waker.wake();
            }
        });

        EventStream {
            filter,
            poll_internal_waker: internal::lock_event_reader().waker(),
            worker,
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

#[derive(Debug, Default)]
struct Worker {
    active: AtomicBool,
    shutdown: AtomicBool,
    #[cfg(unix)]
    waiting_for_query: AtomicBool,
}

impl Worker {
    fn goal(&self) -> ReadGoal {
        #[cfg(unix)]
        if self.waiting_for_query.load(Ordering::SeqCst) {
            return ReadGoal::EventOrQueryBoundary;
        }
        ReadGoal::Event
    }
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
        self.poll_internal_event(cx).map_ok(|event| match event {
            InternalEvent::Event(event) => event,
            #[cfg(unix)]
            _ => unreachable!(),
        })
    }
}

impl EventStream {
    fn poll_internal_event(&self, cx: &mut Context<'_>) -> Poll<Option<io::Result<InternalEvent>>> {
        match internal::poll(Some(Duration::from_secs(0)), &self.filter) {
            Ok(true) => Poll::Ready(Some(internal::read(&self.filter))),
            Ok(false) => {
                if !self.worker.active.swap(true, Ordering::SeqCst) {
                    let _ = self.task_sender.send(cx.waker().clone());
                }
                Poll::Pending
            }
            Err(e) => Poll::Ready(Some(Err(e))),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum StreamFilter {
    Input,
    Terminal,
}

impl Filter for StreamFilter {
    fn eval(&self, event: &InternalEvent) -> bool {
        #[cfg(unix)]
        if matches!(self, Self::Terminal) && event.is_palette_report() {
            return true;
        }
        EventFilter.eval(event)
    }

    fn discard_unmatched(&self, event: &InternalEvent) -> bool {
        EventFilter.discard_unmatched(event)
    }
}

/// Input and terminal palette responses delivered by [`TerminalEventStream`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Ordinary keyboard, paste, mouse, focus, or resize input.
    Input(Event),
    /// An OSC 10 or 11 color response. An unrecognized color is reported as `None`.
    Color {
        slot: u8,
        color: Option<crate::style::Color>,
    },
    /// A DEC mode 2031 notification; query OSC 10/11 to obtain the new colors.
    ColorSchemeChanged,
    /// A DECRPM reply for mode 2031. Only status 2 (reset) allows taking ownership.
    ColorSchemeReporting(u8),
}

/// An opt-in event stream that delivers OSC color replies and DEC mode 2031 notifications.
///
/// Uses the same single input reader as [`EventStream`]. Applications can write color queries
/// without waiting for a reply or blocking keyboard input. Do not use this concurrently with
/// another event reader or the synchronous color-query helpers. Notifications must be enabled
/// separately by the application; this stream does not change terminal modes.
#[derive(Debug)]
pub struct TerminalEventStream {
    stream: EventStream,
}

impl Default for TerminalEventStream {
    fn default() -> Self {
        let stream = EventStream::with_filter(StreamFilter::Terminal);
        // A report already decoded by a previous owner cannot answer our first query.
        #[cfg(unix)]
        internal::lock_event_reader().discard_matching(InternalEvent::is_palette_report);
        Self { stream }
    }
}

#[cfg(unix)]
impl TerminalEventStream {
    /// Check the input boundary before writing a query whose reply shares the input stream.
    ///
    /// While a query is pending, check before each poll. Returns false without waiting
    /// when the reader is busy, a prior palette report is queued, input is unread, or a frame
    /// (including a bracketed paste or discarded sequence) is incomplete. New input may still
    /// arrive after this check. After returning false, the next stream poll can wake the
    /// caller to retry even if the completed frame was silently discarded.
    pub fn can_query(&self) -> bool {
        self.check_query(|reader| reader.can_query())
    }

    /// Prepare a batch ending in one mode 2031 status request.
    ///
    /// Call this for every batch, including status-only probes to check mode
    /// ownership. On success, send the batch immediately. While replies are
    /// outstanding, a fragmented OSC 10/11 selector gets up to two seconds from
    /// the latest query. An ambiguous legacy Escape or Alt+] may wait until that
    /// deadline or until all status replies arrive, whichever comes first.
    /// On failure, retry before the next stream poll as with [`Self::can_query`].
    pub fn prepare_color_query(&self) -> bool {
        self.check_query(|reader| reader.prepare_color_query())
    }

    fn check_query(
        &self,
        check: impl FnOnce(&mut super::read::InternalEventReader) -> bool,
    ) -> bool {
        let ready = internal::try_lock_event_reader_for(Duration::ZERO)
            .is_some_and(|mut reader| check(&mut reader));
        let worker = &self.stream.worker;
        let was_waiting = worker.waiting_for_query.swap(!ready, Ordering::SeqCst);
        // An active worker may already be waiting for an event. Interrupt it once
        // so it can pick up the new goal before blocking on the next read.
        if !ready && !was_waiting && worker.active.load(Ordering::SeqCst) {
            let _ = self.stream.poll_internal_waker.wake();
        }
        ready
    }
}

impl Stream for TerminalEventStream {
    type Item = io::Result<TerminalEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.stream
            .poll_internal_event(cx)
            .map_ok(|event| match event {
                InternalEvent::Event(event) => TerminalEvent::Input(event),
                #[cfg(unix)]
                InternalEvent::OscColor { slot, payload } => TerminalEvent::Color {
                    slot,
                    color: match payload {
                        super::internal::OscColorPayload::Rgb { r, g, b } => {
                            Some(crate::style::Color::Rgb { r, g, b })
                        }
                        super::internal::OscColorPayload::Unrecognized(_) => None,
                    },
                },
                #[cfg(unix)]
                InternalEvent::ColorSchemeChanged => TerminalEvent::ColorSchemeChanged,
                #[cfg(unix)]
                InternalEvent::ColorSchemeReporting(status) => {
                    TerminalEvent::ColorSchemeReporting(status)
                }
                #[cfg(unix)]
                _ => unreachable!(),
            })
    }
}

/// Finish palette queries before handing terminal input to another application.
///
/// Drop the event stream first. If the application enabled mode 2031, disable it
/// and flush the output before draining. Use this only on terminals that answer
/// primary device attributes (DA1) in order, with no other DA1 queries in flight.
#[cfg(unix)]
#[derive(Debug, Default)]
pub struct ColorReportHandoff {
    // None for the first DA1 request, then a distinct mode on each retry.
    next_mode: Option<u16>,
}

#[cfg(unix)]
impl ColorReportHandoff {
    /// Write a DA1 query to stdout at an input boundary and wait for its reply.
    /// Ordinary events stay queued in order. On failure, keep ownership of input
    /// and retain this handoff for the next attempt. If you cancel the transfer,
    /// you may recreate the stream and issue palette queries before retrying.
    /// Do not issue other DA1 queries while a handoff reply may still arrive.
    /// Retrying a sent marker requires a terminal that echoes unknown private
    /// modes with status 0.
    pub fn drain(&mut self, timeout: Duration) -> io::Result<bool> {
        use super::timeout::PollTimeout;
        use std::io::Write;

        let deadline = PollTimeout::new(Some(timeout));
        let Some(mut reader) = internal::try_lock_event_reader_for(timeout) else {
            return Ok(false);
        };
        // A screen transition may have discarded the previous reply. A fresh
        // number prevents a late reply from completing this attempt instead.
        let marker = {
            if !reader.prepare_color_handoff(&deadline)? {
                return Ok(false);
            }
            let mut out = io::stdout().lock();
            let marker = if let Some(mode) = self.next_mode {
                if !internal::HANDOFF_MODES.contains(&mode) {
                    return Err(io::Error::other("exhausted terminal handoff markers"));
                }
                write!(out, "\x1b[?{mode}$p")?;
                self.next_mode = Some(mode + 1);
                InternalEvent::ColorHandoffMarker(mode)
            } else {
                out.write_all(b"\x1b[c")?;
                self.next_mode = Some(*internal::HANDOFF_MODES.start());
                InternalEvent::PrimaryDeviceAttributes
            };
            out.flush()?;
            marker
        };
        // A cancelled stream can leave an interrupt in the source.
        while !deadline.elapsed() {
            if reader.poll(deadline.leftover(), &marker)? {
                reader.finish_color_handoff();
                self.next_mode = None;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.worker.shutdown.store(true, Ordering::SeqCst);
        if self.worker.active.load(Ordering::SeqCst) {
            let _ = self.poll_internal_waker.wake();
        }
    }
}

#[cfg(all(test, unix))]
#[path = "stream_tests.rs"]
mod tests;
