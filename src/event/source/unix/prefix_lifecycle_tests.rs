use std::{
    io::Write,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use crate::event::{
    Event, InputDiscardStatus, InternalEvent, KeyCode, KeyEvent, KeyModifiers, MouseEvent,
    MouseEventKind, internal::OscColorPayload, source::EventSource,
};
use crate::terminal::sys::file_descriptor::FileDesc;

use super::{Parser, UnixInternalEventSource};

fn source_with_escape() -> (UnixInternalEventSource, UnixStream) {
    let (reader, mut writer) = UnixStream::pair().unwrap();
    #[cfg(feature = "libc")]
    let reader = {
        use std::os::fd::IntoRawFd;
        FileDesc::new(reader.into_raw_fd(), true)
    };
    #[cfg(not(feature = "libc"))]
    let reader = FileDesc::Owned(reader.into());
    let mut source = UnixInternalEventSource::from_file_descriptor(reader).unwrap();
    writer.write_all(b"\x1b").unwrap();
    assert_eq!(read(&mut source), None);
    (source, writer)
}

fn read(source: &mut UnixInternalEventSource) -> Option<InternalEvent> {
    source.try_read(Some(Duration::ZERO)).unwrap()
}

fn character(c: char) -> InternalEvent {
    let modifiers = if c.is_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    InternalEvent::Event(Event::Key(KeyEvent::new(KeyCode::Char(c), modifiers)))
}

fn escape() -> InternalEvent {
    InternalEvent::Event(Event::Key(KeyCode::Esc.into()))
}

fn control_c() -> InternalEvent {
    InternalEvent::Event(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )))
}

#[test]
fn an_unfinished_tty_osc_expires_before_an_independent_control_c() {
    let (mut source, mut writer) = source_with_escape();
    writer.write_all(b"]").unwrap();
    assert_eq!(read(&mut source), None);
    source.parser.pending_escape_deadline = Some(Instant::now());
    let expired = [read(&mut source), read(&mut source)];
    writer.write_all(b"\x03").unwrap();
    let next = read(&mut source);
    assert_eq!(expired, [Some(escape()), Some(character(']'))]);
    assert_eq!(next, Some(control_c()));
    assert_eq!(read(&mut source), None);
}

#[test]
fn invalid_tty_csi_and_ss3_control_bytes_are_reprocessed_once() {
    for prefix in [b"[".as_slice(), b"[12;", b"O"] {
        let (mut source, mut writer) = source_with_escape();
        writer.write_all(prefix).unwrap();
        assert_eq!(read(&mut source), None);
        writer.write_all(b"\x03").unwrap();
        let actual: Vec<_> = (0..prefix.len() + 2).map(|_| read(&mut source)).collect();
        let mut expected = vec![Some(escape())];
        expected.extend(prefix.iter().map(|b| Some(character(*b as char))));
        expected.push(Some(control_c()));
        assert_eq!(actual, expected, "prefix {prefix:?}");
        assert_eq!(read(&mut source), None);
    }
}

#[test]
fn discarded_tty_control_prefix_expiry_preserves_the_next_control_c() {
    for prefix in [b']', b'[', b'O'] {
        let (mut source, mut writer) = source_with_escape();
        assert_ne!(
            source.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
        writer.write_all(&[prefix]).unwrap();
        assert_eq!(read(&mut source), None);
        source.parser.pending_escape_deadline = Some(Instant::now());
        writer.write_all(b"\x03").unwrap();
        assert_eq!(read(&mut source), Some(control_c()), "prefix {prefix:?}");
        assert_eq!(
            source.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
        assert_eq!(read(&mut source), None);
    }
}

#[test]
fn tty_prefix_extensions_never_replace_the_original_deadline() {
    for parts in [
        vec![b"]".as_slice(), b"10;unfinished", b"more"],
        vec![b"[".as_slice(), b"<65;", b"101;"],
        vec![b"O".as_slice()],
    ] {
        for discard in [false, true] {
            let mut parser = Parser::default();
            parser.buffer_tty_input(b"\x1b");
            let original = parser.pending_escape_deadline;
            if discard {
                parser.discard_buffered_input();
            }
            for part in &parts {
                parser.buffer_tty_input(part);
                assert_eq!(parser.next(), None);
                assert_eq!(
                    parser.pending_escape_deadline, original,
                    "{parts:?}, discarded={discard}"
                );
            }
        }
    }
}

#[test]
fn continuous_incomplete_tty_osc_input_cannot_extend_its_deadline() {
    for discard in [false, true] {
        let mut parser = Parser::default();
        parser.buffer_tty_input(b"\x1b");
        if discard {
            parser.discard_buffered_input();
        }
        parser.buffer_tty_input(b"]unfinished");
        parser.pending_escape_deadline = Some(Instant::now());
        parser.buffer_tty_input(b"more");
        if discard {
            assert_eq!(parser.next(), None);
        } else {
            assert_eq!(parser.next(), Some(escape()));
            let expected: Vec<_> = "]unfinishedmore".chars().map(character).collect();
            assert_eq!(parser.by_ref().collect::<Vec<_>>(), expected);
        }
        assert_eq!(parser.pending_escape_deadline, None);
        assert_eq!(
            parser.discard_buffered_input(),
            InputDiscardStatus::Complete
        );
        parser.buffer_tty_input(b"\x03");
        assert_eq!(parser.next(), Some(control_c()));
    }
}

#[test]
fn expired_fallback_preserves_utf8_and_does_not_rearm_embedded_escapes() {
    let mut parser = Parser::default();
    parser.buffer_tty_input(b"\x1b");
    parser.buffer_tty_input("]한글\x1b[\x1b]日本\x1b".as_bytes());
    parser.pending_escape_deadline = Some(Instant::now());
    let mut actual = vec![parser.finish_pending_escape().unwrap()];
    actual.extend(parser.by_ref());
    assert_eq!(
        actual,
        vec![
            escape(),
            character(']'),
            character('한'),
            character('글'),
            escape(),
            character('['),
            escape(),
            character(']'),
            character('日'),
            character('本'),
            escape()
        ]
    );
    assert!(parser.buffer.is_empty());
    assert_eq!(parser.pending_escape_deadline, None);
    assert_eq!(parser.finish_pending_escape(), None);
}

#[test]
fn eof_resolves_an_expired_tty_prefix_without_another_input_byte() {
    let (mut source, mut writer) = source_with_escape();
    writer.write_all(b"[").unwrap();
    assert_eq!(read(&mut source), None);
    source.parser.pending_escape_deadline = Some(Instant::now());
    drop(writer);
    assert_eq!(read(&mut source), Some(escape()));
    assert_eq!(read(&mut source), Some(character('[')));
    assert_eq!(read(&mut source), None);
}

#[test]
fn ready_complete_tty_frames_still_win_over_prefix_expiry() {
    for (prefix, suffix, expected) in [
        (
            b']',
            b"10;rgb:11/22/33\x07".as_slice(),
            InternalEvent::OscColor {
                slot: 10,
                payload: OscColorPayload::Rgb {
                    r: 0x11,
                    g: 0x22,
                    b: 0x33,
                },
            },
        ),
        (
            b'O',
            b"A".as_slice(),
            InternalEvent::Event(Event::Key(KeyCode::Up.into())),
        ),
        (
            b'[',
            b"<65;101;53M".as_slice(),
            InternalEvent::Event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 100,
                row: 52,
                modifiers: KeyModifiers::NONE,
            })),
        ),
    ] {
        let (mut source, mut writer) = source_with_escape();
        writer.write_all(&[prefix]).unwrap();
        assert_eq!(read(&mut source), None);
        source.parser.pending_escape_deadline = Some(Instant::now());
        writer.write_all(suffix).unwrap();
        assert_eq!(read(&mut source), Some(expected));
        assert_eq!(read(&mut source), None);
    }
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn confirmed_paste_bodies_do_not_inherit_tty_prefix_expiry() {
    let body = "text\x03\x1b]unfinished\x1b[日本".repeat(128);
    for discard in [false, true] {
        let mut parser = Parser::default();
        parser.buffer_tty_input(b"\x1b");
        parser.buffer_tty_input(b"[200~");
        assert_eq!(parser.pending_escape_deadline, None);
        if discard {
            assert_eq!(
                parser.discard_buffered_input(),
                InputDiscardStatus::BracketedPasteInProgress
            );
        }
        parser.buffer_tty_input(body.as_bytes());
        assert_eq!(parser.next(), None);
        assert_eq!(parser.finish_pending_escape(), None);
        parser.buffer_tty_input(b"\x1b[201~q");
        if !discard {
            assert_eq!(
                parser.next(),
                Some(InternalEvent::Event(Event::Paste(body.clone())))
            );
        }
        assert_eq!(parser.next(), Some(character('q')));
        assert_eq!(parser.next(), None);
    }
}

#[test]
fn external_prefix_quarantine_does_not_gain_tty_expiry() {
    let mut parser = Parser::default();
    parser.buffer_external_input(b"\x1b");
    assert!(parser.pending_escape_deadline.is_some());
    assert!(!parser.timed_tty_escape);
    parser.buffer_external_input(b"]unfinished");
    assert_eq!(parser.pending_escape_deadline, None);
    assert_eq!(
        parser.discard_buffered_input(),
        InputDiscardStatus::ControlSequenceInProgress
    );
    parser.buffer_external_input(b"\x03");
    assert_eq!(parser.next(), None);
    parser.buffer_external_input(b"\x07q");
    assert_eq!(parser.next(), Some(character('q')));
}
