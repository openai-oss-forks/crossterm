#[cfg(feature = "use-dev-tty")]
pub(crate) mod tty;

#[cfg(not(feature = "use-dev-tty"))]
pub(crate) mod mio;

#[cfg(feature = "use-dev-tty")]
pub(crate) use self::tty::UnixInternalEventSource;

#[cfg(not(feature = "use-dev-tty"))]
pub(crate) use self::mio::UnixInternalEventSource;

#[derive(Debug, Clone, Copy)]
enum DiscardedSequence {
    PasteStart(usize),
    PasteBody(usize),
    OscBody,
    OscEscape,
    Csi,
    X10Mouse(usize),
    Ss3,
}

const SGR_MOUSE: &[u8] = b"\x1b[<";

// xterm.js can report ESC [ < 16 ; NaN ; NaN m when a link opens and
// detaches the terminal during mouseup. CSI would normally end at the first
// 'N'. Extend the frame only while it still looks like an SGR mouse report.
fn advance_sgr_mouse(
    buffer: &mut Vec<u8>,
    discarded: &mut Option<DiscardedSequence>,
    byte: u8,
) -> bool {
    // A discarded ESC [ could still have been the start of bracketed paste.
    if byte == b'<' && matches!(discarded, Some(DiscardedSequence::PasteStart(2))) {
        buffer.extend_from_slice(SGR_MOUSE);
        *discarded = Some(DiscardedSequence::Csi);
        return true;
    }
    if (discarded.is_none() && byte != b'N') || !buffer.starts_with(SGR_MOUSE) {
        return false;
    }

    buffer.push(byte);
    if matches_sgr_parameters(&buffer[SGR_MOUSE.len()..], false) {
        *discarded = Some(DiscardedSequence::Csi);
        return true;
    }
    buffer.pop();
    if discarded.is_some() {
        let params = &buffer[SGR_MOUSE.len()..];
        let terminator = matches!(byte, b'M' | b'm') && matches_sgr_parameters(params, true);
        if params.contains(&b'N') && !terminator {
            // Process the interrupting byte as fresh input.
            *discarded = None;
        }
        buffer.clear();
    }
    false
}

fn matches_sgr_parameters(params: &[u8], complete: bool) -> bool {
    fn decimal(field: &[u8], digits: usize) -> bool {
        !field.is_empty() && field.len() <= digits && field.iter().all(u8::is_ascii_digit)
    }

    let mut fields = params.split(|&b| b == b';').peekable();
    if !decimal(fields.next().unwrap(), 3) {
        return false;
    }
    for _ in 0..2 {
        let Some(coord) = fields.next() else {
            return !complete;
        };
        if !complete && fields.peek().is_none() && b"NaN".starts_with(coord) {
            return true;
        }
        if coord != b"NaN" && !decimal(coord, 5) {
            return false;
        }
    }
    // The mouse parser also accepts a trailing separator before M/m.
    fields.next().is_none_or(<[u8]>::is_empty) && fields.next().is_none()
}
