use super::*;
use crate::event::{filter::Filter, stream::StreamFilter, KeyCode, OscColorPayload};

#[test]
fn color_reports_are_opt_in_and_require_valid_rgb() {
    let input = InternalEvent::Event(Event::Key(KeyCode::Char('x').into()));
    let color = InternalEvent::OscColor {
        slot: 11,
        payload: OscColorPayload::Rgb { r: 1, g: 2, b: 3 },
    };
    let invalid = InternalEvent::OscColor {
        slot: 10,
        payload: OscColorPayload::Unrecognized("?".to_string()),
    };

    assert!(StreamFilter::Input.eval(&input));
    assert!(StreamFilter::InputAndColors.eval(&input));
    assert!(!StreamFilter::Input.eval(&color));
    assert!(StreamFilter::InputAndColors.eval(&color));
    assert!(!StreamFilter::InputAndColors.eval(&invalid));
    assert_eq!(
        color_report(&color),
        Some(EventWithColor::BackgroundColor(Color::Rgb {
            r: 1,
            g: 2,
            b: 3
        }))
    );
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn extracts_color_reports_without_splitting_pasted_text() {
    let events = extract_paste_colors(
        "α\x1b]10;rgb:eeee/eeee/eeee\x07 β\x1b]11;rgb:11/22/33\x1b\\γ".to_string(),
    );
    assert_eq!(
        events.into_iter().collect::<Vec<_>>(),
        vec![
            InternalEvent::OscColor {
                slot: 10,
                payload: OscColorPayload::Rgb {
                    r: 238,
                    g: 238,
                    b: 238
                },
            },
            InternalEvent::OscColor {
                slot: 11,
                payload: OscColorPayload::Rgb {
                    r: 17,
                    g: 34,
                    b: 51
                },
            },
            InternalEvent::ProcessedPaste("α βγ".to_string()),
        ]
    );
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn preserves_unrelated_malformed_and_incomplete_paste_sequences() {
    for text in [
        "\x1b]52;preserve me\x07".to_string(),
        "\x1b]10;rgb:invalid\x07".to_string(),
        "\x1b]11;rgb:11/22/33\x1b".to_string(),
        format!("\x1b]10;{}\x07", "x".repeat(2048)),
    ] {
        assert_eq!(
            extract_paste_colors(text.clone())
                .into_iter()
                .collect::<Vec<_>>(),
            vec![InternalEvent::Event(Event::Paste(text))]
        );
    }
}
