use super::*;
use crossterm_winapi::InputRecord;
use winapi::um::wincon::{INPUT_RECORD, KEY_EVENT};

#[test]
fn incomplete_response_replays_press_and_release_after_deadline() {
    let started_at = Instant::now();
    let press = key_record('\u{1b}', true);
    let release = key_record('\u{1b}', false);
    let mut response = TerminalResponseBuffer::default();

    assert_eq!(
        response.push(press.clone(), started_at),
        BufferedResponse::Pending
    );
    assert_eq!(
        response.push(release.clone(), started_at),
        BufferedResponse::Pending
    );
    assert_eq!(
        response.poll_timeout(None, started_at),
        Some(BUFFERED_RESPONSE_TIMEOUT)
    );
    assert_eq!(
        response.finish_expired(started_at + BUFFERED_RESPONSE_TIMEOUT),
        Some(vec![press, release])
    );
}

#[test]
fn non_response_replays_console_records_in_order() {
    let now = Instant::now();
    let records = [
        key_record('\u{1b}', true),
        key_record('\u{1b}', false),
        key_record('[', true),
        key_record('[', false),
        key_record('A', true),
    ];
    let mut response = TerminalResponseBuffer::default();

    for record in &records[..records.len() - 1] {
        assert_eq!(
            response.push(record.clone(), now),
            BufferedResponse::Pending
        );
    }
    assert_eq!(
        response.push(records.last().unwrap().clone(), now),
        BufferedResponse::Replay(records.to_vec())
    );
}

#[test]
fn complete_response_does_not_replay_console_records() {
    let now = Instant::now();
    let reply = "\u{1b}[0n";
    let mut response = TerminalResponseBuffer::default();

    for character in reply.chars().take(reply.len() - 1) {
        assert_eq!(
            response.push(key_record(character, true), now),
            BufferedResponse::Pending
        );
    }
    assert_eq!(
        response.push(key_record('n', true), now),
        BufferedResponse::Event(InternalEvent::OperatingStatus)
    );
}

fn key_record(character: char, key_down: bool) -> crossterm_winapi::KeyEventRecord {
    // INPUT_RECORD contains only integers and unions of integer records.
    let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
    record.EventType = KEY_EVENT;
    // Select the KEY_EVENT union member before conversion by crossterm_winapi.
    unsafe {
        let key = record.Event.KeyEvent_mut();
        key.bKeyDown = key_down.into();
        key.wRepeatCount = 1;
        key.wVirtualKeyCode = 0;
        *key.uChar.UnicodeChar_mut() = character as u16;
    }
    let InputRecord::KeyEvent(key) = InputRecord::from(record) else {
        panic!("expected a key input record");
    };
    key
}
