use super::*;
use crate::event::{Event, KeyCode, KeyEvent, KeyModifiers};

fn push(input: &mut VtInput, units: &str) {
    for unit in units.encode_utf16() {
        let record = win32_report::decode(format!("\x1b[0;0;{unit};1;0;1").as_bytes()).unwrap();
        input.push(record, &mut None);
    }
}

#[test]
#[cfg(feature = "bracketed-paste")]
fn mixed_whitespace_reports_stay_inside_the_literal_paste() {
    let mut input = VtInput::default();
    // Windows Terminal delivers down/up pairs for Return and Tab within an
    // otherwise literal UTF-16 paste. A standalone report in the text stays literal.
    push(&mut input, "\x1b[200~界 🦀__\x1b[13;28;13;1;0;1_");
    push(
        &mut input,
        "\x1b[13;28;13;0;0;1_b\x1b[9;15;9;1;0;1_\x1b[9;15;9;0;0;1_",
    );
    push(&mut input, "\x1b[13;28;13;1;0;1_\x1b[201");
    assert!(input.events.is_empty());
    push(&mut input, "~");
    assert_eq!(
        input.events,
        [InternalEvent::Event(Event::Paste(
            "界 🦀__\rb\t\x1b[13;28;13;1;0;1_".into()
        ))]
    );
}

#[test]
#[cfg(feature = "bracketed-paste")]
fn serialized_frame_unwraps_one_layer_and_keeps_a_following_modified_enter() {
    let mut input = VtInput::default();
    let text = "\r\n有\t🦀\x1b[13;28;13;1;0;1_\x1b[13;28;13;0;0;1_\n";
    for unit in format!("\x1b[200~{text}\x1b[201~").encode_utf16() {
        for down in [1, 0] {
            push(&mut input, &format!("\x1b[0;0;{unit};{down};0;1_"));
        }
    }
    push(&mut input, "\x1b[13;28;13;1;16;1_");
    assert_eq!(
        input.events,
        [
            InternalEvent::Event(Event::Paste(text.into())),
            InternalEvent::Event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::SHIFT
            ))),
        ]
    );
}

#[test]
fn ordinary_vt_navigation_is_decoded_after_a_win32_report() {
    let mut input = VtInput::default();
    push(&mut input, "\x1b[65;30;97;1;0;1_\x1b[1;5D\x1b[3~");
    assert_eq!(
        input.events,
        [
            InternalEvent::Event(Event::Key(KeyCode::Char('a').into())),
            InternalEvent::Event(Event::Key(KeyEvent::new(
                KeyCode::Left,
                KeyModifiers::CONTROL
            ))),
            InternalEvent::Event(Event::Key(KeyCode::Delete.into())),
        ]
    );
}

#[test]
#[cfg(feature = "bracketed-paste")]
fn discarding_a_partial_opener_quarantines_the_whole_frame() {
    let mut input = VtInput::default();
    push(&mut input, "\x1b[20");
    assert_eq!(
        input.discard(),
        InputDiscardStatus::ControlSequenceInProgress
    );
    push(&mut input, "0~old\r");
    assert_eq!(
        input.discard(),
        InputDiscardStatus::BracketedPasteInProgress
    );
    push(&mut input, "text\x1b[201~z");
    assert_eq!(
        input.events,
        [InternalEvent::Event(Event::Key(KeyCode::Char('z').into()))]
    );
}

#[test]
fn mode_replies_require_active_paste_and_restore_the_initial_reporting_mode() {
    use crate::event::Win32InputMode::{Disabled, Enabled};
    for (paste, win32, result) in [
        (1, 1, Some(Enabled)),
        (1, 2, Some(Disabled)),
        (2, 1, None),
        (1, 0, None),
    ] {
        let mut input = VtInput::default();
        input.start_probe();
        push(&mut input, &format!("\x1b[?9001;{win32}$yz"));
        assert!(input.probing());
        push(&mut input, &format!("\x1b[?2004;{paste}$y"));
        assert!(!input.probing());
        assert_eq!(input.probe_result(), result);
        assert_eq!(
            input.events,
            [InternalEvent::Event(Event::Key(KeyCode::Char('z').into()))]
        );
    }
}
