//! Quarantined input keeps completed operating-status metadata without releasing action keys.

use super::Parser;
use crate::event::{Event, InputDiscardStatus, InternalEvent, KeyCode};

#[test]
fn discarded_status_reply_preserves_its_boundary_at_every_split() {
    let reply = b"\x1b[0n";
    for split in 1..reply.len() {
        let mut parser = Parser::default();
        parser.advance(b"y", false);
        parser.advance(&reply[..split], true);
        assert_ne!(
            parser.discard_buffered_input(),
            InputDiscardStatus::Complete
        );

        for byte in &reply[split..] {
            parser.advance(std::slice::from_ref(byte), true);
            parser.discard_buffered_input();
        }
        assert_eq!(
            parser.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
        parser.advance(b"n", false);
        assert_eq!(
            parser.collect::<Vec<_>>(),
            vec![
                InternalEvent::OperatingStatus,
                InternalEvent::Event(Event::Key(KeyCode::Char('n').into())),
            ],
        );
    }
}

#[test]
fn discarded_other_csi_sequences_cannot_become_input_or_status() {
    for sequence in [b"\x1b[A".as_slice(), b"\x1b[49u", b"\x1b[00n", b"\x1b[0;n"] {
        for split in 1..sequence.len() {
            let mut parser = Parser::default();
            parser.advance(&sequence[..split], true);
            parser.discard_buffered_input();
            parser.advance(&sequence[split..], false);
            assert_eq!(parser.collect::<Vec<_>>(), vec![]);
        }
    }
}

#[test]
#[cfg(feature = "bracketed-paste")]
fn discarded_paste_keeps_literal_status_reply_quarantined() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[200~", true);
    parser.discard_buffered_input();
    parser.advance(b"\x1b[0n\x1b[201~", false);
    assert_eq!(parser.collect::<Vec<_>>(), vec![]);
}
