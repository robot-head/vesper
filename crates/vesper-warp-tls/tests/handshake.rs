//! In-memory client/server handshakes over the rustls QUIC API, with the
//! exported secrets driving `vesper-warp-crypto` packet protection.

mod common;

use std::sync::Arc;

use common::*;
use vesper_warp_crypto::{OneRttKeys, TAG_LEN};
use vesper_warp_tls::error::code;
use vesper_warp_tls::params::id;
use vesper_warp_tls::rustls::{self, ClientConfig};
use vesper_warp_tls::{
    ClientSetup, KeyState, QuicTls, ServerCids, Space, TlsError, TransportParameterError,
};

fn secret(p: &Peer, space: Space) -> (Vec<u8>, Vec<u8>) {
    let k = p.keys[space.index()].as_ref().expect("keys for space");
    (
        k.local().as_bytes().to_vec(),
        k.remote().as_bytes().to_vec(),
    )
}

fn short_packet(keys: &mut OneRttKeys, dcid: &[u8], pn: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = vec![0x40 | (u8::from(keys.send_key_phase()) << 2) | 0x01];
    buf.extend_from_slice(dcid);
    let pn_offset = buf.len();
    buf.extend_from_slice(&(pn as u16).to_be_bytes());
    buf.extend_from_slice(payload);
    buf.extend_from_slice(&[0; TAG_LEN]);
    let ticket = keys.ticket(pn).unwrap();
    let len = keys
        .seal(ticket, &mut buf, pn_offset, payload.len())
        .unwrap();
    buf.truncate(len);
    buf
}

fn open_short(keys: &mut OneRttKeys, buf: &mut [u8], dcid_len: usize, pn: u64) -> Vec<u8> {
    let o = keys
        .open_packet(buf, 1 + dcid_len, 0, |_| Some(pn))
        .expect("1-RTT packet opens");
    buf[o.header_len..o.header_len + o.payload_len].to_vec()
}

fn long_packet_roundtrip(sender: &mut Peer, receiver: &mut Peer, space: Space) {
    let (mut sealer, _) = sender.keys[space.index()]
        .take()
        .unwrap()
        .into_sealer_opener()
        .unwrap();
    let (_, mut opener) = receiver.keys[space.index()]
        .take()
        .unwrap()
        .into_sealer_opener()
        .unwrap();
    let payload = [0x06u8; 40]; // padding-like plaintext
    let first = if space == Space::Initial { 0xc1 } else { 0xe1 };
    let mut buf = vec![first, 0, 0, 0, 1, 0, 0];
    if space == Space::Initial {
        buf.push(0); // empty token
    }
    buf.extend_from_slice(&[0x40, 2 + payload.len() as u8 + TAG_LEN as u8, 0x00, 0x07]);
    let pn_offset = buf.len() - 2;
    buf.extend_from_slice(&payload);
    buf.extend_from_slice(&[0; TAG_LEN]);
    let ticket = sealer.ticket(7).unwrap();
    sealer
        .seal(ticket, &mut buf, pn_offset, payload.len())
        .unwrap();
    let o = opener
        .open_packet(&mut buf, pn_offset, |_| Some(7))
        .unwrap();
    assert_eq!(&buf[o.header_len..o.header_len + o.payload_len], &payload);
}

#[test]
fn full_handshake_agrees_and_protects_packets() {
    let (mut client, mut server) = pair();
    run(&mut client, &mut server, |f| f).unwrap();

    for p in [&client, &server] {
        assert!(p.tls.is_handshake_complete());
        assert!(p.tls.is_handshake_confirmed());
        assert!(p.tls.key_update_permitted());
        assert_eq!(p.tls.alpn_protocol(), Some(ALPN));
        assert_eq!(p.tls.key_state(Space::Initial), KeyState::Discarded);
        assert_eq!(p.tls.key_state(Space::Handshake), KeyState::Discarded);
        assert_eq!(p.tls.key_state(Space::Data), KeyState::Installed);
    }
    assert!(client.tls.peer_certificates().is_some());

    // Both sides derived the same secrets, mirrored.
    for space in Space::ALL {
        let (cl, cr) = secret(&client, space);
        let (sl, sr) = secret(&server, space);
        assert_eq!(cl, sr, "{space:?} client->server secret");
        assert_eq!(cr, sl, "{space:?} server->client secret");
    }

    // Transport parameters arrived intact; the server's carry the CIDs the
    // driver filled in.
    let got_by_server = server.peer_params.clone().unwrap();
    assert_eq!(got_by_server, client_params());
    let got_by_client = client.peer_params.clone().unwrap();
    let mut want = server_params();
    want.original_destination_connection_id =
        Some(vesper_warp_tls::ConnectionId::new(ODCID).unwrap());
    assert_eq!(got_by_client, want);

    // Event order on the client follows RFC 9001 §4.9.
    let order: Vec<&str> = client.log.iter().map(String::as_str).collect();
    let pos = |s: &str| order.iter().position(|e| *e == s).unwrap();
    assert!(pos("keys Initial") < pos("keys Handshake"));
    assert!(pos("keys Handshake") < pos("discard Initial"));
    assert!(pos("keys Data") < pos("complete"));
    assert!(pos("complete") < pos("confirmed"));
    assert!(pos("confirmed") < pos("discard Handshake"));
    assert!(server.log.iter().any(|e| e == "send done"));

    // Initial and Handshake keys protect long-header packets end to end.
    long_packet_roundtrip(&mut client, &mut server, Space::Initial);
    long_packet_roundtrip(&mut server, &mut client, Space::Handshake);

    // 1-RTT keys in vesper-warp-crypto, both directions.
    let mut ck = client.keys[2]
        .take()
        .unwrap()
        .into_one_rtt(1_000_000)
        .unwrap();
    let mut sk = server.keys[2]
        .take()
        .unwrap()
        .into_one_rtt(1_000_000)
        .unwrap();
    let mut pkt = short_packet(&mut ck, SERVER_SCID, 0, b"hello over warp");
    assert_eq!(
        open_short(&mut sk, &mut pkt, SERVER_SCID.len(), 0),
        b"hello over warp"
    );
    let mut pkt = short_packet(&mut sk, CLIENT_SCID, 0, b"and back");
    assert_eq!(
        open_short(&mut ck, &mut pkt, CLIENT_SCID.len(), 0),
        b"and back"
    );

    // Key update hand-off: refused by packet protection until a packet of
    // the current phase is acknowledged, then the peer follows the phase.
    assert!(matches!(
        client.tls.initiate_key_update(&mut ck),
        Err(TlsError::KeyUpdate(_))
    ));
    ck.on_packet_acked(0);
    client.tls.initiate_key_update(&mut ck).unwrap();
    assert!(ck.send_key_phase());
    let mut pkt = short_packet(&mut ck, SERVER_SCID, 1, b"after key update");
    assert_eq!(
        open_short(&mut sk, &mut pkt, SERVER_SCID.len(), 1),
        b"after key update"
    );
    assert_eq!(sk.recv_generation(), 1);
}

fn client_with_suite(suite: rustls::SupportedCipherSuite) -> ClientSetup {
    let mut provider = (*vesper_warp_tls::config::provider()).clone();
    provider.cipher_suites = vec![suite];
    let config = ClientConfig::builder_with_details(Arc::new(provider), time(NOW_SECS))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots())
        .with_no_client_auth();
    ClientSetup::from_config(config, vec![ALPN.to_vec()]).unwrap()
}

#[test]
fn every_supported_cipher_suite_exports_matching_keys() {
    use rustls::crypto::aws_lc_rs::cipher_suite as cs;
    use vesper_warp_crypto::Cipher;
    for (suite, cipher) in [
        (cs::TLS13_AES_128_GCM_SHA256, Cipher::Aes128Gcm),
        (cs::TLS13_AES_256_GCM_SHA384, Cipher::Aes256Gcm),
        (cs::TLS13_CHACHA20_POLY1305_SHA256, Cipher::ChaCha20Poly1305),
    ] {
        let tls = QuicTls::client(
            &client_with_suite(suite),
            SERVER_NAME,
            &client_params(),
            ODCID,
        )
        .unwrap();
        let mut client = Peer::new(tls, SERVER_SCID);
        let mut server = Peer::new(new_server(), CLIENT_SCID);
        run(&mut client, &mut server, |f| f).unwrap();
        assert_eq!(client.tls.negotiated_cipher(), Some(cipher));
        assert_eq!(server.tls.negotiated_cipher(), Some(cipher));
        let mut ck = client.keys[2].take().unwrap().into_one_rtt(0).unwrap();
        let mut sk = server.keys[2].take().unwrap().into_one_rtt(0).unwrap();
        assert_eq!(ck.cipher(), cipher);
        let mut pkt = short_packet(&mut sk, CLIENT_SCID, 3, b"suite");
        assert_eq!(
            open_short(&mut ck, &mut pkt, CLIENT_SCID.len(), 3),
            b"suite"
        );
    }
}

#[test]
fn reordered_split_and_duplicated_crypto_data() {
    let (mut client, mut server) = pair();
    let mut flights = 0;
    run(&mut client, &mut server, |frames| {
        flights += 1;
        // Split every frame into 7-byte pieces, send them in reverse, and
        // repeat every third piece.
        let mut pieces = Vec::new();
        for (space, offset, data) in frames {
            for (i, chunk) in data.chunks(7).enumerate() {
                let piece = (space, offset + 7 * i as u64, chunk.to_vec());
                if i % 3 == 0 {
                    pieces.push(piece.clone());
                }
                pieces.push(piece);
            }
        }
        // Reverse within each space; spaces stay in order, as a receiver
        // only decrypts a later space once it holds its keys.
        pieces.sort_by_key(|p| p.0);
        let mut out = Vec::new();
        for space in Space::ALL {
            let mut s: Vec<_> = pieces.iter().filter(|p| p.0 == space).cloned().collect();
            s.reverse();
            out.extend(s);
        }
        out
    })
    .unwrap();
    assert!(flights > 2);
    assert_eq!(
        secret(&client, Space::Data).0,
        secret(&server, Space::Data).1
    );
}

#[test]
fn retry_rederives_initial_keys_and_is_authenticated() {
    const RETRY_SCID: &[u8] = &[0x77; 8];
    let mut client = Peer::new(new_client(), SERVER_SCID);
    // The client's first flight is answered with a Retry instead.
    let first_flight = client.send();
    client.tls.on_retry(RETRY_SCID).unwrap();
    client.drain();
    assert!(client.tls.on_retry(&[0x78; 8]).is_err(), "only one Retry");
    let server = QuicTls::server(
        &server_setup(),
        &server_params(),
        ServerCids {
            original_dcid: ODCID,
            client_scid: CLIENT_SCID,
            retry_scid: Some(RETRY_SCID),
        },
    )
    .unwrap();
    let mut server = Peer::new(server, CLIENT_SCID);
    assert_eq!(
        secret(&client, Space::Initial).0,
        secret(&server, Space::Initial).1
    );
    // The ClientHello is retransmitted under the new Initial keys.
    client.outgoing = first_flight;
    run(&mut client, &mut server, |f| f).unwrap();
    let p = client.peer_params.as_ref().unwrap();
    assert_eq!(p.retry_source_connection_id.unwrap().as_bytes(), RETRY_SCID);
}

#[test]
fn retry_without_server_retry_scid_is_rejected() {
    let mut client = Peer::new(new_client(), SERVER_SCID);
    let first_flight = client.send();
    client.tls.on_retry(&[0x77; 8]).unwrap();
    client.drain();
    // A server that does not admit to the Retry.
    let server = QuicTls::server(
        &server_setup(),
        &server_params(),
        ServerCids {
            original_dcid: ODCID,
            client_scid: CLIENT_SCID,
            retry_scid: None,
        },
    )
    .unwrap();
    let mut server = Peer::new(server, CLIENT_SCID);
    // (Its Initial keys would not even match the client's; the harness
    // passes CRYPTO data directly, so the parameter check is what fails.)
    client.outgoing = first_flight;
    let err = run(&mut client, &mut server, |f| f).unwrap_err();
    assert!(err.client);
    assert_eq!(
        err.error,
        TlsError::TransportParameters(TransportParameterError::Missing(
            id::RETRY_SOURCE_CONNECTION_ID
        ))
    );
    assert_eq!(
        client.tls.error_code(),
        Some(code::TRANSPORT_PARAMETER_ERROR)
    );
}

fn tls_alert(err: &TlsError) -> Option<u8> {
    match err {
        TlsError::Tls { alert, .. } => *alert,
        TlsError::NoApplicationProtocol => Some(120),
        other => panic!("expected a TLS error, got {other:?}"),
    }
}

#[test]
fn untrusted_certificate_is_rejected() {
    let server = QuicTls::server(
        &server_setup_with(ROGUE_CERT, ROGUE_KEY, ALPN),
        &server_params(),
        ServerCids {
            original_dcid: ODCID,
            client_scid: CLIENT_SCID,
            retry_scid: None,
        },
    )
    .unwrap();
    let mut client = Peer::new(new_client(), SERVER_SCID);
    let mut server = Peer::new(server, CLIENT_SCID);
    let err = run(&mut client, &mut server, |f| f).unwrap_err();
    assert!(err.client);
    assert_eq!(tls_alert(&err.error), Some(48), "unknown_ca");
    assert_eq!(err.error.quic_error_code(), code::CRYPTO_ERROR + 48);
    assert!(!client.tls.is_handshake_complete());
    assert_eq!(client.tls.key_state(Space::Data), KeyState::Pending);
    // The driver is closed after a fatal error.
    assert_eq!(
        client.tls.on_crypto_frame(Space::Handshake, 0, b"x"),
        Err(TlsError::Closed {
            code: code::CRYPTO_ERROR + 48
        })
    );
}

#[test]
fn wrong_server_name_is_rejected() {
    let tls = QuicTls::client(&client_setup(), "other.test", &client_params(), ODCID).unwrap();
    let mut client = Peer::new(tls, SERVER_SCID);
    let mut server = Peer::new(new_server(), CLIENT_SCID);
    let err = run(&mut client, &mut server, |f| f).unwrap_err();
    assert!(err.client);
    assert_eq!(tls_alert(&err.error), Some(42), "bad_certificate");
}

#[test]
fn expired_certificate_is_rejected() {
    let setup = ClientSetup::with_time(roots(), vec![ALPN.to_vec()], time(7_258_118_400)).unwrap(); // 2200
    let tls = QuicTls::client(&setup, SERVER_NAME, &client_params(), ODCID).unwrap();
    let mut client = Peer::new(tls, SERVER_SCID);
    let mut server = Peer::new(new_server(), CLIENT_SCID);
    let err = run(&mut client, &mut server, |f| f).unwrap_err();
    assert!(err.client);
    assert_eq!(tls_alert(&err.error), Some(45), "certificate_expired");
}

#[test]
fn alpn_mismatch_is_rejected() {
    let server = QuicTls::server(
        &server_setup_with(SERVER_CERT, SERVER_KEY, b"other-proto"),
        &server_params(),
        ServerCids {
            original_dcid: ODCID,
            client_scid: CLIENT_SCID,
            retry_scid: None,
        },
    )
    .unwrap();
    let mut client = Peer::new(new_client(), SERVER_SCID);
    let mut server = Peer::new(server, CLIENT_SCID);
    let err = run(&mut client, &mut server, |f| f).unwrap_err();
    assert!(!err.client);
    assert_eq!(tls_alert(&err.error), Some(120), "no_application_protocol");
    assert_eq!(err.error.quic_error_code(), 0x178);
}

#[test]
fn transport_parameter_cid_mismatches_are_rejected() {
    // The server claims a different original DCID.
    let server = QuicTls::server(
        &server_setup(),
        &server_params(),
        ServerCids {
            original_dcid: &[9, 9, 9, 9, 9, 9, 9, 9],
            client_scid: CLIENT_SCID,
            retry_scid: None,
        },
    )
    .unwrap();
    let mut client = Peer::new(new_client(), SERVER_SCID);
    let mut server = Peer::new(server, CLIENT_SCID);
    // Initial keys depend only on the client's DCID, so the TLS layer still
    // connects; the parameter check catches the substitution.
    let err = run(&mut client, &mut server, |f| f).unwrap_err();
    assert!(err.client);
    assert_eq!(
        err.error,
        TlsError::TransportParameters(TransportParameterError::ConnectionIdMismatch(
            id::ORIGINAL_DESTINATION_CONNECTION_ID
        ))
    );

    // The server's packets carried a SCID other than its parameter.
    let mut client = Peer::new(new_client(), &[1, 2, 3]);
    let mut server = Peer::new(new_server(), CLIENT_SCID);
    let err = run(&mut client, &mut server, |f| f).unwrap_err();
    assert_eq!(
        err.error,
        TlsError::TransportParameters(TransportParameterError::ConnectionIdMismatch(
            id::INITIAL_SOURCE_CONNECTION_ID
        ))
    );

    // The client's packets carried a SCID other than its parameter.
    let server = QuicTls::server(
        &server_setup(),
        &server_params(),
        ServerCids {
            original_dcid: ODCID,
            client_scid: &[0xee],
            retry_scid: None,
        },
    )
    .unwrap();
    let mut client = Peer::new(new_client(), SERVER_SCID);
    let mut server = Peer::new(server, &[0xee]);
    let err = run(&mut client, &mut server, |f| f).unwrap_err();
    assert!(!err.client);
    assert_eq!(err.error.quic_error_code(), code::TRANSPORT_PARAMETER_ERROR);
}

#[test]
fn invalid_local_parameters_are_refused_before_sending() {
    let mut p = client_params();
    p.stateless_reset_token = Some([0; 16]);
    assert_eq!(
        QuicTls::client(&client_setup(), SERVER_NAME, &p, ODCID).err(),
        Some(TlsError::TransportParameters(
            TransportParameterError::ServerOnly(id::STATELESS_RESET_TOKEN)
        ))
    );
    let mut p = client_params();
    p.initial_source_connection_id = None;
    assert!(matches!(
        QuicTls::client(&client_setup(), SERVER_NAME, &p, ODCID),
        Err(TlsError::TransportParameters(
            TransportParameterError::Missing(_)
        ))
    ));
    let mut p = server_params();
    p.ack_delay_exponent = 21;
    assert!(
        QuicTls::server(
            &server_setup(),
            &p,
            ServerCids {
                original_dcid: ODCID,
                client_scid: CLIENT_SCID,
                retry_scid: None
            }
        )
        .is_err()
    );
}

#[test]
fn crypto_space_misuse_is_a_typed_error() {
    // Handshake data before Handshake keys exist.
    let mut c = new_client();
    assert_eq!(
        c.on_crypto_frame(Space::Handshake, 0, b"early"),
        Err(TlsError::SpaceUnavailable(Space::Handshake))
    );
    assert_eq!(c.error_code(), Some(code::PROTOCOL_VIOLATION));

    // Server Initial data before its SCID was recorded.
    let mut c = new_client();
    assert!(matches!(
        c.on_crypto_frame(Space::Initial, 0, b"x"),
        Err(TlsError::ProtocolViolation(_))
    ));

    // Garbage instead of a ClientHello.
    let mut s = new_server();
    let err = s
        .on_crypto_frame(Space::Initial, 0, &[0xff; 64])
        .unwrap_err();
    assert!(matches!(err, TlsError::Tls { .. }));
    assert!((code::CRYPTO_ERROR..code::CRYPTO_ERROR + 0x100).contains(&err.quic_error_code()));

    // Overflowing the bounded CRYPTO buffer.
    let mut s = new_server().with_crypto_buffer(64, 4).unwrap();
    let err = s
        .on_crypto_frame(Space::Initial, 100, b"far ahead")
        .unwrap_err();
    assert_eq!(err.quic_error_code(), code::CRYPTO_BUFFER_EXCEEDED);

    // Offsets past 2^62 - 1.
    let mut s = new_server();
    let err = s
        .on_crypto_frame(Space::Initial, (1 << 62) - 2, b"xyz")
        .unwrap_err();
    assert_eq!(err.quic_error_code(), code::FRAME_ENCODING_ERROR);

    // HANDSHAKE_DONE is client-bound only, and only after completion.
    let mut s = new_server();
    assert!(s.on_handshake_done_received().is_err());
    assert_eq!(s.error_code(), Some(code::PROTOCOL_VIOLATION));
    let mut c = new_client();
    assert!(c.on_handshake_done_received().is_err());
}

#[test]
fn discarded_and_superseded_spaces_reject_new_data() {
    let (mut client, mut server) = pair();
    run(&mut client, &mut server, |f| f).unwrap();
    assert_eq!(
        server.tls.on_crypto_frame(Space::Initial, 0, b"late"),
        Err(TlsError::SpaceDiscarded(Space::Initial))
    );

    // Before confirmation, Initial is superseded (read level is past it):
    // repeats are fine, new bytes are a PROTOCOL_VIOLATION (RFC 9001 §4.1.3).
    let (mut client, mut server) = pair();
    let ch = client.send();
    server.receive(ch).unwrap();
    let to_client = server.send();
    client.receive(to_client.clone()).unwrap();
    let initial = to_client.iter().find(|f| f.0 == Space::Initial).unwrap();
    // Duplicate server Initial bytes: accepted.
    client
        .tls
        .on_crypto_frame(Space::Initial, initial.1, &initial.2)
        .unwrap();
    // Beyond what the server sent: rejected.
    let end = initial.1 + initial.2.len() as u64;
    assert!(matches!(
        client.tls.on_crypto_frame(Space::Initial, end, b"more"),
        Err(TlsError::ProtocolViolation(_))
    ));
}

#[test]
fn key_update_refused_before_confirmation() {
    let (mut client, mut server) = pair();
    // Run until the client completes but before HANDSHAKE_DONE.
    let f = client.send();
    server.receive(f).unwrap();
    let f = server.send();
    client.receive(f).unwrap();
    assert!(client.tls.is_handshake_complete());
    assert!(!client.tls.is_handshake_confirmed());
    assert!(!client.tls.key_update_permitted());
    let mut ck = client.keys[2].take().unwrap().into_one_rtt(0).unwrap();
    assert_eq!(
        client.tls.initiate_key_update(&mut ck),
        Err(TlsError::KeyUpdate(
            "key update before the handshake is confirmed"
        ))
    );
    // An acknowledged 1-RTT packet confirms the handshake (RFC 9001 §4.1.2).
    client.tls.on_one_rtt_ack_received();
    client.drain();
    assert!(client.tls.is_handshake_confirmed());
    assert!(client.log.iter().any(|e| e == "discard Handshake"));
}
