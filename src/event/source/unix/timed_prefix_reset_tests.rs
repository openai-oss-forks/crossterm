use super::Parser;
use crate::event::{
    Event, InputDiscardStatus, InternalEvent, KeyCode, KeyModifiers, MouseEvent, MouseEventKind,
};
use std::time::Instant;

#[test]
fn complete_discard_gives_the_next_tty_escape_a_fresh_window() {
    let mut parser = Parser::default();
    parser.buffer_tty_input(b"\x1b");
    parser.buffer_tty_input(b"\xc3");
    parser.pending_escape_deadline = Some(Instant::now());
    assert_eq!(
        parser.discard_buffered_input(),
        InputDiscardStatus::Complete
    );

    parser.buffer_tty_input(b"\x1b");
    assert_eq!(
        parser.next(),
        None,
        "a completed discard must not expire the new ESC"
    );
    parser.buffer_tty_input(b"[<65;101;53M");
    assert_eq!(
        parser.next(),
        Some(InternalEvent::Event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 100,
            row: 52,
            modifiers: KeyModifiers::NONE,
        })))
    );
    assert_eq!(parser.next(), None);
    assert_eq!(parser.pending_escape_deadline, None);
    assert!(!parser.timed_tty_escape);
}

#[test]
fn unsupported_short_frames_are_consumed_before_and_after_tty_expiry() {
    for introducer in [b'[', b'O'] {
        for expired in [false, true] {
            let mut parser = Parser::default();
            parser.buffer_tty_input(b"\x1b");
            parser.buffer_tty_input(&[introducer]);
            if expired {
                parser.pending_escape_deadline = Some(Instant::now());
            }
            parser.buffer_tty_input(b"x");
            assert_eq!(
                parser.next(),
                None,
                "introducer={introducer}, expired={expired}"
            );
            assert!(parser.buffer.is_empty());
            assert_eq!(parser.pending_escape_deadline, None);
        }
    }
}

#[test]
fn complete_mouse_and_unsupported_key_reports_never_turn_into_keys() {
    // Start with a real extra-button report, then cover every other Cb value.
    for cb in (128u8..=255).chain(0..128) {
        let mut parser = Parser::default();
        parser.buffer_tty_input(b"\x1b");
        parser.pending_escape_deadline = Some(Instant::now());
        parser.buffer_tty_input(format!("[<{cb};10;5M").as_bytes());
        let event = parser.next();
        assert!(
            matches!(event, None | Some(InternalEvent::Event(Event::Mouse(_)))),
            "Cb={cb}: complete SGR became {event:?}"
        );
        assert_eq!(parser.next(), None);
        parser.buffer_tty_input(b"q");
        assert_eq!(
            parser.next(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Char('q').into())))
        );
        assert_eq!(parser.next(), None);
    }
    for suffix in [b"[1;5E".as_slice(), b"[x", b"Ox"] {
        let mut parser = Parser::default();
        parser.buffer_tty_input(b"\x1b");
        parser.pending_escape_deadline = Some(Instant::now());
        parser.buffer_tty_input(suffix);
        assert_eq!(parser.next(), None, "unsupported frame {suffix:?}");
        parser.buffer_tty_input(b"q");
        assert_eq!(
            parser.next(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Char('q').into())))
        );
        assert_eq!(parser.next(), None);
    }
}
