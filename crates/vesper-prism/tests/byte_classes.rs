//! Every byte at every position of the word-at-a-time scans (SH6 lane 5,
//! #1400): field names (`tchar`), field values (controls) and request
//! targets, checked against the RFC grammars written out here. The scans read
//! eight bytes at a time, so the positions cover the first two words, the
//! tail after them and both sides of each word boundary.
use vesper_prism::{
    Decoder, Error, Limits,
    response::{ConnectionPolicy, ResponseBuilder, ResponseContext, ResponseError, ResponseLimits},
    target::{Target, TargetError, TargetLimits},
};

/// Field lengths and the position of the byte under test.
const LENGTHS: [usize; 6] = [1, 7, 8, 9, 16, 21];

fn tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// RFC 3986 origin-form bytes besides `%` escapes.
fn target_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/?".contains(&b)
}

fn positions() -> impl Iterator<Item = (usize, usize)> {
    LENGTHS
        .into_iter()
        .flat_map(|len| (0..len).map(move |at| (len, at)))
}

fn builder() -> ResponseBuilder {
    ResponseBuilder::new(
        200,
        ResponseContext::Ordinary,
        ConnectionPolicy::KeepAlive,
        0,
        ResponseLimits::default(),
    )
    .unwrap()
}

#[test]
fn response_field_names_accept_exactly_tchar() {
    for (len, at) in positions() {
        for b in 0..=255u8 {
            let mut name = vec![b'x'; len];
            name[at] = b;
            let Ok(name) = String::from_utf8(name) else {
                continue;
            };
            let accepted = builder().header(&name, b"v").is_ok();
            assert_eq!(accepted, tchar(b), "byte {b} at {at} of {len}");
        }
    }
}

#[test]
fn response_field_values_refuse_every_control_and_del() {
    for (len, at) in positions() {
        for b in 0..=255u8 {
            let mut value = vec![b'v'; len];
            value[at] = b;
            let result = builder().header("x-test", &value);
            let control = b < 0x20 || b == 0x7f;
            match result {
                Ok(_) => assert!(!control, "byte {b} at {at} of {len} accepted"),
                Err(e) => {
                    assert!(control, "byte {b} at {at} of {len}: {e:?}");
                    assert_eq!(e, ResponseError::InvalidHeaderValue);
                }
            }
        }
    }
}

fn decode(head: &[u8]) -> Result<(), Error> {
    let mut decoder = Decoder::new(Limits::default()).unwrap();
    decoder.push(head)?;
    decoder.end_input();
    decoder.next_request()?.expect("complete request");
    Ok(())
}

#[test]
fn request_field_names_and_values_follow_their_classes() {
    for (len, at) in positions() {
        for b in 0..=255u8 {
            let mut name = vec![b'x'; len];
            name[at] = b;
            let mut head = b"GET / HTTP/1.1\r\nHost: example.test\r\n".to_vec();
            // A colon ends the name early: the rest is a valid value.
            let colon = b == b':' && at > 0;
            head.extend_from_slice(&name);
            head.extend_from_slice(b": v\r\n\r\n");
            assert_eq!(
                decode(&head).is_ok(),
                tchar(b) || colon,
                "name byte {b} at {at} of {len}"
            );

            let mut value = vec![b'v'; len];
            value[at] = b;
            let mut head = b"GET / HTTP/1.1\r\nHost: example.test\r\nX-Test: ".to_vec();
            head.extend_from_slice(&value);
            head.extend_from_slice(b"\r\n\r\n");
            // A value may hold tab and obs-text but no other control.
            let allowed = b == b'\t' || (b >= 0x20 && b != 0x7f);
            // CR and LF end the line instead (another error), and a trailing
            // tab or space is trimmed whitespace, which is allowed anyway.
            if b == b'\r' || b == b'\n' {
                assert!(decode(&head).is_err(), "value byte {b} at {at} of {len}");
            } else {
                assert_eq!(
                    decode(&head).is_ok(),
                    allowed,
                    "value byte {b} at {at} of {len}"
                );
            }
        }
    }
}

#[test]
fn targets_accept_exactly_the_origin_form_bytes() {
    for (len, at) in positions() {
        for b in 0..=255u8 {
            // `%` starts an escape, and a leading `//` is refused by the
            // grammar itself.
            if b == b'%' || (b == b'/' && at == 0) {
                continue;
            }
            let mut raw = vec![b'p'; len];
            raw[at] = b;
            let mut target = b"/".to_vec();
            target.extend_from_slice(&raw);
            let Ok(target) = String::from_utf8(target) else {
                continue;
            };
            let parsed = Target::parse(&target, TargetLimits::default());
            if target_char(b) {
                // The byte is allowed; a `?`, `/` or `.` can still make the
                // target fail a later rule, but never as a syntax error.
                assert!(
                    !matches!(
                        parsed,
                        Err(TargetError::InvalidSyntax | TargetError::InvalidEscape)
                    ),
                    "byte {b} at {at} of {len}: {parsed:?}"
                );
            } else {
                assert_eq!(
                    parsed,
                    Err(TargetError::InvalidSyntax),
                    "byte {b} at {at} of {len}"
                );
            }
        }
    }
}
