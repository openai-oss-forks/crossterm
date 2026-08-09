use std::{
    collections::VecDeque,
    io,
    time::{Duration, Instant},
};

use mio::{unix::SourceFd, Events, Interest, Poll, Token};
use signal_hook_mio::v1_0::Signals;

#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    source::EventSource, sys::unix::parse::parse_event, timeout::PollTimeout, Event, InternalEvent,
};
use crate::terminal::sys::file_descriptor::{tty_fd, FileDesc};

// Tokens to identify file descriptor
const TTY_TOKEN: Token = Token(0);
const SIGNAL_TOKEN: Token = Token(1);
#[cfg(feature = "event-stream")]
const WAKE_TOKEN: Token = Token(2);

// I (@zrzka) wasn't able to read more than 1_022 bytes when testing
// reading on macOS/Linux -> we don't need bigger buffer and 1k of bytes
// is enough.
const TTY_BUFFER_SIZE: usize = 1_024;
const BUFFERED_ESCAPE_TIMEOUT: Duration = Duration::from_millis(20);

pub(crate) struct UnixInternalEventSource {
    poll: Poll,
    events: Events,
    parser: Parser,
    tty_buffer: [u8; TTY_BUFFER_SIZE],
    tty_fd: FileDesc<'static>,
    signals: Signals,
    #[cfg(feature = "event-stream")]
    waker: Waker,
}

impl UnixInternalEventSource {
    pub fn new() -> io::Result<Self> {
        UnixInternalEventSource::from_file_descriptor(tty_fd()?)
    }

    pub(crate) fn from_file_descriptor(input_fd: FileDesc<'static>) -> io::Result<Self> {
        let poll = Poll::new()?;
        let registry = poll.registry();

        let tty_raw_fd = input_fd.raw_fd();
        let mut tty_ev = SourceFd(&tty_raw_fd);
        registry.register(&mut tty_ev, TTY_TOKEN, Interest::READABLE)?;

        let mut signals = Signals::new([signal_hook::consts::SIGWINCH])?;
        registry.register(&mut signals, SIGNAL_TOKEN, Interest::READABLE)?;

        #[cfg(feature = "event-stream")]
        let waker = Waker::new(registry, WAKE_TOKEN)?;

        Ok(UnixInternalEventSource {
            poll,
            events: Events::with_capacity(3),
            parser: Parser::default(),
            tty_buffer: [0u8; TTY_BUFFER_SIZE],
            tty_fd: input_fd,
            signals,
            #[cfg(feature = "event-stream")]
            waker,
        })
    }
}

impl EventSource for UnixInternalEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> io::Result<Option<InternalEvent>> {
        if let Some(event) = self.parser.next() {
            return Ok(Some(event));
        }
        if let Some(event) = self.parser.finish_pending_escape() {
            return Ok(Some(event));
        }

        let timeout = PollTimeout::new(timeout);

        loop {
            let poll_timeout = self.parser.poll_timeout(timeout.leftover());
            if let Err(e) = self.poll.poll(&mut self.events, poll_timeout) {
                // Mio will throw an interrupted error in case of cursor position retrieval. We need to retry until it succeeds.
                // Previous versions of Mio (< 0.7) would automatically retry the poll call if it was interrupted (if EINTR was returned).
                // https://docs.rs/mio/0.7.0/mio/struct.Poll.html#notes
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                } else {
                    return Err(e);
                }
            };

            if self.events.is_empty() {
                // No readiness events = timeout
                return Ok(self.parser.finish_pending_escape());
            }

            for token in self.events.iter().map(|x| x.token()) {
                match token {
                    TTY_TOKEN => {
                        loop {
                            match self.tty_fd.read(&mut self.tty_buffer) {
                                Ok(read_count) => {
                                    if read_count > 0 {
                                        self.parser.advance(
                                            &self.tty_buffer[..read_count],
                                            read_count == TTY_BUFFER_SIZE,
                                        );
                                    }
                                }
                                Err(e) => {
                                    // No more data to read at the moment. We will receive another event
                                    if e.kind() == io::ErrorKind::WouldBlock {
                                        break;
                                    }
                                    // once more data is available to read.
                                    else if e.kind() == io::ErrorKind::Interrupted {
                                        continue;
                                    }
                                }
                            };

                            if let Some(event) = self.parser.next() {
                                return Ok(Some(event));
                            }
                        }
                    }
                    SIGNAL_TOKEN => {
                        if self.signals.pending().next() == Some(signal_hook::consts::SIGWINCH) {
                            // TODO Should we remove tput?
                            //
                            // This can take a really long time, because terminal::size can
                            // launch new process (tput) and then it parses its output. It's
                            // not a really long time from the absolute time point of view, but
                            // it's a really long time from the mio, async-std/tokio executor, ...
                            // point of view.
                            let new_size = crate::terminal::size()?;
                            return Ok(Some(InternalEvent::Event(Event::Resize(
                                new_size.0, new_size.1,
                            ))));
                        }
                    }
                    #[cfg(feature = "event-stream")]
                    WAKE_TOKEN => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "Poll operation was woken up by `Waker::wake`",
                        ));
                    }
                    _ => unreachable!("Synchronize Evented handle registration & token handling"),
                }
            }

            // Processing above can take some time, check if timeout expired
            if timeout.elapsed() {
                return Ok(self.parser.finish_pending_escape());
            }
        }
    }

    fn buffer_input(&mut self, input: &[u8], events: &mut VecDeque<InternalEvent>) {
        self.parser.buffer_external_input(input);
        events.extend(
            self.parser
                .by_ref()
                .filter(|event| matches!(event, InternalEvent::Event(_))),
        );
    }

    fn discard_buffered_input(&mut self) {
        self.parser.clear();
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.waker.clone()
    }
}

//
// Following `Parser` structure exists for two reasons:
//
//  * mimic anes Parser interface
//  * move the advancing, parsing, ... stuff out of the `try_read` method
//
#[derive(Debug)]
struct Parser {
    buffer: Vec<u8>,
    internal_events: VecDeque<InternalEvent>,
    pending_escape_deadline: Option<Instant>,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            // This buffer is used for -> 1 <- ANSI escape sequence. Are we
            // aware of any ANSI escape sequence that is bigger? Can we make
            // it smaller?
            //
            // Probably not worth spending more time on this as "there's a plan"
            // to use the anes crate parser.
            buffer: Vec::with_capacity(256),
            // TTY_BUFFER_SIZE is 1_024 bytes. How many ANSI escape sequences can
            // fit? What is an average sequence length? Let's guess here
            // and say that the average ANSI escape sequence length is 8 bytes. Thus
            // the buffer size should be 1024/8=128 to avoid additional allocations
            // when processing large amounts of data.
            //
            // There's no need to make it bigger, because when you look at the `try_read`
            // method implementation, all events are consumed before the next TTY_BUFFER
            // is processed -> events pushed.
            internal_events: VecDeque::with_capacity(128),
            pending_escape_deadline: None,
        }
    }
}

impl Parser {
    fn clear(&mut self) {
        self.buffer.clear();
        self.internal_events.clear();
        self.pending_escape_deadline = None;
    }

    fn buffer_external_input(&mut self, buffer: &[u8]) {
        self.advance(buffer, true);
        if self.buffer.as_slice() == b"\x1b" {
            self.pending_escape_deadline = Some(Instant::now() + BUFFERED_ESCAPE_TIMEOUT);
        }
    }

    fn poll_timeout(&self, timeout: Option<Duration>) -> Option<Duration> {
        let Some(deadline) = self.pending_escape_deadline else {
            return timeout;
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        Some(timeout.map_or(remaining, |timeout| timeout.min(remaining)))
    }

    fn finish_pending_escape(&mut self) -> Option<InternalEvent> {
        let deadline = self.pending_escape_deadline?;
        if Instant::now() < deadline {
            return None;
        }
        self.pending_escape_deadline = None;
        let event = parse_event(&self.buffer, false).ok().flatten()?;
        self.buffer.clear();
        Some(event)
    }

    fn advance(&mut self, buffer: &[u8], more: bool) {
        self.pending_escape_deadline = None;
        for (idx, byte) in buffer.iter().enumerate() {
            let more = idx + 1 < buffer.len() || more;

            self.buffer.push(*byte);

            match parse_event(&self.buffer, more) {
                Ok(Some(ie)) => {
                    self.internal_events.push_back(ie);
                    self.buffer.clear();
                }
                Ok(None) => {
                    // Event can't be parsed, because we don't have enough bytes for
                    // the current sequence. Keep the buffer and process next bytes.
                }
                Err(_) => {
                    // Event can't be parsed (not enough parameters, parameter is not a number, ...).
                    // Clear the buffer and continue with another sequence.
                    self.buffer.clear();
                }
            }
        }
    }
}

impl Iterator for Parser {
    type Item = InternalEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.internal_events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::{Instant, Parser};
    use crate::event::{Event, InternalEvent, KeyCode};

    #[test]
    fn externally_buffered_escape_remains_available_for_its_continuation() {
        let mut parser = Parser::default();
        parser.buffer_external_input(b"\x1b");
        assert_eq!(parser.next(), None);

        parser.advance(b"[A", false);
        assert_eq!(
            parser.next(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Up.into())))
        );
    }

    #[test]
    fn standalone_buffered_escape_is_emitted_after_its_ambiguity_window() {
        let mut parser = Parser::default();
        parser.buffer_external_input(b"\x1b");
        assert_eq!(parser.finish_pending_escape(), None);

        parser.pending_escape_deadline = Some(Instant::now());
        assert_eq!(
            parser.finish_pending_escape(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
    }

    #[test]
    fn clearing_parser_discards_incomplete_bracketed_paste() {
        let mut parser = Parser::default();
        parser.advance(b"\x1b[200~unfinished", true);
        assert_eq!(parser.next(), None);

        parser.clear();
        parser.advance(b"y", false);
        assert_eq!(
            parser.next(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Char('y').into())))
        );
    }
}
