use std::{
    io::{self, Error, Write},
    time::Duration,
};

use crate::{
    event::{
        filter::CursorPositionFilter,
        internal::{self, InternalEvent},
        timeout::PollTimeout,
    },
    terminal::{disable_raw_mode, enable_raw_mode, sys::is_raw_mode_enabled},
};

/// Returns the cursor position (column, row).
///
/// The top left cell is represented as `(0, 0)`.
///
/// On unix systems, this function will block and possibly time out while
/// [`crossterm::event::read`](crate::event::read) or [`crossterm::event::poll`](crate::event::poll) are being called.
pub fn position() -> io::Result<(u16, u16)> {
    if is_raw_mode_enabled() {
        read_position_raw()
    } else {
        read_position()
    }
}

/// Returns the cursor position (column, row) with a bounded wait for its reply.
///
/// The top left cell is `(0, 0)`. One deadline covers acquiring the shared input reader,
/// discarding stale cursor replies, and waiting for the new reply. Unrelated input and terminal
/// replies remain queued, and incomplete sequences retain their parser state. The deadline does
/// not bound operating-system calls that change terminal modes or write the query.
///
/// Pause or drop active event streams before calling this function. It uses their existing
/// input reader and returns [`io::ErrorKind::TimedOut`] when the deadline expires.
pub fn position_with_timeout(timeout: Duration) -> io::Result<(u16, u16)> {
    let timeout = PollTimeout::new(Some(timeout));
    let raw_mode_enabled = is_raw_mode_enabled();
    if !raw_mode_enabled {
        enable_raw_mode()?;
    }
    let result = (|| {
        let timed_out = || Error::new(io::ErrorKind::TimedOut, "cursor position query timed out");
        let mut reader = internal::try_lock_event_reader_for(timeout.leftover().unwrap())
            .ok_or_else(timed_out)?;
        while !timeout.elapsed() && reader.poll(Some(Duration::ZERO), &CursorPositionFilter)? {
            let _ = reader.try_read(&CursorPositionFilter);
        }
        if timeout.elapsed() {
            return Err(timed_out());
        }

        let mut stdout = io::stdout();
        stdout.write_all(b"\x1b[6n")?;
        stdout.flush()?;
        while !timeout.elapsed() {
            if !reader.poll(timeout.leftover(), &CursorPositionFilter)? {
                continue;
            }
            if let Some(InternalEvent::CursorPosition(x, y)) =
                reader.try_read(&CursorPositionFilter)
            {
                return Ok((x, y));
            }
        }
        Err(timed_out())
    })();
    if !raw_mode_enabled {
        disable_raw_mode()?;
    }
    result
}

fn read_position() -> io::Result<(u16, u16)> {
    enable_raw_mode()?;
    let pos = read_position_raw();
    disable_raw_mode()?;
    pos
}

fn read_position_raw() -> io::Result<(u16, u16)> {
    // Discard any buffered cursor-position replies from earlier `ESC[6n` requests so the
    // position returned below corresponds to the fresh request we are about to send.
    // Poll with a zero timeout to drain only already-available events without blocking.
    while let Ok(true) = internal::poll(Some(Duration::ZERO), &CursorPositionFilter) {
        let _ = internal::read(&CursorPositionFilter);
    }

    // Use `ESC [ 6 n` to and retrieve the cursor position.
    let mut stdout = io::stdout();
    stdout.write_all(b"\x1B[6n")?;
    stdout.flush()?;

    loop {
        match internal::poll(Some(Duration::from_millis(2000)), &CursorPositionFilter) {
            Ok(true) => {
                if let Ok(InternalEvent::CursorPosition(x, y)) =
                    internal::read(&CursorPositionFilter)
                {
                    return Ok((x, y));
                }
            }
            Ok(false) => {
                return Err(Error::other(
                    "The cursor position could not be read within a normal duration",
                ));
            }
            Err(_) => {}
        }
    }
}
