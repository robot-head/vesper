//! RFC 9000 §18 transport parameters: codec, validation, and corrupted
//! parameters arriving through a real rustls server.

mod common;

use common::*;
use vesper_warp_tls::error::code;
use vesper_warp_tls::params::id;
use vesper_warp_tls::rustls::quic;
use vesper_warp_tls::{
    ConnectionId, PreferredAddress, Side, Space, TlsError, TransportParameterError as E,
    TransportParameters,
};

fn varint_param(pid: u8, v: &[u8]) -> Vec<u8> {
    let mut out = vec![pid, v.len() as u8];
    out.extend_from_slice(v);
    out
}

fn with_isci(mut body: Vec<u8>) -> Vec<u8> {
    body.extend(varint_param(0x0f, SERVER_SCID));
    body
}

fn full_server_params() -> TransportParameters {
    TransportParameters {
        original_destination_connection_id: Some(ConnectionId::new(ODCID).unwrap()),
        preferred_address: Some(PreferredAddress {
            ipv4: [192, 0, 2, 1],
            ipv4_port: 4433,
            ipv6: [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            ipv6_port: 4434,
            connection_id: ConnectionId::new(&[1, 2, 3, 4]).unwrap(),
            stateless_reset_token: [0x42; 16],
        }),
        retry_source_connection_id: Some(ConnectionId::new(&[7; 8]).unwrap()),
        max_udp_payload_size: 1452,
        ack_delay_exponent: 20,
        max_ack_delay_ms: (1 << 14) - 1,
        active_connection_id_limit: 8,
        initial_max_streams_bidi: 1 << 60,
        ..server_params()
    }
}

#[test]
fn every_parameter_round_trips() {
    let p = full_server_params();
    let bytes = p.encode(Side::Server).unwrap();
    assert_eq!(
        TransportParameters::decode(&bytes, Side::Server).unwrap(),
        p
    );
    let c = client_params();
    let bytes = c.encode(Side::Client).unwrap();
    assert_eq!(
        TransportParameters::decode(&bytes, Side::Client).unwrap(),
        c
    );
}

#[test]
fn unknown_and_reserved_parameters_are_ignored() {
    let mut bytes = client_params().encode(Side::Client).unwrap();
    // Reserved id 31 * 3 + 27 = 120 (two-byte varint 0x40 0x78), and an
    // unknown four-byte-varint id; each repeated, which is fine.
    for _ in 0..2 {
        bytes.extend_from_slice(&[0x40, 0x78, 3, 1, 2, 3]);
        bytes.extend_from_slice(&[0x80, 0x01, 0x00, 0x00, 0]);
    }
    assert_eq!(
        TransportParameters::decode(&bytes, Side::Client).unwrap(),
        client_params()
    );
}

#[test]
fn malformed_lists_are_rejected() {
    let isci = with_isci;
    let cases: Vec<(Vec<u8>, E)> = vec![
        // Truncated id / length / value.
        (vec![0x40], E::Truncated),
        (vec![0x04], E::Truncated),
        (vec![0x04, 4, 0x80], E::Truncated),
        // Duplicate initial_max_data.
        (
            isci([varint_param(4, &[1]), varint_param(4, &[2])].concat()),
            E::Duplicate(id::INITIAL_MAX_DATA),
        ),
        // A varint value that does not fill its length.
        (
            isci(varint_param(4, &[1, 0])),
            E::InvalidValue(id::INITIAL_MAX_DATA),
        ),
        // An empty varint value.
        (
            isci(varint_param(4, &[])),
            E::InvalidValue(id::INITIAL_MAX_DATA),
        ),
        // Out-of-range values.
        (
            isci(varint_param(3, &[0x43, 0xe7])), // 999
            E::InvalidValue(id::MAX_UDP_PAYLOAD_SIZE),
        ),
        (
            isci(varint_param(0x0a, &[21])),
            E::InvalidValue(id::ACK_DELAY_EXPONENT),
        ),
        (
            isci(varint_param(0x0b, &[0x80, 0, 0x40, 0])), // 2^14
            E::InvalidValue(id::MAX_ACK_DELAY),
        ),
        (
            isci(varint_param(0x0e, &[1])),
            E::InvalidValue(id::ACTIVE_CONNECTION_ID_LIMIT),
        ),
        (
            // 2^60 + 1 streams.
            isci(varint_param(8, &[0xd0, 0, 0, 0, 0, 0, 0, 1])),
            E::InvalidValue(id::INITIAL_MAX_STREAMS_BIDI),
        ),
        (
            isci(varint_param(0x0c, &[0])),
            E::InvalidValue(id::DISABLE_ACTIVE_MIGRATION),
        ),
        (
            isci(varint_param(0x0f, &[0; 21])),
            E::InvalidValue(id::INITIAL_SOURCE_CONNECTION_ID),
        ),
        // Missing initial_source_connection_id.
        (
            varint_param(4, &[1]),
            E::Missing(id::INITIAL_SOURCE_CONNECTION_ID),
        ),
        // Server-only parameters from a client.
        (
            isci(varint_param(2, &[0; 16])),
            E::ServerOnly(id::STATELESS_RESET_TOKEN),
        ),
        (
            isci(varint_param(0, ODCID)),
            E::ServerOnly(id::ORIGINAL_DESTINATION_CONNECTION_ID),
        ),
    ];
    for (bytes, want) in cases {
        assert_eq!(
            TransportParameters::decode(&bytes, Side::Client),
            Err(want),
            "{bytes:02x?}"
        );
    }

    // Server-side rules.
    let server_cases: Vec<(Vec<u8>, E)> = vec![
        (
            with_isci(vec![]),
            E::Missing(id::ORIGINAL_DESTINATION_CONNECTION_ID),
        ),
        (
            with_isci([varint_param(0, ODCID), varint_param(2, &[0; 15])].concat()),
            E::InvalidValue(id::STATELESS_RESET_TOKEN),
        ),
    ];
    for (bytes, want) in server_cases {
        assert_eq!(TransportParameters::decode(&bytes, Side::Server), Err(want));
    }

    // preferred_address: zero-length CID, short, and trailing bytes.
    let p = full_server_params();
    let mut ok = p.encode(Side::Server).unwrap();
    let pos = ok.windows(2).position(|w| w == [0x0d, 45]).unwrap();
    ok[pos + 2 + 24] = 0; // CID length byte
    assert_eq!(
        TransportParameters::decode(&ok, Side::Server),
        Err(E::InvalidValue(id::PREFERRED_ADDRESS))
    );
}

#[test]
fn cid_authentication_rules() {
    use vesper_warp_tls::CidExpectations;
    let p = full_server_params();
    let good = CidExpectations {
        peer_initial_scid: SERVER_SCID,
        original_dcid: Some(ODCID),
        retry_scid: Some(&[7; 8]),
    };
    p.authenticate_cids(Side::Server, &good).unwrap();
    let no_retry = CidExpectations {
        retry_scid: None,
        ..good
    };
    assert_eq!(
        p.authenticate_cids(Side::Server, &no_retry),
        Err(E::ConnectionIdMismatch(id::RETRY_SOURCE_CONNECTION_ID))
    );
    let other_retry = CidExpectations {
        retry_scid: Some(&[8; 8]),
        ..good
    };
    assert_eq!(
        p.authenticate_cids(Side::Server, &other_retry),
        Err(E::ConnectionIdMismatch(id::RETRY_SOURCE_CONNECTION_ID))
    );
}

/// Runs a real rustls QUIC server whose transport parameters are `raw`
/// against our client, returning the client's error.
fn client_error_for_server_params(raw: Vec<u8>) -> TlsError {
    let mut client = new_client();
    let mut server =
        quic::ServerConnection::new(server_setup().config().clone(), quic::Version::V1, raw)
            .unwrap();
    let mut client_hello = Vec::new();
    while let Some(e) = client.poll_event() {
        if let vesper_warp_tls::Event::Crypto { data, .. } = e {
            client_hello.extend(data);
        }
    }
    server.read_hs(&client_hello).unwrap();
    client.set_peer_initial_scid(SERVER_SCID).unwrap();
    let mut space = Space::Initial;
    let mut offsets = [0u64; 3];
    loop {
        let mut buf = Vec::new();
        let change = server.write_hs(&mut buf);
        if !buf.is_empty() {
            let off = offsets[space.index()];
            offsets[space.index()] += buf.len() as u64;
            if let Err(e) = client.on_crypto_frame(space, off, &buf) {
                return e;
            }
        }
        match change {
            Some(quic::KeyChange::Handshake { .. }) => space = Space::Handshake,
            Some(quic::KeyChange::OneRtt { .. }) => space = Space::Data,
            None if buf.is_empty() => panic!("client accepted the parameters"),
            None => {}
        }
    }
}

#[test]
fn corrupted_parameters_from_a_rustls_server_close_with_transport_parameter_error() {
    let good = {
        let mut p = server_params();
        p.original_destination_connection_id = Some(ConnectionId::new(ODCID).unwrap());
        p.encode(Side::Server).unwrap()
    };
    let cases: Vec<(Vec<u8>, E)> = vec![
        // Truncated in the middle of the last parameter.
        (good[..good.len() - 1].to_vec(), E::Truncated),
        // A duplicated parameter.
        (
            [good.clone(), varint_param(0x04, &[1])].concat(),
            E::Duplicate(id::INITIAL_MAX_DATA),
        ),
        // Out of range.
        (
            [good.clone(), varint_param(0x0e, &[0])].concat(),
            E::InvalidValue(id::ACTIVE_CONNECTION_ID_LIMIT),
        ),
        // No original_destination_connection_id from the server.
        (
            with_isci(vec![]),
            E::Missing(id::ORIGINAL_DESTINATION_CONNECTION_ID),
        ),
        // Random bytes.
        (vec![0xff; 9], E::Truncated),
    ];
    for (raw, want) in cases {
        let err = client_error_for_server_params(raw);
        assert_eq!(err, TlsError::TransportParameters(want));
        assert_eq!(err.quic_error_code(), code::TRANSPORT_PARAMETER_ERROR);
    }
}
