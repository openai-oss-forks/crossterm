//! Regression coverage for characters delivered through Windows console input records.

use super::*;
use crate::event::KeyEventState;
use crossterm_winapi::InputRecord;
use winapi::um::wincon::{INPUT_RECORD, KEY_EVENT};

fn assert_key_events(
    virtual_key_code: u16,
    character: char,
    control_key_state: u32,
    expected: Option<(KeyCode, KeyModifiers)>,
) {
    for (key_down, kind) in [(1, KeyEventKind::Press), (0, KeyEventKind::Release)] {
        // INPUT_RECORD contains only integers and unions of integer records.
        let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
        record.EventType = KEY_EVENT;
        // Select the KEY_EVENT union member before conversion by crossterm_winapi.
        unsafe {
            let key = record.Event.KeyEvent_mut();
            key.bKeyDown = key_down;
            key.wRepeatCount = 1;
            key.wVirtualKeyCode = virtual_key_code;
            *key.uChar.UnicodeChar_mut() = character as u16;
            key.dwControlKeyState = control_key_state;
        }
        let InputRecord::KeyEvent(key) = InputRecord::from(record) else {
            panic!("expected a key input record");
        };
        // KeyEvent equality normalizes Shift and ASCII case. Check the raw fields here.
        let actual = handle_key_event(key, &mut None).map(|event| match event {
            Event::Key(key) => (key.code, key.modifiers, key.kind, key.state),
            other => panic!("unexpected event {other:?}"),
        });
        assert_eq!(
            actual,
            expected.map(|(code, modifiers)| (code, modifiers, kind, KeyEventState::NONE)),
            "virtual key {virtual_key_code:#x}, character {character:?}, state {control_key_state:#x}"
        );
    }
}

#[test]
fn preserves_escape_and_literal_characters_from_console_records() {
    for (virtual_key_code, character, code) in [
        (0, '\u{1b}', KeyCode::Esc),
        (VK_ESCAPE as u16, '\u{1b}', KeyCode::Esc),
        (0, '[', KeyCode::Char('[')),
        (0, '1', KeyCode::Char('1')),
        (0, '3', KeyCode::Char('3')),
        (0, ';', KeyCode::Char(';')),
        (0, '2', KeyCode::Char('2')),
        (0, 'u', KeyCode::Char('u')),
    ] {
        for (state, modifiers) in [
            (0, KeyModifiers::NONE),
            (SHIFT_PRESSED, KeyModifiers::SHIFT),
        ] {
            assert_key_events(virtual_key_code, character, state, Some((code, modifiers)));
        }
    }
}

#[test]
fn ctrl_letters_use_the_control_code_or_the_virtual_key_when_nul() {
    let ctrl = KeyModifiers::CONTROL;
    let ctrl_shift = ctrl | KeyModifiers::SHIFT;
    for (control_code, upper) in (1..=26u8).zip(b'A'..=b'Z') {
        let vk = u16::from(upper);
        let upper = char::from(upper);
        let lower = upper.to_ascii_lowercase();
        for (state, letter, modifiers) in [
            (LEFT_CTRL_PRESSED, lower, ctrl),
            (RIGHT_CTRL_PRESSED | CAPSLOCK_ON, lower, ctrl),
            (LEFT_CTRL_PRESSED | SHIFT_PRESSED, upper, ctrl_shift),
            (
                RIGHT_CTRL_PRESSED | SHIFT_PRESSED | CAPSLOCK_ON,
                upper,
                ctrl_shift,
            ),
        ] {
            let expected = Some((KeyCode::Char(letter), modifiers));
            // Console records can carry the control code with or without a virtual key.
            assert_key_events(vk, char::from(control_code), state, expected);
            assert_key_events(0, char::from(control_code), state, expected);
            // Armenian Ctrl+Shift+letter carries the virtual key and a NUL character.
            assert_key_events(vk, '\0', state, expected);
        }
    }
}

#[test]
fn ctrl_preserves_printable_characters_altgr_and_special_keys() {
    let ctrl = KeyModifiers::CONTROL;
    let altgr = LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED;
    let ctrl_alt = ctrl | KeyModifiers::ALT;
    for letter in ['с', 'ց'] {
        assert_key_events(
            0x43,
            letter,
            LEFT_CTRL_PRESSED,
            Some((KeyCode::Char(letter), ctrl)),
        );
    }
    assert_key_events(0x41, 'ą', altgr, Some((KeyCode::Char('ą'), ctrl_alt)));
    assert_key_events(0, '\x03', altgr, None);
    for (vk, character, code) in [
        (VK_BACK, '\x08', KeyCode::Backspace),
        (VK_TAB, '\t', KeyCode::Tab),
        (VK_RETURN, '\r', KeyCode::Enter),
    ] {
        assert_key_events(vk as u16, character, LEFT_CTRL_PRESSED, Some((code, ctrl)));
    }
}
