use super::UnixInternalEventSource;
use crate::event::{
    Event, InternalEvent, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
    source::EventSource,
};
use crate::terminal::sys::file_descriptor::FileDesc;
use std::{
    io::Write,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

fn source_with_input() -> (UnixInternalEventSource, UnixStream) {
    let (reader, writer) = UnixStream::pair().unwrap();
    #[cfg(feature = "libc")]
    let reader = {
        use std::os::fd::IntoRawFd;
        FileDesc::new(reader.into_raw_fd(), true)
    };
    #[cfg(not(feature = "libc"))]
    let reader = FileDesc::Owned(reader.into());
    (
        UnixInternalEventSource::from_file_descriptor(reader).unwrap(),
        writer,
    )
}

#[test]
fn every_fragmentation_of_a_mouse_report_is_one_mouse_event() {
    let report = b"\x1b[<65;101;53M";
    let expected = InternalEvent::Event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 100,
        row: 52,
        modifiers: KeyModifiers::empty(),
    }));
    for partitions in 0..(1usize << (report.len() - 1)) {
        let (mut source, mut writer) = source_with_input();
        let mut start = 0;
        for end in 1..=report.len() {
            if end == report.len() || partitions & (1 << (end - 1)) != 0 {
                writer.write_all(&report[start..end]).unwrap();
                let actual = source.try_read(Some(Duration::ZERO)).unwrap();
                if end == report.len() {
                    assert_eq!(actual, Some(expected.clone()), "partition {partitions}");
                } else {
                    assert_eq!(
                        actual, None,
                        "premature event in partition {partitions}, byte {end}"
                    );
                }
                start = end;
            }
        }
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    }
}

#[test]
fn a_standalone_tty_escape_expires_once_without_waiting_for_another_read() {
    let (mut source, mut writer) = source_with_input();
    writer.write_all(b"\x1b").unwrap();
    for _ in 0..3 {
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    }
    assert!(source.parser.pending_escape_deadline.is_some());
    source.parser.pending_escape_deadline = Some(Instant::now());
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
    );
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
}

#[test]
fn a_ready_continuation_wins_even_when_the_escape_deadline_has_passed() {
    let (mut source, mut writer) = source_with_input();
    writer.write_all(b"\x1b").unwrap();
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    source.parser.pending_escape_deadline = Some(Instant::now());
    writer.write_all(b"[A").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyCode::Up.into())))
    );
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
}

#[test]
fn fragmented_keys_focus_and_utf8_keep_their_meaning() {
    let cases = [
        (b"\x1b[A".as_slice(), Event::Key(KeyCode::Up.into())),
        (b"\x1b[27u".as_slice(), Event::Key(KeyCode::Esc.into())),
        (
            b"\x1bx".as_slice(),
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)),
        ),
        (b"\x1b[I".as_slice(), Event::FocusGained),
        (b"\x1b[O".as_slice(), Event::FocusLost),
        ("😀".as_bytes(), Event::Key(KeyCode::Char('😀').into())),
    ];
    for (bytes, expected) in cases {
        for split in 1..bytes.len() {
            let (mut source, mut writer) = source_with_input();
            writer.write_all(&bytes[..split]).unwrap();
            assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
            writer.write_all(&bytes[split..]).unwrap();
            assert_eq!(
                source.try_read(Some(Duration::ZERO)).unwrap(),
                Some(InternalEvent::Event(expected.clone()))
            );
            assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
        }
    }
    let (mut source, mut writer) = source_with_input();
    writer.write_all(b"\x03").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))))
    );
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn fragmented_paste_preserves_protocol_looking_text() {
    let bytes = b"\x1b[200~[<65;101;53M\x1b[201~";
    for split in 1..bytes.len() {
        let (mut source, mut writer) = source_with_input();
        writer.write_all(&bytes[..split]).unwrap();
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
        writer.write_all(&bytes[split..]).unwrap();
        assert_eq!(
            source.try_read(Some(Duration::ZERO)).unwrap(),
            Some(InternalEvent::Event(Event::Paste(
                "[<65;101;53M".to_owned()
            )))
        );
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    }
}
