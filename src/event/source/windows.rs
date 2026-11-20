use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm_winapi::{Console, Handle, InputRecord};

use crate::event::{
    Event,
    sys::windows::{parse::MouseButtonsPressed, poll::WinApiPoll},
};

#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    internal::InternalEvent,
    source::EventSource,
    sys::windows::parse::{handle_key_event, handle_mouse_event},
    timeout::PollTimeout,
};

pub(crate) struct WindowsEventSource {
    console: Console,
    response: TerminalResponseBuffer,
    ready: VecDeque<InternalEvent>,
    poll: WinApiPoll,
    surrogate_buffer: Option<u16>,
    mouse_buttons_pressed: MouseButtonsPressed,
}

impl WindowsEventSource {
    pub(crate) fn new() -> std::io::Result<WindowsEventSource> {
        let console = Console::from(Handle::current_in_handle()?);
        Ok(WindowsEventSource {
            console,
            response: TerminalResponseBuffer::default(),
            ready: VecDeque::new(),

            #[cfg(not(feature = "event-stream"))]
            poll: WinApiPoll::new(),
            #[cfg(feature = "event-stream")]
            poll: WinApiPoll::new()?,

            surrogate_buffer: None,
            mouse_buttons_pressed: MouseButtonsPressed::default(),
        })
    }
}

impl WindowsEventSource {
    // Terminal replies use synthetic Unicode records; retain native virtual-key handling for
    // navigation, modifiers, UTF-16, and mouse input. Incomplete replies are bounded.
    fn handle_record(&mut self, record: crossterm_winapi::KeyEventRecord) {
        match self.response.push(record, Instant::now()) {
            BufferedResponse::Pending => {}
            BufferedResponse::Event(event) => self.ready.push_back(event),
            BufferedResponse::Input(record) => self.queue_record(record),
            BufferedResponse::Replay(records) => self.replay(records),
        }
    }

    fn replay(&mut self, records: Vec<crossterm_winapi::KeyEventRecord>) {
        for record in records {
            self.queue_record(record);
        }
    }

    fn queue_record(&mut self, record: crossterm_winapi::KeyEventRecord) {
        if let Some(event) = handle_key_event(record, &mut self.surrogate_buffer) {
            self.ready.push_back(InternalEvent::Event(event));
        }
    }
}

const BUFFERED_RESPONSE_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Default)]
struct TerminalResponseBuffer {
    records: Vec<crossterm_winapi::KeyEventRecord>,
    deadline: Option<Instant>,
}

#[derive(Debug, PartialEq)]
enum BufferedResponse {
    Pending,
    Event(InternalEvent),
    Input(crossterm_winapi::KeyEventRecord),
    Replay(Vec<crossterm_winapi::KeyEventRecord>),
}

impl TerminalResponseBuffer {
    fn push(&mut self, record: crossterm_winapi::KeyEventRecord, now: Instant) -> BufferedResponse {
        let synthetic_ascii = record.virtual_key_code == 0 && record.u_char <= 127;
        if self.records.is_empty() {
            if !synthetic_ascii || !record.key_down || record.u_char != 27 {
                return BufferedResponse::Input(record);
            }
            self.deadline = Some(now + BUFFERED_RESPONSE_TIMEOUT);
        } else if !synthetic_ascii {
            self.records.push(record);
            return BufferedResponse::Replay(self.take());
        }

        self.records.push(record);
        let bytes: Vec<_> = self
            .records
            .iter()
            .filter(|record| record.key_down)
            .map(|record| record.u_char as u8)
            .collect();
        match crate::event::terminal_response::parse_console_response(&bytes) {
            Ok(Some(event)) => {
                self.take();
                BufferedResponse::Event(event)
            }
            Ok(None) => BufferedResponse::Pending,
            Err(_) => BufferedResponse::Replay(self.take()),
        }
    }

    fn poll_timeout(&self, timeout: Option<Duration>, now: Instant) -> Option<Duration> {
        let Some(deadline) = self.deadline else {
            return timeout;
        };
        let remaining = deadline.saturating_duration_since(now);
        Some(timeout.map_or(remaining, |timeout| timeout.min(remaining)))
    }

    fn finish_expired(&mut self, now: Instant) -> Option<Vec<crossterm_winapi::KeyEventRecord>> {
        (self.deadline? <= now).then(|| self.take())
    }

    fn take(&mut self) -> Vec<crossterm_winapi::KeyEventRecord> {
        self.deadline = None;
        std::mem::take(&mut self.records)
    }
}

impl EventSource for WindowsEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> std::io::Result<Option<InternalEvent>> {
        let poll_timeout = PollTimeout::new(timeout);

        loop {
            if let Some(event) = self.ready.pop_front() {
                return Ok(Some(event));
            }
            let timeout = self
                .response
                .poll_timeout(poll_timeout.leftover(), Instant::now());
            if self.poll.poll(timeout)?.is_some() {
                let number = self.console.number_of_console_input_events()?;
                if number != 0 {
                    let event = match self.console.read_single_input_event()? {
                        InputRecord::KeyEvent(record) => {
                            self.handle_record(record);
                            None
                        }
                        InputRecord::MouseEvent(record) => {
                            let mouse_event =
                                handle_mouse_event(record, &self.mouse_buttons_pressed);
                            self.mouse_buttons_pressed = MouseButtonsPressed {
                                left: record.button_state.left_button(),
                                right: record.button_state.right_button(),
                                middle: record.button_state.middle_button(),
                            };

                            mouse_event
                        }
                        InputRecord::WindowBufferSizeEvent(record) => {
                            // windows starts counting at 0, unix at 1, add one to replicate unix behaviour.
                            Some(Event::Resize(
                                (record.size.x as i32 + 1) as u16,
                                (record.size.y as i32 + 1) as u16,
                            ))
                        }
                        InputRecord::FocusEvent(record) => {
                            let event = if record.set_focus {
                                Event::FocusGained
                            } else {
                                Event::FocusLost
                            };
                            Some(event)
                        }
                        _ => None,
                    };

                    if let Some(event) = event {
                        return Ok(Some(InternalEvent::Event(event)));
                    }
                }
            } else if let Some(records) = self.response.finish_expired(Instant::now()) {
                self.replay(records);
                continue;
            }

            if poll_timeout.elapsed() {
                return Ok(None);
            }
        }
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.poll.waker()
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
