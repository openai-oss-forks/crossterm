//! Decode framed VT input from a Windows console record queue.
//!
//! With Win32 keyboard reporting enabled, ConPTY can serialize every record
//! (including paste delimiters), or deliver a literal VT frame with serialized
//! tabs and returns. Unwrap at most one transport layer. Inside either frame,
//! the text never visits the keyboard handler.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm_winapi::KeyEventRecord;
use winapi::um::winuser::VK_MENU;

use super::parse::handle_key_event;
use super::vt_parse::parse_event;
use super::win32_report;
use crate::event::InputDiscardStatus;
use crate::event::internal::InternalEvent;

const ESCAPE_TIMEOUT: Duration = Duration::from_millis(20);
const ESC: u16 = 0x1b;
const PASTE_START: &[u8] = b"\x1b[200~";

#[derive(Default)]
struct ModeReports {
    paste: Option<u8>,
    win32: Option<u8>,
}

#[derive(Default)]
pub(crate) struct VtInput {
    probe: Option<ModeReports>,
    // Bounded prefix of a possible Win32 keyboard report on the wire.
    report: Vec<u8>,
    // Input to the same VT event parser used on Unix, after unwrapping reports.
    buffer: Vec<u8>,
    high_surrogate: Option<u16>,
    literal_paste: bool,
    discarding: bool,
    escape_deadline: Option<Instant>,
    pub(crate) events: VecDeque<InternalEvent>,
}

impl VtInput {
    pub(crate) fn start_probe(&mut self) {
        self.probe = Some(ModeReports::default());
    }

    pub(crate) fn probing(&self) -> bool {
        self.probe
            .as_ref()
            .is_some_and(|s| s.paste.is_none() || s.win32.is_none())
    }

    pub(crate) fn probe_result(&self) -> Option<crate::event::Win32InputMode> {
        use crate::event::Win32InputMode;
        let probe = self.probe.as_ref()?;
        match (probe.paste?, probe.win32?) {
            (b'1' | b'3', b'1') => Some(Win32InputMode::Enabled),
            (b'1' | b'3', b'2') => Some(Win32InputMode::Disabled),
            _ => None,
        }
    }

    pub(crate) fn push(&mut self, key: KeyEventRecord, surrogates: &mut Option<u16>) {
        if key.key_down && key.virtual_key_code == 0 {
            for _ in 0..key.repeat_count {
                self.push_wire_unit(key.u_char, surrogates);
            }
        } else {
            self.push_record(key, surrogates);
        }
        let escape = self.report == b"\x1b" || self.report.is_empty() && self.buffer == b"\x1b";
        self.escape_deadline =
            (escape && !self.in_paste()).then(|| Instant::now() + ESCAPE_TIMEOUT);
        self.discarding &= self.is_buffering();
    }

    fn push_wire_unit(&mut self, unit: u16, surrogates: &mut Option<u16>) {
        if self.literal_paste {
            #[cfg(feature = "bracketed-paste")]
            if unit == u16::from(b'_') && win32_report::finish_whitespace_pair(&mut self.buffer) {
                return;
            }
            self.push_utf16(unit);
            return;
        }

        if !self.report.is_empty() {
            if unit == u16::from(b'_') {
                if let Some(key) = win32_report::decode(&self.report) {
                    self.report.clear();
                    self.push_record(key, surrogates);
                    return;
                }
            }
            let next = u8::try_from(unit).ok().filter(|b| {
                if self.report.len() == 1 {
                    *b == b'['
                } else {
                    self.report.len() < 38 && (b.is_ascii_digit() || *b == b';')
                }
            });
            if let Some(byte) = next {
                self.report.push(byte);
                return;
            }
            self.flush_report_prefix();
        }

        if unit == ESC {
            self.report.push(0x1b);
        } else {
            self.push_utf16(unit);
            self.literal_paste = self.buffer == PASTE_START && self.in_paste();
        }
    }

    fn flush_report_prefix(&mut self) {
        for i in 0..self.report.len() {
            self.push_utf16(u16::from(self.report[i]));
        }
        self.report.clear();
    }

    fn push_record(&mut self, key: KeyEventRecord, surrogates: &mut Option<u16>) {
        if self.in_paste() || !self.buffer.is_empty() || key.virtual_key_code == 0 {
            let alt_code = !key.key_down && i32::from(key.virtual_key_code) == VK_MENU;
            if (key.key_down || alt_code) && key.u_char != 0 {
                for _ in 0..key.repeat_count {
                    self.push_utf16(key.u_char);
                }
            }
            return;
        }
        if let Some(event) = handle_key_event(key, surrogates) {
            self.emit(InternalEvent::Event(event));
        }
    }

    fn in_paste(&self) -> bool {
        cfg!(feature = "bracketed-paste") && self.buffer.starts_with(PASTE_START)
    }

    pub(crate) fn is_buffering(&self) -> bool {
        !self.buffer.is_empty() || !self.report.is_empty()
    }

    fn emit(&mut self, event: InternalEvent) {
        if !self.discarding && matches!(event, InternalEvent::Event(_)) {
            self.events.push_back(event);
        }
    }

    fn push_utf16(&mut self, unit: u16) {
        if let Some(high) = self.high_surrogate.take() {
            if (0xdc00..=0xdfff).contains(&unit) {
                self.push_char(char::decode_utf16([high, unit]).next().unwrap().unwrap());
                return;
            }
            self.push_char(char::REPLACEMENT_CHARACTER);
        }
        if (0xd800..=0xdbff).contains(&unit) {
            self.high_surrogate = Some(unit);
        } else {
            self.push_char(char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER));
        }
    }

    fn push_char(&mut self, ch: char) {
        let mut bytes = [0; 4];
        self.buffer
            .extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
        if self.probing() {
            let probe = self.probe.as_mut().unwrap();
            if let [
                0x1b,
                b'[',
                b'?',
                a,
                b,
                c,
                d,
                b';',
                mode @ b'0'..=b'4',
                b'$',
                b'y',
            ] = self.buffer.as_slice()
            {
                match &[*a, *b, *c, *d] {
                    b"2004" => probe.paste = Some(*mode),
                    b"9001" => probe.win32 = Some(*mode),
                    _ => {}
                }
                self.buffer.clear();
                return;
            }
        }
        match parse_event(&self.buffer, true) {
            Ok(Some(event)) => self.emit(event),
            Ok(None) => {
                let finished_osc = self.buffer.starts_with(b"\x1b]")
                    && (ch == '\x07' || self.buffer.ends_with(b"\x1b\\"));
                let finished_csi = self.buffer.len() > 2
                    && self.buffer.starts_with(b"\x1b[")
                    && ('@'..='~').contains(&ch)
                    && !self.buffer.starts_with(b"\x1b[M")
                    && !self.in_paste();
                if !finished_osc && !finished_csi {
                    return;
                }
            }
            Err(_) => {}
        }
        self.buffer.clear();
        self.literal_paste = false;
    }

    pub(crate) fn timeout(&self, timeout: Option<Duration>) -> Option<Duration> {
        self.escape_deadline.map_or(timeout, |deadline| {
            let left = deadline.saturating_duration_since(Instant::now());
            Some(timeout.map_or(left, |t| t.min(left)))
        })
    }

    // Check deadlines only after draining queued records. A descheduled
    // reader must not turn a queued paste opener into the Escape shortcut.
    pub(crate) fn flush_escape(&mut self) {
        if self.escape_deadline.is_some_and(|t| Instant::now() >= t) {
            self.escape_deadline = None;
            self.flush_report_prefix();
            if let Ok(Some(event)) = parse_event(&self.buffer, false) {
                self.emit(event);
            }
            self.buffer.clear();
            self.discarding = false;
        }
    }

    pub(crate) fn discard(&mut self) -> InputDiscardStatus {
        self.events.clear();
        self.high_surrogate = None;
        self.discarding = self.is_buffering();
        if self.in_paste() {
            InputDiscardStatus::BracketedPasteInProgress
        } else if self.discarding {
            InputDiscardStatus::ControlSequenceInProgress
        } else {
            InputDiscardStatus::Complete
        }
    }
}

#[cfg(test)]
#[path = "vt_input_tests.rs"]
mod tests;
