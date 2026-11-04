use super::Parser;
use crate::event::{Event, InputDiscardStatus, InternalEvent, KeyCode};
use std::time::Instant;

#[test]
fn completed_utf8_osc_from_a_timed_prefix_is_not_replayed_as_keys() {
    for discard in [false, true] {
        let mut parser = Parser::default();
        parser.buffer_tty_input(b"\x1b");
        if discard {
            parser.discard_buffered_input();
        }
        parser.buffer_tty_input(b"]");
        parser.pending_escape_deadline = Some(Instant::now());
        // A complete, unhandled OSC is consumed, including its valid UTF-8 body.
        // Only the independent byte after its terminator becomes a key.
        parser.buffer_tty_input("0;日本\x07q".as_bytes());
        assert_eq!(
            parser.by_ref().collect::<Vec<_>>(),
            vec![InternalEvent::Event(Event::Key(KeyCode::Char('q').into()))],
            "discarded={discard}"
        );
        assert_eq!(
            parser.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
    }
}
