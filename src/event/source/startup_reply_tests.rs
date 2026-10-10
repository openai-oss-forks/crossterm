//! Keyboard capability replies preserve ordinary input and protected input boundaries.

use super::Parser;
use crate::event::internal::InternalEvent;
use crate::event::{
    Event, InputDiscardStatus, KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags,
};

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

#[test]
fn encoded_alt_key_reports_capability_before_the_key_and_delayed_reply() {
    let encoded_key = b"\x1b[120;3u";
    for split in 0..=encoded_key.len() {
        let mut parser = Parser::default();
        parser.buffer_external_input(&encoded_key[..split]);
        parser.advance(&encoded_key[split..], false);
        parser.advance(b"\x1b[?1u\x1b[121u", false);
        let events = parser.collect::<Vec<_>>();
        let alt_x = InternalEvent::Event(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::ALT,
        )));
        let y = InternalEvent::Event(Event::Key(KeyCode::Char('y').into()));
        assert_eq!(
            events,
            vec![
                InternalEvent::KeyboardEnhancementDetected,
                alt_x.clone(),
                InternalEvent::KeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
                ),
                y.clone(),
            ],
        );
        assert_eq!(
            events
                .into_iter()
                .filter(|event| matches!(event, InternalEvent::Event(_)))
                .collect::<Vec<_>>(),
            vec![alt_x, y],
        );
    }
}

#[test]
fn discarding_encoded_input_preserves_its_capability_evidence() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[120;3u", false);
    assert_eq!(
        parser.discard_buffered_input(),
        InputDiscardStatus::Complete
    );
    assert_eq!(
        parser.collect::<Vec<_>>(),
        vec![InternalEvent::KeyboardEnhancementDetected],
    );
}

#[test]
fn malformed_and_incomplete_keys_do_not_report_capability() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[1114112u\x1b[120;", true);
    assert_eq!(parser.next(), None);
    parser.advance(b"3u", false);
    assert_eq!(
        parser.collect::<Vec<_>>(),
        vec![
            InternalEvent::KeyboardEnhancementDetected,
            InternalEvent::Event(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::ALT,
            ))),
        ],
    );
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn encoded_keys_inside_paste_remain_text() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[200~\x1b[120;3u\x1b[201~", false);
    assert_eq!(
        parser.collect::<Vec<_>>(),
        vec![InternalEvent::Event(Event::Paste("\x1b[120;3u".into()))],
    );
}
