use super::Parser;
use crate::event::{
    Event, InputDiscardStatus, InternalEvent, KeyCode, KeyEvent, KeyModifiers, MouseEvent,
    MouseEventKind,
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
fn invalid_printable_continuations_replay_only_after_tty_expiry() {
    for introducer in [b'[', b'O'] {
        for expired in [false, true] {
            let mut parser = Parser::default();
            parser.buffer_tty_input(b"\x1b");
            parser.buffer_tty_input(&[introducer]);
            if expired {
                parser.pending_escape_deadline = Some(Instant::now());
            }
            parser.buffer_tty_input(b"x");
            let expected = if expired {
                vec![
                    InternalEvent::Event(Event::Key(KeyCode::Esc.into())),
                    InternalEvent::Event(Event::Key(KeyEvent::new(
                        KeyCode::Char(introducer as char),
                        if introducer == b'O' {
                            KeyModifiers::SHIFT
                        } else {
                            KeyModifiers::NONE
                        },
                    ))),
                    InternalEvent::Event(Event::Key(KeyCode::Char('x').into())),
                ]
            } else {
                Vec::new()
            };
            assert_eq!(
                parser.by_ref().collect::<Vec<_>>(),
                expected,
                "introducer={introducer}, expired={expired}"
            );
            assert!(parser.buffer.is_empty());
            assert_eq!(parser.pending_escape_deadline, None);
        }
    }
}
