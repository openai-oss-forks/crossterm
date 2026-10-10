use super::*;
use crate::event::{KeyCode, KeyboardEnhancementFlags, internal::OscColorPayload};

#[test]
fn terminal_stream_includes_palette_replies_without_changing_input_streams() {
    let key = InternalEvent::Event(Event::Key(KeyCode::Char('x').into()));
    assert!(StreamFilter::Input.eval(&key));
    assert!(StreamFilter::Terminal.eval(&key));
    for event in [
        InternalEvent::ColorSchemeChanged,
        InternalEvent::OperatingStatus,
        InternalEvent::OscColor {
            slot: 10,
            payload: OscColorPayload::Rgb { r: 1, g: 2, b: 3 },
        },
        InternalEvent::OscColor {
            slot: 11,
            payload: OscColorPayload::Unrecognized("unknown".into()),
        },
    ] {
        assert!(!StreamFilter::Input.eval(&event));
        assert!(StreamFilter::Terminal.eval(&event));
    }
    let keyboard = InternalEvent::KeyboardEnhancementFlags(
        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
    );
    assert!(!StreamFilter::Input.eval(&keyboard));
    assert!(StreamFilter::Terminal.eval(&keyboard));
    // Draining a palette boundary must leave a pending keyboard reply for the stream.
    assert!(!StreamFilter::Responses.eval(&keyboard));
    assert!(!StreamFilter::Input.eval(&InternalEvent::KeyboardEnhancementDetected));
    assert!(StreamFilter::Terminal.eval(&InternalEvent::KeyboardEnhancementDetected));
    assert!(!StreamFilter::Responses.eval(&InternalEvent::KeyboardEnhancementDetected));
    assert!(!StreamFilter::Terminal.eval(&InternalEvent::CursorPosition(1, 2)));
}

#[test]
fn canceled_stream_does_not_poll_after_waiting_for_the_reader() {
    let reader = internal::lock_event_reader();
    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = shutdown.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        ready_tx.send(()).unwrap();
        result_tx
            .send(internal::poll_event_stream(
                &StreamFilter::Terminal,
                &worker_shutdown,
            ))
            .unwrap();
    });
    ready_rx.recv().unwrap();
    shutdown.store(true, Ordering::SeqCst);
    // Cancellation remains visible even if a different reader has already consumed its wakeup.
    drop(reader);
    assert!(
        !result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
    );
    worker.join().unwrap();
}
