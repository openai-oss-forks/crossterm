//! Recover native keyboard records carried by the Win32 input protocol.
//!
//! A report is CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _. Feed its fields back through
//! the native record parser so modifier, layout and Alt-code behavior is shared.

use crossterm_winapi::{InputRecord, KeyEventRecord};
use winapi::um::wincon::{INPUT_RECORD, KEY_EVENT};

pub(super) fn decode(prefix: &[u8]) -> Option<KeyEventRecord> {
    let params = prefix.strip_prefix(b"\x1b[")?;
    // Four WORDs, a boolean, a DWORD and five separators.
    if params.len() > 36 {
        return None;
    }
    let mut fields = std::str::from_utf8(params).ok()?.split(';');
    let vk = fields.next()?.parse().ok()?;
    let scan = fields.next()?.parse().ok()?;
    let unit = fields.next()?.parse().ok()?;
    let down = match fields.next()? {
        "0" => 0,
        "1" => 1,
        _ => return None,
    };
    let state = fields.next()?.parse().ok()?;
    let repeat = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    // KeyEventRecord's control-state constructor is private. Use the same
    // conversion as ReadConsoleInputW. The discriminant selects the key union.
    let mut raw: INPUT_RECORD = unsafe { std::mem::zeroed() };
    raw.EventType = KEY_EVENT;
    unsafe {
        let key = raw.Event.KeyEvent_mut();
        key.wVirtualKeyCode = vk;
        key.wVirtualScanCode = scan;
        *key.uChar.UnicodeChar_mut() = unit;
        key.bKeyDown = down;
        key.dwControlKeyState = state;
        key.wRepeatCount = repeat;
    }
    match InputRecord::from(raw) {
        InputRecord::KeyEvent(key) => Some(key),
        _ => unreachable!(),
    }
}

/// Some hosts insert paired whitespace reports among the literal characters
/// of a paste. Collapse only a matching press and release. Never dispatch
/// those keys, or interpret report syntax in an already decoded Win32 record.
#[cfg(feature = "bracketed-paste")]
pub(super) fn finish_whitespace_pair(buffer: &mut Vec<u8>) -> bool {
    // At most two 39-byte reports. The current '_' hasn't been appended yet.
    let tail = buffer.len().saturating_sub(78);
    let Some(pos) = buffer[tail..].iter().rposition(|&b| b == 0x1b) else {
        return false;
    };
    let release_at = tail + pos;
    let Some(mut up) = decode(&buffer[release_at..]) else {
        return false;
    };
    if up.key_down || up.repeat_count != 1 || !matches!(up.u_char, 9 | 10 | 13) {
        return false;
    }
    let Some(first) = buffer[tail..release_at].strip_suffix(b"_") else {
        return false;
    };
    let Some(down_at) = first.iter().rposition(|&b| b == 0x1b) else {
        return false;
    };
    up.key_down = true;
    if decode(&first[down_at..]).as_ref() == Some(&up) {
        buffer.truncate(tail + down_at);
        buffer.push(up.u_char as u8);
        return true;
    }
    false
}
