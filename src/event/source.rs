use std::{io, time::Duration};

use super::internal::InternalEvent;
#[cfg(unix)]
use std::collections::VecDeque;

#[cfg(unix)]
use super::InputDiscardStatus;

#[cfg(feature = "event-stream")]
use super::sys::Waker;

#[cfg(unix)]
pub(crate) mod unix;
#[cfg(windows)]
pub(crate) mod windows;

#[derive(Clone, Copy)]
pub(crate) enum ReadGoal {
    Event,
    #[cfg(all(unix, feature = "event-stream"))]
    EventOrQueryBoundary,
}

impl ReadGoal {
    pub(crate) fn boundary_reached(self, _source: &dyn EventSource) -> bool {
        match self {
            Self::Event => false,
            #[cfg(all(unix, feature = "event-stream"))]
            Self::EventOrQueryBoundary => _source.can_query(),
        }
    }
}

/// An interface for trying to read an `InternalEvent` within an optional `Duration`.
pub(crate) trait EventSource: Sync + Send {
    /// Tries to read an `InternalEvent` within the given duration.
    ///
    /// # Arguments
    ///
    /// * `timeout` - `None` block indefinitely until an event is available, `Some(duration)` blocks
    ///   for the given timeout
    ///
    /// Returns `Ok(None)` if there's no event available and timeout expires.
    fn try_read(&mut self, timeout: Option<Duration>) -> io::Result<Option<InternalEvent>>;

    /// Also allow a waiting query to resume when input finishes without producing an event.
    fn read_until(
        &mut self,
        timeout: Option<Duration>,
        _goal: ReadGoal,
    ) -> io::Result<Option<InternalEvent>> {
        self.try_read(timeout)
    }

    /// Parses bytes consumed outside the source and appends their decoded events in order.
    #[cfg(unix)]
    fn buffer_input(&mut self, input: &[u8], events: &mut VecDeque<InternalEvent>);

    /// Discards buffered input while preserving incomplete control-sequence boundaries.
    #[cfg(unix)]
    fn discard_buffered_input(&mut self) -> InputDiscardStatus;

    /// Whether a query can be sent without inserting its response into a partial input frame.
    #[cfg(all(unix, feature = "event-stream"))]
    fn can_query(&self) -> bool;

    /// Give a possible OSC color prefix time to finish until the status reply.
    #[cfg(all(unix, feature = "event-stream"))]
    fn expect_color_replies(&mut self);

    /// A matched handoff marker rules out any further replies to earlier queries.
    #[cfg(all(unix, feature = "event-stream"))]
    fn finish_color_handoff(&mut self);

    /// Remove matching responses already decoded into the source's local queue.
    #[cfg(all(unix, feature = "event-stream"))]
    fn discard_matching(&mut self, matches: fn(&InternalEvent) -> bool);

    /// Can a buffered escape resolve without further input?
    #[cfg(unix)]
    fn has_pending_escape(&self) -> bool {
        false
    }

    /// Returns a `Waker` allowing to wake/force the `try_read` method to return `Ok(None)`.
    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker;
}
