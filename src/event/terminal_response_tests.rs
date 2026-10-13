use super::*;

#[test]
fn fragmented_console_replies_stay_framed_and_other_input_is_replayed() {
    for (reply, expected) in [
        (
            b"\x1b]10;rgb:1010/1111/1212\x1b\\".as_slice(),
            InternalEvent::OscColor {
                slot: 10,
                payload: OscColorPayload::Rgb {
                    r: 16,
                    g: 17,
                    b: 18,
                },
            },
        ),
        (
            b"\x1b]11;rgb:ffff/ffff/ffff\x07",
            InternalEvent::OscColor {
                slot: 11,
                payload: OscColorPayload::Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                },
            },
        ),
        (b"\x1b[0n", InternalEvent::OperatingStatus),
        (b"\x1b[?997;2n", InternalEvent::ColorSchemeChanged),
    ] {
        for end in 1..reply.len() {
            assert_eq!(parse_console_response(&reply[..end]).unwrap(), None);
        }
        assert_eq!(parse_console_response(reply).unwrap(), Some(expected));
    }
    for input in [b"\x1b[A".as_slice(), b"\x1b[200~", b"\x1bX", b"hello"] {
        assert!(parse_console_response(input).is_err());
    }
    let oversized = [b"\x1b]11;".as_slice(), &vec![b'a'; 1024]].concat();
    assert!(parse_console_response(&oversized).is_err());
}
