use std::{
    io::Write,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use crate::event::{
    Event, InputDiscardStatus, InternalEvent, KeyCode, KeyEvent, KeyModifiers, MouseEvent,
    MouseEventKind, source::EventSource,
};
use crate::terminal::sys::file_descriptor::FileDesc;

use super::UnixInternalEventSource;

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
fn a_real_escape_preserves_the_next_terminal_report() {
    for (bytes, expected) in [
        (
            b"\x1b[<65;101;53M".as_slice(),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 100,
                row: 52,
                modifiers: KeyModifiers::empty(),
            }),
        ),
        (b"\x1b[A".as_slice(), Event::Key(KeyCode::Up.into())),
        (b"\x1b[I".as_slice(), Event::FocusGained),
        (b"\x1b[27u".as_slice(), Event::Key(KeyCode::Esc.into())),
    ] {
        let (mut source, mut writer) = source_with_input();
        writer.write_all(b"\x1b").unwrap();
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
        writer.write_all(bytes).unwrap();
        assert_eq!(
            source.try_read(Some(Duration::ZERO)).unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
        assert_eq!(
            source.try_read(Some(Duration::ZERO)).unwrap(),
            Some(InternalEvent::Event(expected))
        );
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    }
}

#[test]
fn two_legacy_escape_presses_are_two_events() {
    let (mut source, mut writer) = source_with_input();
    writer.write_all(b"\x1b").unwrap();
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    writer.write_all(b"\x1b").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
    );
    source.parser.pending_escape_deadline = Some(Instant::now());
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
    );
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
}

#[test]
fn discarding_a_tty_escape_finishes_before_a_one_second_input_boundary() {
    let (mut source, mut writer) = source_with_input();
    writer.write_all(b"\x1b").unwrap();
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    assert_ne!(
        source.discard_buffered_input(),
        InputDiscardStatus::Complete
    );
    let started = Instant::now();
    assert_eq!(source.try_read(Some(Duration::from_secs(2))).unwrap(), None);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "discarded Esc waited {:?}",
        started.elapsed()
    );
    assert_eq!(
        source.discard_buffered_input(),
        InputDiscardStatus::Complete
    );
    writer.write_all(b"\x03").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))))
    );
}

#[test]
fn discarding_a_tty_escape_still_quarantines_its_in_window_suffix() {
    let (mut source, mut writer) = source_with_input();
    writer.write_all(b"\x1b").unwrap();
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    assert_ne!(
        source.discard_buffered_input(),
        InputDiscardStatus::Complete
    );
    writer.write_all(b"[<65;101;53M").unwrap();
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    assert_eq!(
        source.discard_buffered_input(),
        InputDiscardStatus::Complete
    );
    writer.write_all(b"q").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyCode::Char('q').into())))
    );
}
