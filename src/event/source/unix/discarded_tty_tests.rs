use std::{
    io::Write,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use crate::event::{
    Event, InputDiscardStatus, InternalEvent, KeyCode, KeyEvent, KeyModifiers, source::EventSource,
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
fn a_readable_discarded_tty_suffix_wins_over_the_elapsed_deadline() {
    for expired_before_discard in [true, false] {
        let (mut source, mut writer) = source_with_input();
        writer.write_all(b"\x1b").unwrap();
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
        writer.write_all(b"[<65;101;53M").unwrap();
        if expired_before_discard {
            source.parser.pending_escape_deadline = Some(Instant::now());
        }
        assert_ne!(
            source.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
        if !expired_before_discard {
            source.parser.pending_escape_deadline = Some(Instant::now());
        }
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
}

#[test]
fn a_fresh_escape_after_a_discarded_tty_escape_starts_a_new_report() {
    for (bytes, expected) in [
        (b"\x1b[A".as_slice(), Event::Key(KeyCode::Up.into())),
        (b"\x1b[I".as_slice(), Event::FocusGained),
        (b"\x1b[27u".as_slice(), Event::Key(KeyCode::Esc.into())),
    ] {
        let (mut source, mut writer) = source_with_input();
        writer.write_all(b"\x1b").unwrap();
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
        assert_ne!(
            source.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
        writer.write_all(bytes).unwrap();
        assert_eq!(
            source.try_read(Some(Duration::ZERO)).unwrap(),
            Some(InternalEvent::Event(expected))
        );
        assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    }
}

#[test]
fn an_independent_control_c_after_the_discard_deadline_is_preserved() {
    let (mut source, mut writer) = source_with_input();
    writer.write_all(b"\x1b").unwrap();
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
    assert_ne!(
        source.discard_buffered_input(),
        InputDiscardStatus::Complete
    );
    source.parser.pending_escape_deadline = Some(Instant::now());
    writer.write_all(b"\x03").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))))
    );
}
