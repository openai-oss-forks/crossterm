#[cfg(feature = "use-dev-tty")]
pub(crate) mod tty;

#[cfg(not(feature = "use-dev-tty"))]
pub(crate) mod mio;

#[cfg(feature = "use-dev-tty")]
pub(crate) use self::tty::UnixInternalEventSource;

#[cfg(not(feature = "use-dev-tty"))]
pub(crate) use self::mio::UnixInternalEventSource;

fn unread_bytes(tty: &crate::terminal::sys::file_descriptor::FileDesc<'_>) -> std::io::Result<u64> {
    // SAFETY: the source owns this descriptor for the lifetime of the borrow.
    let fd = unsafe { rustix::fd::BorrowedFd::borrow_raw(tty.raw_fd()) };
    Ok(rustix::io::ioctl_fionread(fd)?)
}
