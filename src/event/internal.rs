use std::time::Duration;

use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};

#[cfg(unix)]
use crate::event::KeyboardEnhancementFlags;
use crate::event::{Event, filter::Filter, read::InternalEventReader, timeout::PollTimeout};

/// Static instance of `InternalEventReader`.
/// This needs to be static because there can be one event reader.
static EVENT_READER: Mutex<Option<InternalEventReader>> = parking_lot::const_mutex(None);

pub(crate) fn lock_event_reader() -> MappedMutexGuard<'static, InternalEventReader> {
    MutexGuard::map(EVENT_READER.lock(), |reader| {
        reader.get_or_insert_with(InternalEventReader::default)
    })
}

pub(crate) fn try_lock_event_reader_for(
    duration: Duration,
) -> Option<MappedMutexGuard<'static, InternalEventReader>> {
    Some(MutexGuard::map(
        EVENT_READER.try_lock_for(duration)?,
        |reader| reader.get_or_insert_with(InternalEventReader::default),
    ))
}

/// Polls to check if there are any `InternalEvent`s that can be read within the given duration.
pub(crate) fn poll<F>(timeout: Option<Duration>, filter: &F) -> std::io::Result<bool>
where
    F: Filter,
{
    let (mut reader, timeout) = if let Some(timeout) = timeout {
        let poll_timeout = PollTimeout::new(Some(timeout));
        let reader = match try_lock_event_reader_for(timeout) {
            Some(reader) => reader,
            None => return Ok(false),
        };
        (reader, poll_timeout.leftover())
    } else {
        (lock_event_reader(), None)
    };
    reader.poll(timeout, filter)
}

/// Check cancellation after acquiring the reader so a dropped stream cannot start another poll.
/// A different input owner may have consumed the wake notification while this task waited for
/// the lock; once the lock is held, a later cancellation wake can only reach this poll.
#[cfg(feature = "event-stream")]
pub(crate) fn poll_event_stream<F>(
    filter: &F,
    shutdown: &std::sync::atomic::AtomicBool,
) -> std::io::Result<bool>
where
    F: Filter,
{
    let mut reader = lock_event_reader();
    if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(false);
    }
    reader.poll(None, filter)
}

/// Reads a single `InternalEvent`.
pub(crate) fn read<F>(filter: &F) -> std::io::Result<InternalEvent>
where
    F: Filter,
{
    let mut reader = lock_event_reader();
    reader.read(filter)
}

/// Reads a single `InternalEvent`. Non-blocking.
pub(crate) fn try_read<F>(filter: &F) -> Option<InternalEvent>
where
    F: Filter,
{
    let mut reader = lock_event_reader();
    reader.try_read(filter)
}

/// An internal event.
///
/// Encapsulates publicly available `Event` with additional internal
/// events that shouldn't be publicly available to the crate users.
#[derive(Debug, PartialOrd, PartialEq, Hash, Clone, Eq)]
pub(crate) enum InternalEvent {
    /// An event.
    Event(Event),
    /// A cursor position (`col`, `row`).
    #[cfg(unix)]
    CursorPosition(u16, u16),
    /// The progressive keyboard enhancement flags enabled by the terminal.
    #[cfg(unix)]
    KeyboardEnhancementFlags(KeyboardEnhancementFlags),
    /// A successfully decoded CSI-u key proves keyboard enhancement is already active.
    #[cfg(unix)]
    KeyboardEnhancementDetected,
    /// Attributes and architectural class of the terminal.
    #[cfg(unix)]
    PrimaryDeviceAttributes,
    /// OSC color response (`slot`, `payload`).
    OscColor { slot: u8, payload: OscColorPayload },
    /// Successful operating-status reply (DSR 5).
    OperatingStatus,
    /// A DEC mode 2031 palette-change notification.
    ColorSchemeChanged,
}

impl InternalEvent {
    /// Replay input and its encoding evidence, but not query replies already read by the caller.
    #[cfg(unix)]
    pub(crate) fn is_replayed_input(&self) -> bool {
        matches!(self, Self::Event(_) | Self::KeyboardEnhancementDetected)
    }

    /// Keep capability replies and palette boundaries when an application quarantines user input.
    #[cfg(unix)]
    pub(crate) fn is_terminal_response(&self) -> bool {
        matches!(
            self,
            Self::KeyboardEnhancementFlags(_)
                | Self::KeyboardEnhancementDetected
                | Self::OscColor { .. }
                | Self::OperatingStatus
                | Self::ColorSchemeChanged
        )
    }
}

/// Parsed payload of an OSC color response.
#[derive(Debug, PartialOrd, PartialEq, Hash, Clone, Eq)]
pub(crate) enum OscColorPayload {
    /// Parsed RGB values (always 8-bit per channel).
    Rgb { r: u8, g: u8, b: u8 },
    /// Payload was returned but not recognized/parsible.
    Unrecognized(String),
}
