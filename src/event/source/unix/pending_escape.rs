//! A legacy Alt+] key and an OSC reply both begin with ESC ]. Use the Escape
//! ambiguity window until a numeric selector and semicolon identify a reply.
//! For an outstanding color query, a partial OSC 10/11 selector can take
//! the remaining reply budget until the mode status marker arrives.
//! A partial selector replayed by another consumer also gets the reply
//! timeout, even before the event stream starts. A bare Escape keeps its
//! ordinary keyboard deadline.
//! Completed replies (including unsupported ones) still take the normal path.

use std::time::{Duration, Instant};

use crate::event::{Event, InternalEvent, KeyCode, KeyEvent, KeyModifiers};

use super::Parser;

const ESCAPE_TIMEOUT: Duration = Duration::from_millis(20);
// Match the foreground/background color query budget after the OSC header.
const OSC_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

fn is_color_prefix(input: &[u8]) -> bool {
    !input.is_empty() && (b"\x1b]10;".starts_with(input) || b"\x1b]11;".starts_with(input))
}

#[cfg(feature = "event-stream")]
#[derive(Debug, Default)]
pub(super) struct ColorQueries {
    pending: usize,
    deadline: Option<Instant>,
}

#[cfg(feature = "event-stream")]
impl ColorQueries {
    pub(super) fn begin(&mut self) {
        // A key deadline can expire while its batch's status is still in flight.
        self.pending = self.pending.saturating_add(1);
        self.deadline = Some(Instant::now() + OSC_REPLY_TIMEOUT);
    }

    pub(super) fn finish(&mut self) {
        self.pending = self.pending.saturating_sub(1);
        if self.pending == 0 {
            self.deadline = None;
        }
    }

    fn prefix_deadline(&self, input: &[u8]) -> Option<Instant> {
        self.deadline.filter(|_| is_color_prefix(input))
    }
}

impl Parser {
    pub(super) fn buffer_replayed_input(&mut self, buffer: &[u8]) {
        self.advance(buffer, true);
        self.replayed_color_prefix = is_color_prefix(&self.buffer);
        self.arm_escape_timeout();
    }

    pub(super) fn buffer_external_input(&mut self, buffer: &[u8]) {
        if buffer.is_empty() {
            return;
        }
        // Process queued continuations before checking for an idle timeout.
        // A fragmented reply can keep making progress across many reads.
        self.advance(buffer, true);
        // A longer tail still contains bytes from the replayed frame. A new
        // Alt+] in this read must get its own, ordinary keyboard deadline.
        self.replayed_color_prefix &=
            self.buffer.len() > buffer.len() && is_color_prefix(&self.buffer);
        self.arm_escape_timeout();
    }

    fn arm_escape_timeout(&mut self) {
        let delay = match self.buffer.as_slice() {
            [0x1b, b']', ..] if self.replayed_color_prefix || self.has_osc_selector() => {
                OSC_REPLY_TIMEOUT
            }
            [0x1b] | [0x1b, b']', ..] => ESCAPE_TIMEOUT,
            _ => return,
        };
        let deadline = Some(Instant::now() + delay);
        #[cfg(feature = "event-stream")]
        let deadline = deadline.max(self.color_queries.prefix_deadline(&self.buffer));
        self.pending_escape_deadline = deadline;
    }

    #[cfg(feature = "event-stream")]
    pub(super) fn finish_color_handoff(&mut self) {
        self.color_queries = ColorQueries::default();
        // A key can follow the marker in the same read. Remove its query
        // extension without extending any existing key or OSC deadline.
        let previous = self.pending_escape_deadline;
        self.arm_escape_timeout();
        self.pending_escape_deadline = previous.min(self.pending_escape_deadline);
    }

    pub(super) fn interrupt_pending_osc(&mut self, byte: u8) {
        if !self.buffer.starts_with(b"\x1b]") {
            return;
        }
        if matches!(byte, 0x03 | 0x04) {
            self.resolve_pending_escape();
            // An incomplete UTF-8 character must not consume Ctrl+C or Ctrl+D.
            self.buffer.clear();
        } else if self.buffer.ends_with(b"\x1b") && byte != b'\\' {
            // The trailing ESC starts the next key or paste sequence.
            self.buffer.pop();
            self.resolve_pending_escape();
            self.buffer.clear();
            self.buffer.push(0x1b);
        }
    }

    pub(super) fn has_pending_escape_key(&self) -> bool {
        self.pending_escape_deadline.is_some() && !self.has_osc_selector()
    }

    pub(super) fn pending_escape_expired(&self) -> bool {
        self.pending_escape_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn has_osc_selector(&self) -> bool {
        let Some(suffix) = self.buffer.strip_prefix(b"\x1b]") else {
            return false;
        };
        suffix.first().is_some_and(u8::is_ascii_digit)
            && suffix.iter().find(|b| !b.is_ascii_digit()) == Some(&b';')
    }

    fn resolve_pending_escape(&mut self) {
        self.pending_escape_deadline = None;
        if self.has_osc_selector() {
            // Keep truncated terminal replies out of keyboard input.
            self.buffer.clear();
        } else if self.buffer == b"\x1b" {
            self.internal_events
                .push_back(InternalEvent::Event(Event::Key(KeyCode::Esc.into())));
            self.buffer.clear();
        } else {
            debug_assert!(self.buffer.starts_with(b"\x1b]"));
            self.internal_events
                .push_back(InternalEvent::Event(Event::Key(KeyEvent::new(
                    KeyCode::Char(']'),
                    KeyModifiers::ALT,
                ))));
            // Decode the suffix exactly as live input, retaining partial UTF-8.
            let pending = std::mem::take(&mut self.buffer);
            self.advance(&pending[2..], false);
        }
    }

    pub(super) fn finish_pending_escape(&mut self) -> Option<InternalEvent> {
        if self.pending_escape_expired() {
            self.resolve_pending_escape();
        }
        self.next()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::event::{source::EventSource, sys::unix::parse::parse_event};

    use super::*;

    fn key(c: char, modifiers: KeyModifiers) -> InternalEvent {
        InternalEvent::Event(Event::Key(KeyEvent::new(KeyCode::Char(c), modifiers)))
    }

    fn alt_bracket() -> InternalEvent {
        key(']', KeyModifiers::ALT)
    }

    fn check_every_split(input: &[u8], expected: &[InternalEvent]) {
        for split in 0..=input.len() {
            let mut parser = Parser::default();
            parser.buffer_external_input(&input[..split]);
            parser.buffer_external_input(&input[split..]);
            assert_eq!(parser.collect::<Vec<_>>(), expected, "split {split}");
        }
    }

    #[test]
    fn live_reader_delivers_alt_bracket_and_text_without_another_write() {
        for text in ["", "fast", "10é🦀"] {
            let (mut reader, mut writer) = super::super::tests::source_with_input();
            write!(writer, "\x1b]{text}").unwrap();
            assert_eq!(
                reader.try_read(Some(Duration::from_secs(1))).unwrap(),
                Some(alt_bracket())
            );
            for c in text.chars() {
                assert_eq!(
                    reader.try_read(Some(Duration::ZERO)).unwrap(),
                    Some(key(c, KeyModifiers::NONE))
                );
            }
            assert_eq!(reader.try_read(Some(Duration::ZERO)).unwrap(), None);
        }
    }

    #[test]
    fn alt_bracket_preserves_interrupts_without_waiting_for_a_timeout() {
        for (byte, c) in [(3, 'c'), (4, 'd')] {
            for text in ["", "fast"] {
                let mut input = format!("\x1b]{text}").into_bytes();
                input.push(byte);
                let mut expected = vec![alt_bracket()];
                expected.extend(text.chars().map(|c| key(c, KeyModifiers::NONE)));
                expected.push(key(c, KeyModifiers::CONTROL));
                check_every_split(&input, &expected);
            }
        }
        check_every_split(
            b"\x1b]\x1b[99;5u",
            &[alt_bracket(), key('c', KeyModifiers::CONTROL)],
        );
    }

    #[test]
    fn partial_utf8_after_alt_bracket_survives_timeout() {
        let mut parser = Parser::default();
        parser.buffer_external_input(b"\x1b]\xc3");
        *parser.pending_escape_deadline.as_mut().unwrap() = Instant::now();
        assert_eq!(parser.finish_pending_escape(), Some(alt_bracket()));
        parser.buffer_external_input(b"\xa9");
        assert_eq!(parser.collect::<Vec<_>>(), [key('é', KeyModifiers::NONE)]);
    }

    #[test]
    fn replayed_bare_escape_keeps_its_keyboard_deadline() {
        let (mut reader, _writer) = super::super::tests::source_with_input();
        reader.buffer_input(b"\x1b", &mut Default::default());
        assert_eq!(
            reader.try_read(Some(Duration::from_secs(1))).unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
    }

    #[test]
    fn replayed_color_selector_survives_delayed_fragments() {
        for (slot, replayed, queued) in [
            (10, b"\x1b]".as_slice(), b"".as_slice()),
            (11, b"\x1b", b"]"),
        ] {
            let (mut reader, mut writer) = super::super::tests::source_with_input();
            // Queue the continuation before starting the bare Escape deadline.
            writer.write_all(queued).unwrap();
            reader.buffer_input(replayed, &mut Default::default());
            for digit in slot.to_string().bytes() {
                assert_eq!(reader.try_read(Some(ESCAPE_TIMEOUT * 3)).unwrap(), None);
                writer.write_all(&[digit]).unwrap();
            }
            assert_eq!(reader.try_read(Some(ESCAPE_TIMEOUT * 3)).unwrap(), None);
            writer.write_all(b";rgb:ff/00/00\x07m\x1b]").unwrap();
            let reply = format!("\x1b]{slot};rgb:ff/00/00\x07");
            for expected in [
                parse_event(reply.as_bytes(), false).unwrap().unwrap(),
                key('m', KeyModifiers::NONE),
                alt_bracket(),
            ] {
                assert_eq!(
                    reader.try_read(Some(Duration::from_secs(1))).unwrap(),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn fragmented_color_replies_do_not_become_keys() {
        for reply in [
            b"\x1b]10;rgb:ff/00/00\x07".as_slice(),
            b"\x1b]11;rgb:ff/00/00\x1b\\",
        ] {
            let event = parse_event(reply, false).unwrap().unwrap();
            assert!(matches!(event, InternalEvent::OscColor { .. }));
            check_every_split(reply, &[event]);
        }
    }

    #[cfg(feature = "event-stream")]
    #[test]
    fn outstanding_color_query_protects_the_selector_until_its_status_reply() {
        let (mut reader, mut writer) = super::super::tests::source_with_input();
        reader.expect_color_replies();
        // Expiry restores the keyboard timeout without cancelling an in-flight status.
        reader.parser.color_queries.deadline = Some(Instant::now());
        writer.write_all(b"\x1b]").unwrap();
        assert_eq!(
            reader.try_read(Some(Duration::from_secs(1))).unwrap(),
            Some(alt_bracket())
        );
        reader.expect_color_replies();
        writer.write_all(b"\x1b[?2031;1$y\x1b]").unwrap();
        assert_eq!(
            reader.try_read(Some(Duration::from_secs(1))).unwrap(),
            Some(InternalEvent::ColorSchemeReporting(1))
        );
        assert_eq!(reader.try_read(Some(ESCAPE_TIMEOUT * 3)).unwrap(), None);
        writer
            .write_all(b"11;rgb:ff/ff/ff\x07\x1b[?2031;1$y\x1b]")
            .unwrap();
        let reply = parse_event(b"\x1b]11;rgb:ff/ff/ff\x07", false).unwrap();
        for event in [
            reply,
            Some(InternalEvent::ColorSchemeReporting(1)),
            Some(alt_bracket()),
        ] {
            assert_eq!(
                reader.try_read(Some(Duration::from_secs(1))).unwrap(),
                event
            );
        }
        assert_eq!(reader.try_read(Some(Duration::ZERO)).unwrap(), None);
    }

    #[test]
    fn split_enhanced_interrupt_gets_its_own_escape_window() {
        let mut parser = Parser::default();
        parser.buffer_external_input(b"\x1b]");
        parser.pending_escape_deadline = Some(Instant::now());
        parser.buffer_external_input(b"\x1b");
        parser.buffer_external_input(b"[99;5u");
        assert_eq!(
            parser.collect::<Vec<_>>(),
            [alt_bracket(), key('c', KeyModifiers::CONTROL)]
        );
    }

    #[cfg(feature = "event-stream")]
    #[test]
    fn transition_discard_waits_for_escape_but_strict_discard_quarantines_it() {
        use crate::event::{InputDiscardStatus, filter::EventFilter, read::InternalEventReader};

        let (source, _writer) = super::super::tests::source_with_input();
        let mut reader = InternalEventReader::with_test_source(source);
        assert!(reader.prepare_color_query());
        reader.buffer_input(b"\x1b").unwrap();
        assert_eq!(
            reader.discard_buffered_input_after_poll().unwrap(),
            InputDiscardStatus::ControlSequenceInProgress
        );
        reader.buffer_input(b"y").unwrap();
        assert_eq!(
            reader.try_read(&EventFilter),
            Some(key('y', KeyModifiers::ALT))
        );

        reader.buffer_input(b"\x1b").unwrap();
        assert_eq!(
            reader.discard_buffered_input().unwrap(),
            InputDiscardStatus::BracketedPasteInProgress
        );
        reader.buffer_input(b"yx").unwrap();
        assert_eq!(
            reader.try_read(&EventFilter),
            Some(key('x', KeyModifiers::NONE))
        );
    }

    #[cfg(feature = "event-stream")]
    #[test]
    fn handoff_releases_a_key_buffered_after_the_marker_when_a_status_was_lost() {
        let (mut reader, mut writer) = super::super::tests::source_with_input();
        reader.expect_color_replies();
        writer.write_all(b"\x1b[?62;22c\x1b]").unwrap();
        assert_eq!(
            reader.try_read(Some(Duration::from_secs(1))).unwrap(),
            Some(InternalEvent::PrimaryDeviceAttributes)
        );
        reader.finish_color_handoff();
        assert_eq!(
            reader.try_read(Some(Duration::from_secs(1))).unwrap(),
            Some(alt_bracket())
        );
    }

    #[test]
    fn queued_color_reply_keeps_progress_across_expired_deadlines() {
        let mut parser = Parser::default();
        parser.buffer_external_input(b"\x1b]");
        for fragment in [b"1".as_slice(), b"1;rgb:", b"ff/00/00", b"\x07"] {
            // Model a descheduled reader resuming with bytes already queued.
            parser.pending_escape_deadline = Some(Instant::now());
            parser.buffer_external_input(fragment);
        }
        assert!(matches!(
            parser.next(),
            Some(InternalEvent::OscColor { slot: 11, .. })
        ));
        assert_eq!(parser.next(), None);
    }

    #[test]
    fn truncated_osc_drops_reply_bytes_and_preserves_fresh_input() {
        let mut parser = Parser::default();
        parser.buffer_external_input(b"\x1b]10;");
        parser.buffer_external_input(b"rgb:ff/");
        assert!(parser.pending_escape_deadline.is_some());
        parser.pending_escape_deadline = Some(Instant::now());
        assert_eq!(parser.finish_pending_escape(), None);
        parser.buffer_external_input(b"x");
        assert_eq!(parser.collect::<Vec<_>>(), [key('x', KeyModifiers::NONE)]);
        check_every_split(
            b"\x1b]52;c;unfinished\x03",
            &[key('c', KeyModifiers::CONTROL)],
        );
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn alt_bracket_preserves_the_next_bracketed_paste() {
        check_every_split(
            b"\x1b]\x1b[200~literal\x1b]\x03\x1b[201~",
            &[
                alt_bracket(),
                InternalEvent::Event(Event::Paste("literal\x1b]\x03".into())),
            ],
        );
    }
}
