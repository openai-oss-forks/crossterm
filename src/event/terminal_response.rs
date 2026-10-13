//! Decode color responses without interpreting ordinary console keys as VT input.
use super::internal::{InternalEvent, OscColorPayload};
use crate::style::Color;
use std::io;

pub(super) fn parse_osc(buffer: &[u8]) -> io::Result<Option<InternalEvent>> {
    debug_assert!(buffer.starts_with(b"\x1B]"));

    let Some(content_end) = osc_payload_end(buffer) else {
        return Ok(None);
    };

    let text = String::from_utf8_lossy(&buffer[2..content_end]);
    let mut parts = text.splitn(2, ';');

    let slot = match parts.next().unwrap_or("").parse::<u16>() {
        Ok(value) if value <= u8::MAX as u16 => value as u8,
        _ => return Ok(None),
    };

    if slot != 10 && slot != 11 {
        return Ok(None);
    }

    let payload_str = parts.next().unwrap_or("");
    let payload = match Color::from_osc_rgb(payload_str) {
        Some(Color::Rgb { r, g, b }) => OscColorPayload::Rgb { r, g, b },
        Some(_) => unreachable!("Color::from_osc_rgb returned non-RGB variant"),
        None => OscColorPayload::Unrecognized(payload_str.to_string()),
    };

    Ok(Some(InternalEvent::OscColor { slot, payload }))
}

fn osc_payload_end(buffer: &[u8]) -> Option<usize> {
    let mut idx = 2;
    while idx < buffer.len() {
        match buffer[idx] {
            0x07 => return Some(idx),
            0x1B => {
                if idx + 1 >= buffer.len() {
                    return None;
                }
                if buffer[idx + 1] == b'\\' {
                    return Some(idx);
                }
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

/// Decode the synthetic character records emitted by a Windows console for terminal replies.
/// A non-reply returns an error so the caller can replay the original input records unchanged.
#[cfg(any(windows, test))]
pub(super) fn parse_console_response(bytes: &[u8]) -> io::Result<Option<InternalEvent>> {
    if bytes == b"\x1b[0n" {
        return Ok(Some(InternalEvent::OperatingStatus));
    }
    if matches!(bytes, b"\x1b[?997;1n" | b"\x1b[?997;2n") {
        return Ok(Some(InternalEvent::ColorSchemeChanged));
    }
    if bytes.starts_with(b"\x1b]") {
        if let Some(event) = parse_osc(bytes)? {
            return Ok(Some(event));
        }
    }
    let partial = [
        b"\x1b]10;".as_slice(),
        b"\x1b]11;",
        b"\x1b[?997;1n",
        b"\x1b[?997;2n",
        b"\x1b[0n",
    ]
    .iter()
    .any(|prefix| {
        prefix.starts_with(bytes) || (prefix.starts_with(b"\x1b]") && bytes.starts_with(prefix))
    });
    if partial && bytes.len() < 1024 {
        return Ok(None);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "not a terminal response",
    ))
}

#[cfg(test)]
#[path = "terminal_response_tests.rs"]
mod tests;
