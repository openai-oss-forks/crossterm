use std::collections::VecDeque;
use std::time::Duration;

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
    response: Vec<crossterm_winapi::KeyEventRecord>,
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
            response: Vec::new(),
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
        if record.virtual_key_code == 0
            && record.key_down
            && record.u_char <= 127
            && (record.u_char == 27 || !self.response.is_empty())
        {
            self.response.push(record);
            let bytes: Vec<_> = self
                .response
                .iter()
                .map(|record| record.u_char as u8)
                .collect();
            match crate::event::terminal_response::parse_console_response(&bytes) {
                Ok(Some(event)) => {
                    self.response.clear();
                    self.ready.push_back(event);
                    return;
                }
                Ok(None) => return,
                Err(_) => {}
            }
            for record in self.response.drain(..) {
                if let Some(event) = handle_key_event(record, &mut self.surrogate_buffer) {
                    self.ready.push_back(InternalEvent::Event(event));
                }
            }
        } else if let Some(event) = handle_key_event(record, &mut self.surrogate_buffer) {
            self.ready.push_back(InternalEvent::Event(event));
        }
    }
}

impl EventSource for WindowsEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> std::io::Result<Option<InternalEvent>> {
        let poll_timeout = PollTimeout::new(timeout);

        loop {
            if let Some(event) = self.ready.pop_front() {
                return Ok(Some(event));
            }
            if let Some(event_ready) = self.poll.poll(poll_timeout.leftover())? {
                let number = self.console.number_of_console_input_events()?;
                if event_ready && number != 0 {
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
