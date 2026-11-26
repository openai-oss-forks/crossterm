//! Secondary device attributes must remain terminal input, including across startup replay.

use super::Parser;
use crate::event::{Event, InternalEvent, KeyCode};

#[test]
fn secondary_device_attributes_do_not_become_keys_at_any_split() {
    for reply in ["\x1b[>1;95;0c", "\x1b[>64;2500;0c", "\x1b[>1;4000;46c"] {
        let input = format!("a{reply}b");
        for split in 0..=input.len() {
            let mut parser = Parser::default();
            parser.buffer_external_input(&input.as_bytes()[..split]);
            parser.advance(&input.as_bytes()[split..], /*more*/ false);
            assert_eq!(
                parser.collect::<Vec<_>>(),
                vec![
                    InternalEvent::Event(Event::Key(KeyCode::Char('a').into())),
                    InternalEvent::Event(Event::Key(KeyCode::Char('b').into())),
                ],
                "reply {reply:?}, split {split}"
            );
        }
    }
}

#[test]
#[cfg(feature = "bracketed-paste")]
fn secondary_device_attributes_inside_paste_remain_text() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[200~\x1b[>1;95;0c\x1b[201~", /*more*/ false);
    assert_eq!(
        parser.collect::<Vec<_>>(),
        vec![InternalEvent::Event(Event::Paste("\x1b[>1;95;0c".into()))]
    );
}

#[cfg(feature = "event-stream")]
#[test]
fn finishing_an_ignored_reply_releases_a_deferred_color_query() {
    use crate::event::source::{EventSource, ReadGoal};
    use std::{io::Write, time::Duration};

    let (mut source, mut terminal) = super::tests::source_with_input();
    terminal.write_all(b"\x1b[?997;2n\x1b[>1;").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::ColorSchemeChanged),
    );
    assert!(!source.can_query());

    // A DA2 reply has no public event. The query must resume before another key.
    terminal.write_all(b"95;0c").unwrap();
    let deadline = crate::event::timeout::PollTimeout::new(Some(Duration::from_secs(5)));
    assert_eq!(
        source
            .read_until(deadline.leftover(), ReadGoal::EventOrQueryBoundary)
            .unwrap(),
        None,
    );
    assert!(!deadline.elapsed(), "the reader waited for another event");
    assert!(source.can_query());

    terminal.write_all(b"x").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyCode::Char('x').into()))),
    );
}
