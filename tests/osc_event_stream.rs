#![cfg(all(unix, feature = "event-stream"))]

use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    os::{fd::FromRawFd, unix::process::CommandExt},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::Duration,
};

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;

// Run the real global EventStream in a subprocess with its own controlling PTY.
#[test]
fn event_stream_child() {
    if std::env::var_os("CROSSTERM_OSC_TEST_CHILD").is_none() {
        return;
    }
    crossterm::terminal::enable_raw_mode().unwrap();
    futures::executor::block_on(async {
        let mut events = EventStream::new();
        println!("READY");
        std::io::stdout().flush().unwrap();
        while let Some(event) = events.next().await {
            if let Event::Key(key) = event.unwrap() {
                println!("KEY {key:?}");
                std::io::stdout().flush().unwrap();
                if key == KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE) {
                    break;
                }
            }
        }
    });
    crossterm::terminal::disable_raw_mode().unwrap();
}

struct Probe {
    child: Child,
    input: File,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl Probe {
    fn new() -> Self {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty initializes two descriptors; optional parameters are null.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: these descriptors are newly opened and each File owns one.
        let input = unsafe { File::from_raw_fd(master) };
        let terminal = unsafe { File::from_raw_fd(slave) };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "event_stream_child", "--nocapture"])
            .env("CROSSTERM_OSC_TEST_CHILD", "1")
            .stdin(terminal)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // SAFETY: only async-signal-safe syscalls run between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let probe = Self {
            child,
            input,
            lines,
            reader: Some(reader),
        };
        while probe.lines.recv_timeout(Duration::from_secs(5)).unwrap() != "READY" {}
        probe
    }

    fn expect_key(&self, expected: KeyEvent) {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(3))
            .expect("input stalled after an OSC prefix");
        assert_eq!(line, format!("KEY {expected:?}"));
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        // A failed assertion must not leave a blocked EventStream subprocess behind.
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        self.child.wait().unwrap();
        self.reader.take().unwrap().join().unwrap();
    }
}

#[test]
fn unterminated_osc_preserves_ctrl_c_and_following_input() {
    for prefix in [b"\x1b]".as_slice(), b"\x1b]10;rgb:"] {
        for separate_write in [false, true] {
            let mut probe = Probe::new();
            if separate_write {
                probe.input.write_all(prefix).unwrap();
                thread::sleep(Duration::from_millis(50));
                probe.input.write_all(b"\x03q").unwrap();
            } else {
                let mut input = prefix.to_vec();
                input.extend_from_slice(b"\x03q");
                probe.input.write_all(&input).unwrap();
            }
            probe.expect_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
            probe.expect_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        }
    }
}

#[test]
fn bare_alt_bracket_eventually_resolves_without_more_input() {
    let mut probe = Probe::new();
    probe.input.write_all(b"\x1b]").unwrap();
    probe.expect_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT));
    probe.input.write_all(b"\x03q").unwrap();
    probe.expect_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    probe.expect_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
}
