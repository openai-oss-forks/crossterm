use super::{InputDiscardStatus, Parser};
use crate::event::{Event, InternalEvent, KeyCode};

#[test]
fn nan_mouse_reports_leave_the_next_key_at_every_split() {
    let x = KeyCode::Char('x');
    for (input, expected) in [
        ("\x1b[<16;NaN;NaNmx", x),
        ("\x1b[<64;NaN;12Mx", x),
        ("\x1b[<0;32;NaN;mx", x),
        ("\x1b[<16;Nx", x),
        ("\x1b[<0;Nax", x),
        ("\x1b[<0;NaN;NaNx", x),
        ("\x1b[<16;NaNm", KeyCode::Char('m')),
        ("\x1b[<16;NaN;Na\x1b[A", KeyCode::Up),
    ] {
        let bytes = input.as_bytes();
        for split in 0..=bytes.len() {
            let mut parser = Parser::default();
            parser.buffer_external_input(&bytes[..split]);
            parser.advance(&bytes[split..], false);
            assert_eq!(
                parser.collect::<Vec<_>>(),
                [InternalEvent::Event(Event::Key(expected.into()))],
                "{input:?} split at {split}"
            );
        }
    }
}

#[test]
fn discarded_nan_mouse_report_does_not_leak_its_delayed_suffix() {
    let report = b"\x1b[<16;NaN;NaNm";
    for split in 1..report.len() {
        let mut parser = Parser::default();
        parser.advance(&report[..split], true);
        for byte in &report[split..] {
            assert!(parser.next().is_none());
            assert_ne!(
                parser.discard_buffered_input(),
                InputDiscardStatus::Complete
            );
            parser.advance(std::slice::from_ref(byte), false);
        }
        assert!(parser.next().is_none());
        assert_eq!(
            parser.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
        parser.advance(b"x", false);
        assert_eq!(
            parser.collect::<Vec<_>>(),
            [InternalEvent::Event(Event::Key(KeyCode::Char('x').into()))],
            "split at {split}"
        );
    }
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn nan_mouse_report_inside_paste_remains_literal() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[200~\x1b[<16;NaN;NaNm\x1b[201~", false);
    assert_eq!(
        parser.collect::<Vec<_>>(),
        [InternalEvent::Event(Event::Paste(
            "\x1b[<16;NaN;NaNm".into()
        ))]
    );
}
