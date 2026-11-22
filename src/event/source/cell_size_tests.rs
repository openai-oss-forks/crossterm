//! Cell-size replies use the same live Unix reader as keys and bracketed paste.
#![cfg(test)]

use super::tests::source_with_input;
use crate::event::{Event, InternalEvent, KeyCode, KeyEvent, KeyModifiers, source::EventSource};
use std::{io::Write, time::Duration};

fn key(c: char) -> InternalEvent {
    InternalEvent::Event(Event::Key(KeyEvent::new(
        KeyCode::Char(c),
        if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        },
    )))
}

#[test]
fn live_cell_size_report_survives_every_split() {
    let report = b"\x1b[6;20;10t";
    for split in 0..report.len() {
        let (mut source, mut writer) = source_with_input();
        writer.write_all(&report[..split]).unwrap();
        assert_eq!(
            source.try_read(Some(Duration::from_millis(1))).unwrap(),
            None
        );
        writer.write_all(&report[split..]).unwrap();
        writer.write_all(b"q").unwrap();
        assert_eq!(
            source.try_read(Some(Duration::from_millis(1))).unwrap(),
            Some(InternalEvent::Event(Event::CellSize(10, 20))),
            "split {split}"
        );
        assert_eq!(
            source.try_read(Some(Duration::from_millis(1))).unwrap(),
            Some(key('q'))
        );
    }
}

#[test]
fn live_cell_size_invalid_reports_leave_the_next_key() {
    for report in [
        b"\x1b[6;0;10t".as_slice(),
        b"\x1b[6;20;0t",
        b"\x1b[6;65536;10t",
        b"\x1b[6;;10t",
        b"\x1b[6;20;10;1t",
    ] {
        for split in 0..report.len() {
            let (mut source, mut writer) = source_with_input();
            writer.write_all(&report[..split]).unwrap();
            assert_eq!(
                source.try_read(Some(Duration::from_millis(1))).unwrap(),
                None
            );
            writer.write_all(&report[split..]).unwrap();
            writer.write_all(b"q").unwrap();
            let event = source.try_read(Some(Duration::from_millis(1))).unwrap();
            if matches!(event, Some(InternalEvent::Event(Event::CellSize(_, _)))) {
                assert_eq!(
                    source.try_read(Some(Duration::from_millis(1))).unwrap(),
                    Some(key('q'))
                );
            } else {
                assert_eq!(event, Some(key('q')), "report {report:?}, split {split}");
            }
        }
    }
}

#[test]
fn partial_cell_size_report_does_not_consume_typing_or_escape() {
    let report = b"\x1b[6;20;10t";
    for split in 3..report.len() {
        let prefix = &report[..split];
        let (mut source, mut writer) = source_with_input();
        writer.write_all(prefix).unwrap();
        assert_eq!(
            source.try_read(Some(Duration::from_millis(1))).unwrap(),
            None
        );
        writer.write_all(b"qAQZ\x1b").unwrap();
        for c in "qAQZ".chars() {
            assert_eq!(
                source.try_read(Some(Duration::from_millis(1))).unwrap(),
                Some(key(c))
            );
        }
        assert_eq!(
            source.try_read(Some(Duration::from_millis(100))).unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
    }
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn partial_cell_size_report_preserves_exact_paste_at_every_split() {
    let paste = b"\x1b[200~draft\r\n\x1b[6;20;10t\x1b[201~";
    for split in 0..paste.len() {
        let (mut source, mut writer) = source_with_input();
        writer.write_all(b"\x1b[6;20;").unwrap();
        assert_eq!(
            source.try_read(Some(Duration::from_millis(1))).unwrap(),
            None
        );
        writer.write_all(&paste[..split]).unwrap();
        assert_eq!(
            source.try_read(Some(Duration::from_millis(1))).unwrap(),
            None
        );
        writer.write_all(&paste[split..]).unwrap();
        assert_eq!(
            source.try_read(Some(Duration::from_millis(1))).unwrap(),
            Some(InternalEvent::Event(Event::Paste(
                "draft\r\n\x1b[6;20;10t".into()
            ))),
            "split {split}"
        );
    }
}

#[test]
fn cell_size_handling_preserves_escape_and_modified_page_down() {
    let (mut source, mut writer) = source_with_input();
    writer.write_all(b"\x1b\x1b[6;20;10t\x1b[6;5~").unwrap();
    for expected in [
        InternalEvent::Event(Event::Key(KeyCode::Esc.into())),
        InternalEvent::Event(Event::CellSize(10, 20)),
        InternalEvent::Event(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::CONTROL,
        ))),
    ] {
        assert_eq!(
            source.try_read(Some(Duration::from_millis(1))).unwrap(),
            Some(expected)
        );
    }
}
