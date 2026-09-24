use std::{collections::VecDeque, io, time::Duration};

use crossterm_winapi::{Console, Handle, InputRecord};

use crate::event::{
    sys::windows::{parse::MouseButtonsPressed, poll::WinApiPoll},
    Event,
};

#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    source::EventSource,
    sys::windows::parse::{handle_key_event, handle_mouse_event},
    timeout::PollTimeout,
    InternalEvent,
};

pub(crate) struct WindowsEventSource {
    console: Console,
    poll: WinApiPoll,
    pending_input_records: VecDeque<InputRecord>,
    surrogate_buffer: Option<u16>,
    mouse_buttons_pressed: MouseButtonsPressed,
}

impl WindowsEventSource {
    pub(crate) fn new() -> std::io::Result<WindowsEventSource> {
        let console = Console::from(Handle::current_in_handle()?);
        Ok(WindowsEventSource {
            console,

            #[cfg(not(feature = "event-stream"))]
            poll: WinApiPoll::new(),
            #[cfg(feature = "event-stream")]
            poll: WinApiPoll::new()?,

            pending_input_records: VecDeque::new(),
            surrogate_buffer: None,
            mouse_buttons_pressed: MouseButtonsPressed::default(),
        })
    }
}

fn refill_pending_input_records<T>(
    pending_input_records: &mut VecDeque<T>,
    read_console_input: impl FnOnce() -> io::Result<Vec<T>>,
) -> io::Result<()> {
    debug_assert!(pending_input_records.is_empty());
    pending_input_records.extend(read_console_input()?);
    Ok(())
}

impl EventSource for WindowsEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> std::io::Result<Option<InternalEvent>> {
        let poll_timeout = PollTimeout::new(timeout);

        loop {
            if let Some(record) = self.pending_input_records.pop_front() {
                let event = match record {
                    InputRecord::KeyEvent(record) => {
                        handle_key_event(record, &mut self.surrogate_buffer)
                    }
                    InputRecord::MouseEvent(record) => {
                        let mouse_event = handle_mouse_event(record, &self.mouse_buttons_pressed);
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

                if poll_timeout.elapsed() {
                    return Ok(None);
                }

                continue;
            }

            if let Some(event_ready) = self.poll.poll(poll_timeout.leftover())? {
                if event_ready {
                    let console = &self.console;
                    refill_pending_input_records(&mut self.pending_input_records, || {
                        console.read_console_input()
                    })?;
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::io;

    use super::refill_pending_input_records;

    #[test]
    fn refill_pending_input_records_reads_once_and_preserves_order() {
        let read_count = Cell::new(0);
        let records = vec![1, 2, 3];
        let mut pending = VecDeque::new();

        refill_pending_input_records(&mut pending, || {
            read_count.set(read_count.get() + 1);
            Ok(records.clone())
        })
        .unwrap();

        assert_eq!(read_count.get(), 1);
        assert_eq!(pending.into_iter().collect::<Vec<_>>(), records);
    }

    #[test]
    fn refill_pending_input_records_propagates_read_errors() {
        let mut pending = VecDeque::<u8>::new();
        let error = refill_pending_input_records(&mut pending, || {
            Err(io::Error::new(io::ErrorKind::Other, "read failed"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(pending.is_empty());
    }
}
