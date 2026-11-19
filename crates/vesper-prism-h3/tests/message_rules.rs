//! HTTP/3 message validation (RFC 9114 §4.1.2, §4.2, §4.3) through the
//! public API, so both builds and the divergent port builds
//! (`port/check.sh`) run it.

use vesper_prism_h3::message::{
    MessageError, RequestHead, check_trailers, parse_request, parse_response,
};
use vesper_prism_h3::qpack::Field;

fn req(list: &[(&str, &str)]) -> Result<RequestHead, MessageError> {
    parse_request(list.iter().map(|(n, v)| Field::new(*n, *v)).collect())
}

#[test]
fn request_rules() {
    let ok = req(&[
        (":method", "GET"),
        (":scheme", "https"),
        (":authority", "a.test"),
        (":path", "/x"),
        ("accept", "*/*"),
    ])
    .unwrap();
    assert_eq!(ok.authority.as_deref(), Some("a.test"));
    assert_eq!(ok.fields.len(), 1);
    // Host instead of :authority.
    let h = req(&[
        (":method", "GET"),
        (":scheme", "https"),
        (":path", "/"),
        ("host", "b.test"),
    ])
    .unwrap();
    assert_eq!(h.authority.as_deref(), Some("b.test"));
    for (fields, err) in [
        (
            vec![(":method", "GET"), (":path", "/")],
            MessageError::MissingPseudoHeader,
        ),
        (
            vec![(":method", "GET"), (":scheme", "https"), (":path", "/")],
            MessageError::MissingPseudoHeader,
        ),
        (
            vec![(":method", "GET"), (":method", "GET")],
            MessageError::DuplicatePseudoHeader,
        ),
        (
            vec![("accept", "x"), (":method", "GET")],
            MessageError::PseudoAfterRegular,
        ),
        (
            vec![(":protocol", "websocket")],
            MessageError::MissingPseudoHeader,
        ),
        (
            vec![
                (":method", "GET"),
                (":protocol", "websocket"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
            ],
            MessageError::InvalidPseudoHeader,
        ),
        (
            vec![
                (":method", "CONNECT"),
                (":protocol", "webtransport"),
                (":authority", "a"),
                (":path", "/"),
            ],
            MessageError::MissingPseudoHeader,
        ),
        (
            vec![(":method", "CONNECT"), (":authority", "a:443")],
            MessageError::UnsupportedConnect,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", ""),
            ],
            MessageError::InvalidPseudoHeader,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("Accept", "x"),
            ],
            MessageError::InvalidFieldName,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("x", " y"),
            ],
            MessageError::InvalidFieldValue,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("connection", "close"),
            ],
            MessageError::ConnectionSpecificField,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("te", "gzip"),
            ],
            MessageError::ConnectionSpecificField,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("host", "b"),
            ],
            MessageError::AuthorityMismatch,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("content-length", "1"),
                ("content-length", "2"),
            ],
            MessageError::InvalidContentLength,
        ),
        (
            vec![
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "a"),
                (":path", "/"),
                ("content-length", "-1"),
            ],
            MessageError::InvalidContentLength,
        ),
    ] {
        assert_eq!(req(&fields), Err(err), "{fields:?}");
    }
}

#[test]
fn response_rules() {
    let r = parse_response(vec![
        Field::new(":status", "204"),
        Field::new("server", "prism"),
    ])
    .unwrap();
    assert_eq!(r.status, 204);
    assert_eq!(
        parse_response(vec![Field::new(":status", "20")]),
        Err(MessageError::InvalidStatus)
    );
    assert_eq!(
        parse_response(vec![Field::new(":path", "/")]),
        Err(MessageError::UnknownPseudoHeader)
    );
    assert_eq!(
        check_trailers(&[Field::new(":status", "200")]),
        Err(MessageError::UnknownPseudoHeader)
    );
}

fn base() -> Vec<(&'static str, &'static str)> {
    vec![
        (":method", "POST"),
        (":scheme", "https"),
        (":authority", "a.test"),
        (":path", "/up"),
    ]
}

#[test]
fn content_length_and_host_rules() {
    let mut f = base();
    f.extend([
        ("content-length", "12"),
        ("x", "y"),
        ("content-length", "12"),
    ]);
    let r = req(&f).unwrap();
    assert_eq!(r.content_length, Some(12));
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/up");
    assert_eq!(r.fields.len(), 3);
    let mut f = base();
    f.push(("content-length", "18446744073709551615"));
    assert_eq!(req(&f), Err(MessageError::InvalidContentLength));
    let mut f = base();
    f.push(("content-length", "9999999999999999999"));
    assert_eq!(
        req(&f).unwrap().content_length,
        Some(9_999_999_999_999_999_999)
    );
    // Two Host fields disagree with themselves even when they match.
    let f = vec![
        (":method", "GET"),
        (":scheme", "https"),
        (":path", "/"),
        ("host", "a"),
        ("host", "a"),
    ];
    assert_eq!(req(&f), Err(MessageError::AuthorityMismatch));
    // A matching Host is fine; TE: trailers is allowed.
    let mut f = base();
    f.extend([("host", "a.test"), ("te", "trailers")]);
    assert!(req(&f).is_ok());
    // Extended CONNECT.
    let r = req(&[
        (":method", "CONNECT"),
        (":protocol", "webtransport"),
        (":scheme", "https"),
        (":authority", "a.test"),
        (":path", "/wt"),
    ])
    .unwrap();
    assert_eq!(r.protocol.as_deref(), Some("webtransport"));
}

#[test]
fn pseudo_header_values_are_checked() {
    let mut f = base();
    f[3] = (":path", "/a b");
    assert_eq!(req(&f), Err(MessageError::InvalidPseudoHeader));
    let mut f = base();
    f[0] = (":method", "G(T");
    assert_eq!(req(&f), Err(MessageError::InvalidPseudoHeader));
    // Not UTF-8.
    let mut fields: Vec<Field> = base().iter().map(|(n, v)| Field::new(*n, *v)).collect();
    fields[3] = Field::new(":path", &b"/\xff"[..]);
    assert_eq!(parse_request(fields), Err(MessageError::InvalidFieldValue));
    let mut f = base();
    f.push((":status", "200"));
    assert_eq!(req(&f), Err(MessageError::UnknownPseudoHeader));
    let mut f = base();
    f.push(("x", "a\rb"));
    assert_eq!(req(&f), Err(MessageError::InvalidFieldValue));
    let mut f = base();
    f.push(("", "x"));
    assert_eq!(req(&f), Err(MessageError::InvalidFieldName));
    let mut f = base();
    f.push(("x", "a\t"));
    assert_eq!(req(&f), Err(MessageError::InvalidFieldValue));
}

#[test]
fn response_statuses() {
    for (s, ok) in [
        ("100", true),
        ("599", true),
        ("099", false),
        ("600", false),
        ("2x0", false),
    ] {
        let r = parse_response(vec![
            Field::new(":status", s),
            Field::new("content-length", "5"),
        ]);
        assert_eq!(r.is_ok(), ok, "{s}");
        if let Ok(r) = r {
            assert_eq!(r.status.to_string(), s);
            assert_eq!(r.content_length, Some(5));
            assert_eq!(r.fields.len(), 1);
        }
    }
    assert_eq!(
        parse_response(vec![]),
        Err(MessageError::MissingPseudoHeader)
    );
    assert_eq!(
        parse_response(vec![
            Field::new(":status", "200"),
            Field::new(":status", "200")
        ]),
        Err(MessageError::DuplicatePseudoHeader)
    );
    assert!(check_trailers(&[Field::new("x-checksum", "abc")]).is_ok());
    assert_eq!(
        check_trailers(&[Field::new("upgrade", "h2c")]),
        Err(MessageError::ConnectionSpecificField)
    );
}
