use super::Parser;
use crate::event::{Event, InputDiscardStatus, InternalEvent, KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode, modifiers: KeyModifiers) -> InternalEvent {
    InternalEvent::Event(Event::Key(KeyEvent::new(code, modifiers)))
}

#[test]
fn osc_does_not_swallow_interrupt_or_eof_keys() {
    for prefix in [b"\x1b]".as_slice(), b"\x1b]10;rgb:", b"\x1b]999;unknown"] {
        for (byte, ch) in [(b'\x03', 'c'), (b'\x04', 'd')] {
            for discard in [false, true] {
                let mut parser = Parser::default();
                parser.advance(prefix, true);
                if discard {
                    assert_eq!(
                        parser.discard_buffered_input(),
                        InputDiscardStatus::ControlSequenceInProgress
                    );
                }
                parser.advance(&[byte, b'q'], false);
                assert_eq!(
                    parser.next(),
                    Some(key(KeyCode::Char(ch), KeyModifiers::CONTROL))
                );
                assert_eq!(
                    parser.next(),
                    Some(key(KeyCode::Char('q'), KeyModifiers::NONE))
                );
                assert_eq!(parser.next(), None);
            }
        }
    }
}

#[test]
fn osc_does_not_swallow_a_new_escape_sequence() {
    for discard in [false, true] {
        let mut parser = Parser::default();
        parser.advance(b"\x1b]", true);
        if discard {
            parser.discard_buffered_input();
        }
        // Split between ESC and the rest, just like a fragmented ST terminator.
        parser.advance(b"\x1b", true);
        parser.advance(b"[99;5uq", false);
        assert_eq!(
            parser.next(),
            Some(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            parser.next(),
            Some(key(KeyCode::Char('q'), KeyModifiers::NONE))
        );
        assert_eq!(parser.next(), None);
    }
}

#[test]
fn fragmented_color_reports_and_unknown_osc_remain_framed() {
    for sequence in [
        b"\x1b]10;rgb:ffff/0000/0000\x07".as_slice(),
        b"\x1b]11;rgb:ffff/0000/0000\x1b\\",
        b"\x1b]999;unknown\x07",
    ] {
        for boundary in 2..sequence.len() {
            for discard in [false, true] {
                let mut parser = Parser::default();
                parser.advance(&sequence[..boundary], true);
                if discard {
                    parser.discard_buffered_input();
                }
                parser.advance(&sequence[boundary..], false);
                if !discard && !sequence.starts_with(b"\x1b]999") {
                    assert!(matches!(
                        parser.next(),
                        Some(InternalEvent::OscColor { .. })
                    ));
                }
                assert_eq!(parser.next(), None);
                parser.advance(b"q", false);
                assert_eq!(
                    parser.next(),
                    Some(key(KeyCode::Char('q'), KeyModifiers::NONE))
                );
                assert_eq!(parser.next(), None);
            }
        }
    }
}

#[test]
fn osc_like_paste_content_is_not_interpreted_as_keys() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[200~\x1b]\x03\x04\x1b[99;5u\x1b[201~", false);
    assert_eq!(
        parser.next(),
        Some(InternalEvent::Event(Event::Paste(
            "\x1b]\x03\x04\x1b[99;5u".into()
        )))
    );
    assert_eq!(parser.next(), None);
}

#[test]
fn osc_timeout_is_absolute_and_clears_stale_input_before_new_keys() {
    for discard in [false, true] {
        let mut parser = Parser::default();
        parser.advance(b"\x1b]10;", true);
        let deadline = parser.pending_osc_deadline;
        parser.advance(b"rgb:", true);
        assert_eq!(parser.pending_osc_deadline, deadline);
        if discard {
            parser.discard_buffered_input();
            assert_eq!(parser.pending_osc_deadline, deadline);
        }
        parser.pending_osc_deadline = Some(std::time::Instant::now());
        parser.advance(b"q", false);
        assert_eq!(
            parser.next(),
            Some(key(KeyCode::Char('q'), KeyModifiers::NONE))
        );
        assert_eq!(parser.next(), None);
        assert_eq!(parser.pending_osc_deadline, None);
        assert_eq!(
            parser.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
    }
}

#[test]
fn expired_osc_wakes_reader_without_new_input() {
    for prefix in [b"\x1b]".as_slice(), b"\x1b]10;rgb:", b"\x1b]10;rgb:\x1b"] {
        for discard in [false, true] {
            let (mut source, _writer) = super::tests::source_with_input();
            source.parser.buffer_external_input(prefix);
            if discard {
                source.parser.discard_buffered_input();
            }
            source.parser.pending_osc_deadline = Some(std::time::Instant::now());
            let expected = if discard {
                None
            } else if prefix == b"\x1b]" {
                Some(key(KeyCode::Char(']'), KeyModifiers::ALT))
            } else if prefix.ends_with(b"\x1b") {
                Some(key(KeyCode::Esc, KeyModifiers::NONE))
            } else {
                None
            };
            use crate::event::source::EventSource;
            assert_eq!(
                source.try_read(Some(std::time::Duration::ZERO)).unwrap(),
                expected
            );
            assert_eq!(
                source.discard_buffered_input(),
                InputDiscardStatus::Complete
            );
        }
    }
}

#[test]
fn osc_deadline_participates_in_poll_without_changing_shorter_deadlines() {
    use std::time::Duration;
    let mut parser = Parser::default();
    parser.advance(b"\x1b]", true);
    let timeout = parser.poll_timeout(None).unwrap();
    assert!(timeout <= super::OSC_TIMEOUT && !timeout.is_zero());
    assert_eq!(
        parser.poll_timeout(Some(Duration::ZERO)),
        Some(Duration::ZERO)
    );
    parser.discard_buffered_input();
    assert!(parser.poll_timeout(None).unwrap() <= timeout);
    parser.advance(b"\x07", false);
    assert_eq!(parser.poll_timeout(None), None);
}

#[test]
fn discarded_escape_prefix_transition_into_osc_has_a_deadline() {
    let mut parser = Parser::default();
    parser.buffer_external_input(b"\x1b");
    parser.discard_buffered_input();
    parser.advance(b"]10;unfinished", true);
    assert!(parser.pending_osc_deadline.is_some());
    parser.pending_osc_deadline = Some(std::time::Instant::now());
    parser.advance(b"q", false);
    assert_eq!(
        parser.next(),
        Some(key(KeyCode::Char('q'), KeyModifiers::NONE))
    );
    assert_eq!(parser.next(), None);
}
