//! Session resumption and 0-RTT (wave six, issue #247): tickets from a
//! first connection resume a second one, its 0-RTT secret matches on
//! both sides, a replayed ClientHello gets no 0-RTT keys (single-use
//! tickets), and a server that refuses early data is reported to the
//! client as a rejection.

mod common;

use common::*;
use vesper_warp_crypto::TAG_LEN;
use vesper_warp_tls::{
    ClientSetup, EarlyData, KeyState, QuicTls, ServerCids, ServerSetup, Side, Space,
};

fn client_peer(setup: &ClientSetup) -> Peer {
    let tls = QuicTls::client(setup, SERVER_NAME, &client_params(), ODCID).unwrap();
    Peer::new(tls, SERVER_SCID)
}

fn server(setup: &ServerSetup) -> Peer {
    let cids = ServerCids {
        original_dcid: ODCID,
        client_scid: CLIENT_SCID,
        retry_scid: None,
    };
    Peer::new(
        QuicTls::server(setup, &server_params(), cids).unwrap(),
        CLIENT_SCID,
    )
}

/// A full handshake whose session tickets reach the client.
fn first_connection(c: &ClientSetup, s: &ServerSetup) {
    let (mut client, mut server) = (client_peer(c), server(s));
    run(&mut client, &mut server, |f| f).unwrap();
    assert!(!client.tls.is_resumed());
    assert_eq!(client.tls.early_data(), EarlyData::None);
    assert!(client.zero_rtt.is_none());
}

/// Seals a 0-RTT long-header packet with the client's key and opens it
/// with the server's.
fn early_packet_roundtrip(client: &mut Peer, server: &mut Peer) {
    let mut sealer = client.zero_rtt.take().unwrap().into_sealer().unwrap();
    let mut opener = server.zero_rtt.take().unwrap().into_opener().unwrap();
    let payload = [0x01u8; 24]; // PING frames
    // 0-RTT long header: 0xd1 (2-byte PN), v1, empty CIDs, length.
    let mut buf = vec![0xd1, 0, 0, 0, 1, 0, 0];
    buf.extend_from_slice(&[0x40, 2 + payload.len() as u8 + TAG_LEN as u8, 0x00, 0x03]);
    let pn_offset = buf.len() - 2;
    buf.extend_from_slice(&payload);
    buf.extend_from_slice(&[0; TAG_LEN]);
    let ticket = sealer.ticket(3).unwrap();
    sealer
        .seal(ticket, &mut buf, pn_offset, payload.len())
        .unwrap();
    let o = opener
        .open_packet(&mut buf, pn_offset, |_| Some(3))
        .unwrap();
    assert_eq!(&buf[o.header_len..o.header_len + o.payload_len], &payload);
}

#[test]
fn resumed_session_carries_zero_rtt_keys_both_ways() {
    let c = client_setup().with_zero_rtt();
    let s = server_setup().with_zero_rtt();
    assert!(c.zero_rtt() && s.zero_rtt());
    first_connection(&c, &s);

    let mut client = client_peer(&c);
    assert_eq!(client.tls.early_data(), EarlyData::Offered);
    assert_eq!(client.tls.zero_rtt_key_state(), KeyState::Installed);
    let remembered = client.remembered.clone().expect("remembered parameters");
    let sp = server_params();
    assert_eq!(remembered.initial_max_data, sp.initial_max_data);
    assert_eq!(remembered.initial_max_streams_uni, sp.initial_max_streams_uni);
    assert_eq!(client.zero_rtt.as_ref().unwrap().side(), Side::Client);
    // The ClientHello reaches the server, which accepts early data.
    let mut server = server(&s);
    let hello = client.send();
    server.receive(hello).unwrap();
    assert_eq!(server.tls.early_data(), EarlyData::Accepted);
    assert!(server.log.iter().any(|l| l == "0rtt keys"));
    early_packet_roundtrip(&mut client, &mut server);
    run(&mut client, &mut server, |f| f).unwrap();
    assert_eq!(client.tls.early_data(), EarlyData::Accepted);
    assert!(client.log.iter().any(|l| l == "early Accepted"));
    assert!(client.tls.is_resumed() && server.tls.is_resumed());
    // The server's own transport parameters replaced the remembered ones
    // once EncryptedExtensions arrived and were authenticated.
    let fresh = client.peer_params.clone().unwrap();
    assert_eq!(
        fresh.initial_source_connection_id.unwrap().as_bytes(),
        SERVER_SCID
    );
}

#[test]
fn a_replayed_client_hello_gets_no_zero_rtt() {
    let c = client_setup().with_zero_rtt();
    let s = server_setup().with_zero_rtt();
    first_connection(&c, &s);
    let mut client = client_peer(&c);
    let hello = client.send();
    let mut accepted = server(&s);
    accepted.receive(hello.clone()).unwrap();
    assert!(accepted.zero_rtt.is_some());
    // The same ClientHello again: its ticket was taken from the store, so
    // the second server neither resumes nor accepts early data.
    let mut replay = server(&s);
    replay.receive(hello).unwrap();
    assert!(replay.zero_rtt.is_none());
    assert_eq!(replay.tls.early_data(), EarlyData::None);
    assert_eq!(replay.tls.zero_rtt_key_state(), KeyState::Pending);
}

#[test]
fn a_server_refusing_early_data_is_a_clean_rejection() {
    let c = client_setup().with_zero_rtt();
    let s = server_setup().with_zero_rtt();
    first_connection(&c, &s);
    // Same session store, early data turned off.
    let refusing =
        ServerSetup::from_config((**s.config()).clone(), vec![ALPN.to_vec()]).unwrap();
    assert!(!refusing.zero_rtt());
    let mut client = client_peer(&c);
    assert_eq!(client.tls.early_data(), EarlyData::Offered);
    let mut server = server(&refusing);
    run(&mut client, &mut server, |f| f).unwrap();
    assert!(server.zero_rtt.is_none());
    assert_eq!(client.tls.early_data(), EarlyData::Rejected);
    assert!(client.log.iter().any(|l| l == "early Rejected"));
    // The rejection precedes completion, and the session still resumed.
    let rej = client
        .log
        .iter()
        .position(|l| l == "early Rejected")
        .unwrap();
    let done = client.log.iter().position(|l| l == "complete").unwrap();
    assert!(rej < done);
    assert!(client.tls.is_resumed());
    assert_eq!(client.tls.key_state(Space::Data), KeyState::Installed);
}

#[test]
fn an_unknown_ticket_falls_back_to_a_full_handshake() {
    let c = client_setup().with_zero_rtt();
    first_connection(&c, &server_setup().with_zero_rtt());
    // A different server (its own session store) cannot resume.
    let mut client = client_peer(&c);
    let mut server = server(&server_setup().with_zero_rtt());
    run(&mut client, &mut server, |f| f).unwrap();
    assert_eq!(client.tls.early_data(), EarlyData::Rejected);
    assert!(!client.tls.is_resumed());
}

#[test]
fn zero_rtt_is_off_unless_both_sides_opt_in() {
    let c = client_setup();
    let s = server_setup().with_zero_rtt();
    first_connection(&c, &s);
    let mut client = client_peer(&c);
    assert!(client.zero_rtt.is_none());
    assert_eq!(client.tls.early_data(), EarlyData::None);
    let mut server = server(&s);
    run(&mut client, &mut server, |f| f).unwrap();
    // Resumed, but without early data.
    assert!(client.tls.is_resumed());
    assert!(server.zero_rtt.is_none());
    // Keys of the wrong side are refused.
    let c0 = client_setup().with_zero_rtt();
    first_connection(&c0, &s);
    let mut again = client_peer(&c0);
    let k = again.zero_rtt.take().unwrap();
    assert!(k.into_opener().is_err());
}
