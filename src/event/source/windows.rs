use std::time::Duration;

use crossterm_winapi::{Console, ConsoleMode, Handle, InputRecord};
use winapi::um::consoleapi::ReadConsoleInputW;
use winapi::um::wincon::{ENABLE_VIRTUAL_TERMINAL_INPUT, INPUT_RECORD};

use crate::event::{
    Event, InputDiscardStatus,
    sys::windows::{parse::MouseButtonsPressed, poll::WinApiPoll, vt_input::VtInput},
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
    poll: WinApiPoll,
    surrogate_buffer: Option<u16>,
    mouse_buttons_pressed: MouseButtonsPressed,
    prepared: bool,
    input: VtInput,
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

            surrogate_buffer: None,
            mouse_buttons_pressed: MouseButtonsPressed::default(),
            prepared: false,
            input: VtInput::default(),
        })
    }

    fn read_available(&mut self) -> std::io::Result<()> {
        let vt = self.prepared
            && ConsoleMode::from(Handle::current_in_handle()?).mode()?
                & ENABLE_VIRTUAL_TERMINAL_INPUT
                != 0;
        if !vt {
            self.push_record(self.console.read_single_input_event()?, false);
            return Ok(());
        }

        // Amortize Win32 calls while limiting the amount of work between
        // timeout/waker checks. Decode the batch in its original console mode.
        let handle = Handle::current_in_handle()?;
        let mut records = [INPUT_RECORD::default(); 1024];
        let mut count = 0;
        // SAFETY: the input handle remains live and the supplied length matches
        // the allocated array of writable INPUT_RECORDs.
        if unsafe {
            ReadConsoleInputW(
                *handle,
                records.as_mut_ptr(),
                records.len() as u32,
                &mut count,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        for record in &records[..count as usize] {
            self.push_record(InputRecord::from(*record), true);
        }
        Ok(())
    }

    fn push_record(&mut self, record: InputRecord, vt: bool) {
        let event = match record {
            // In native mode only synthetic replies (VK=0, beginning
            // with ESC) enter the probe parser. Keep ordinary key records
            // on the native path, including after an unanswered query.
            InputRecord::KeyEvent(record)
                if vt
                    || (self.input.probing()
                        && record.virtual_key_code == 0
                        && (record.u_char == 0x1b || self.input.is_buffering())) =>
            {
                self.input.push(record, &mut self.surrogate_buffer);
                return;
            }
            InputRecord::KeyEvent(record) => handle_key_event(record, &mut self.surrogate_buffer),
            InputRecord::MouseEvent(record) => {
                let event = handle_mouse_event(record, &self.mouse_buttons_pressed);
                self.mouse_buttons_pressed = MouseButtonsPressed {
                    left: record.button_state.left_button(),
                    right: record.button_state.right_button(),
                    middle: record.button_state.middle_button(),
                };
                event
            }
            InputRecord::WindowBufferSizeEvent(record) => Some(Event::Resize(
                (record.size.x as i32 + 1) as u16,
                (record.size.y as i32 + 1) as u16,
            )),
            InputRecord::FocusEvent(record) => Some(if record.set_focus {
                Event::FocusGained
            } else {
                Event::FocusLost
            }),
            _ => None,
        };
        if let Some(event) = event {
            self.input.events.push_back(InternalEvent::Event(event));
        }
    }
}

impl EventSource for WindowsEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> std::io::Result<Option<InternalEvent>> {
        if let Some(event) = self.input.events.pop_front() {
            return Ok(Some(event));
        }
        let poll_timeout = PollTimeout::new(timeout);
        loop {
            let wait = self.input.timeout(poll_timeout.leftover());
            if self.poll.poll(wait)? == Some(true)
                && self.console.number_of_console_input_events()? != 0
            {
                self.read_available()?;
            } else {
                self.input.flush_escape();
            }
            if let Some(event) = self.input.events.pop_front() {
                return Ok(Some(event));
            }
            if poll_timeout.elapsed() {
                return Ok(None);
            }
        }
    }

    fn prepare_windows_framed_paste(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<crate::event::Win32InputMode>> {
        use std::io::Write;
        // Avoid taking ownership of a partial sequence or accepting a late
        // reply from an earlier query as the answer to a fresh request.
        if timeout.is_zero() || self.input.is_buffering() || self.input.probing() {
            return Ok(None);
        }
        self.prepared = false;
        let budget = PollTimeout::new(Some(timeout));
        self.input.start_probe();
        {
            let mut output = std::io::stdout().lock();
            output.write_all(b"\x1b[?2004$p\x1b[?9001$p")?;
            output.flush()?;
        }
        while !budget.elapsed() {
            match self.poll.poll(budget.leftover()) {
                Ok(Some(true)) => {}
                Ok(_) => break,
                // Dropping a paused EventStream can leave its wake pending.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            if self.console.number_of_console_input_events()? != 0 {
                self.read_available()?;
            }
            if !self.input.probing() {
                let result = self.input.probe_result();
                self.prepared = result.is_some();
                return Ok(result);
            }
        }
        Ok(None)
    }

    fn discard_buffered_input(&mut self) -> InputDiscardStatus {
        self.surrogate_buffer = None;
        self.input.discard()
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.poll.waker()
    }
}
