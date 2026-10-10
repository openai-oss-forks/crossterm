//! Keyboard capability replies preserve ordinary input and protected input boundaries.

use super::Parser;
use crate::event::internal::InternalEvent;
use crate::event::{Event, InputDiscardStatus, KeyCode, KeyboardEnhancementFlags};

#[test]
fn fragmented_keyboard_reply_preserves_surrounding_typeahead() {
    let reply = b"\x1b[?1u";
    for boundary in 1..reply.len() {
        let mut parser = Parser::default();
        parser.advance(b"early", true);
        parser.advance(&reply[..boundary], true);
        parser.advance(&reply[boundary..], false);
        parser.advance(b"later", false);

        let mut expected: Vec<_> = "early"
            .chars()
            .map(|key| InternalEvent::Event(Event::Key(KeyCode::Char(key).into())))
            .collect();
        expected.push(InternalEvent::KeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ));
        expected.extend(
            "later"
                .chars()
                .map(|key| InternalEvent::Event(Event::Key(KeyCode::Char(key).into()))),
        );
        assert_eq!(parser.collect::<Vec<_>>(), expected);
    }
}

#[test]
fn discarding_typeahead_retains_complete_keyboard_replies() {
    let mut parser = Parser::default();
    parser.advance(b"old\x1b[?1u", false);
    assert_eq!(
        parser.discard_buffered_input(),
        InputDiscardStatus::Complete
    );
    parser.advance(b"new", false);
    assert_eq!(
        parser.collect::<Vec<_>>(),
        vec![
            InternalEvent::KeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
            ),
            InternalEvent::Event(Event::Key(KeyCode::Char('n').into())),
            InternalEvent::Event(Event::Key(KeyCode::Char('e').into())),
            InternalEvent::Event(Event::Key(KeyCode::Char('w').into())),
        ]
    );
}

#[test]
fn discarded_partial_keyboard_reply_cannot_become_input() {
    let reply = b"\x1b[?1u";
    for boundary in 1..reply.len() {
        let mut parser = Parser::default();
        parser.advance(&reply[..boundary], true);
        assert_ne!(
            parser.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
        parser.advance(&reply[boundary..], false);
        parser.advance(b"n", false);
        assert_eq!(
            parser.collect::<Vec<_>>(),
            vec![InternalEvent::Event(Event::Key(KeyCode::Char('n').into()))]
        );
    }
}
