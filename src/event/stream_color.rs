use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

#[cfg(all(unix, feature = "bracketed-paste"))]
use std::collections::VecDeque;

use futures_core::stream::Stream;

use super::EventStream;
use crate::{
    event::{Event, InternalEvent},
    style::Color,
};

/// An ordinary input event or a terminal default-color report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventWithColor {
    /// An ordinary keyboard, paste, mouse, focus, or resize event.
    Event(Event),
    /// The terminal's default foreground color, reported by OSC 10.
    ForegroundColor(Color),
    /// The terminal's default background color, reported by OSC 11.
    BackgroundColor(Color),
}

impl From<Event> for EventWithColor {
    fn from(event: Event) -> Self {
        Self::Event(event)
    }
}

/// An opt-in event stream that includes terminal default-color reports.
///
/// Construct this with [`EventStream::with_color_reports`]. Like `EventStream`, it must not
/// be combined with another terminal event reader.
#[derive(Debug)]
pub struct ColorEventStream {
    inner: EventStream,
}

impl ColorEventStream {
    pub(super) fn new(inner: EventStream) -> Self {
        Self { inner }
    }
}

impl Stream for ColorEventStream {
    type Item = io::Result<EventWithColor>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.poll_internal_event(cx) {
            Poll::Ready(Some(Ok(InternalEvent::Event(event)))) => {
                Poll::Ready(Some(Ok(EventWithColor::Event(event))))
            }
            #[cfg(unix)]
            Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(color_report(&event)
                .expect("color stream filter only accepts recognized color reports")))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(unix)]
pub(super) fn color_report(event: &InternalEvent) -> Option<EventWithColor> {
    use crate::event::OscColorPayload;
    let InternalEvent::OscColor {
        slot,
        payload: OscColorPayload::Rgb { r, g, b },
    } = event
    else {
        return None;
    };
    let color = Color::Rgb {
        r: *r,
        g: *g,
        b: *b,
    };
    match slot {
        10 => Some(EventWithColor::ForegroundColor(color)),
        11 => Some(EventWithColor::BackgroundColor(color)),
        _ => None,
    }
}

#[cfg(all(unix, feature = "bracketed-paste"))]
pub(in crate::event) fn extract_paste_colors(text: String, pending: &mut VecDeque<InternalEvent>) {
    const MAX_COLOR_REPORT_BYTES: usize = 1024;
    let bytes = text.as_bytes();
    let mut retained = String::new();
    let mut copied = 0;
    let mut cursor = 0;
    while cursor + 2 < bytes.len() {
        if !bytes[cursor..].starts_with(b"\x1b]") {
            cursor += 1;
            continue;
        }
        let limit = bytes.len().min(cursor + MAX_COLOR_REPORT_BYTES);
        let end = (cursor + 2..limit).find_map(|index| match bytes[index] {
            b'\x07' => Some(index + 1),
            b'\x1b' if bytes.get(index + 1) == Some(&b'\\') && index + 1 < limit => Some(index + 2),
            _ => None,
        });
        if let Some(end) = end {
            if let Ok(Some(event)) =
                crate::event::sys::unix::parse::parse_event(&bytes[cursor..end], false)
            {
                if color_report(&event).is_some() {
                    retained.push_str(&text[copied..cursor]);
                    pending.push_back(event);
                    copied = end;
                    cursor = end;
                    continue;
                }
            }
        }
        cursor += 2;
    }
    if copied == 0 {
        pending.push_back(InternalEvent::Event(Event::Paste(text)));
    } else {
        retained.push_str(&text[copied..]);
        pending.push_back(InternalEvent::Event(Event::Paste(retained)));
    }
}

#[cfg(all(test, unix))]
#[path = "stream_color_tests.rs"]
mod tests;
