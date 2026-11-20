use super::*;
use crate::event::{KeyCode, internal::OscColorPayload};

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
    assert!(!StreamFilter::Terminal.eval(&InternalEvent::CursorPosition(1, 2)));
}
