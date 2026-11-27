//! Regression coverage for UTF-16 pairs split across native Windows key records.
#![cfg(test)]

use super::*;

fn key_record(virtual_key_code: u16, unit: u16, key_down: bool) -> KeyEventRecord {
    // INPUT_RECORD contains only integers and unions of integer records.
    let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
    record.EventType = KEY_EVENT;
    let InputRecord::KeyEvent(key) = InputRecord::from(record) else {
        panic!("expected a key input record");
    };
    KeyEventRecord {
        virtual_key_code,
        u_char: unit,
        key_down,
        repeat_count: 1,
        ..key
    }
}

#[test]
fn pairs_pasted_surrogates_across_key_releases() {
    let mut buffer = None;
    for ch in ['𠀀', '😀', '🔵'] {
        let mut units = [0; 2];
        ch.encode_utf16(&mut units);
        let [high, low] = units;
        let events =
            [(high, true), (high, false), (low, true), (low, false)].map(|(unit, down)| {
                handle_key_event(
                    key_record(/* virtual_key_code */ 0, unit, down),
                    &mut buffer,
                )
            });
        let press = Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        assert_eq!(events, [None, None, Some(press), None]);
    }
}

#[test]
fn pairs_alt_code_surrogates_on_key_release() {
    let mut buffer = None;
    let events = [0xd83d, 0xdd35].map(|unit| {
        let release = key_record(VK_MENU as u16, unit, /* key_down */ false);
        handle_key_event(release, &mut buffer)
    });
    let press = Event::Key(KeyEvent::new(KeyCode::Char('🔵'), KeyModifiers::NONE));
    assert_eq!(events, [None, Some(press)]);
}
