//! Wave seven lane 16 (issue #339): Chorus sessions over Warp connections
//! opened by the real TLS 1.3 handshake (mutual authentication against the
//! test PKI in `tests/fixtures/pki`), and the connection lifecycle under
//! session traffic: key updates and CONNECTION_CLOSE.

mod common;

use common::{CA, HOSTS, NOW_SECS, ROGUE_CA, ROGUE_NODE1, credential, pki};
use std::time::Duration;
use vesper_chorus::session::{
    Branch, Case, ChorusError, End, Fault, LeakAuditor, NetFail, OfferN, Rec, SelectN, Session,
    Var, Z,
};
use vesper_chorus::warpnet::{ALPN, WarpNet, WarpNetConfig, WarpNetError, WarpTls};
use vesper_chorus::{branches, parse_choreography, role, u64_message, unit_message};
use vesper_warp::endpoint::ConnectionError;
use vesper_warp::handshake::HandshakePhase;
use vesper_warp::handshake::tls::rustls::pki_types::{CertificateDer, UnixTime};

const STREAM: &str = "
roles P, C;
rec L {
    P -> C {
        Item(u64) { continue L; }
        Fin() { }
    }
}
";

role!(RP = "P");
role!(RC = "C");
u64_message!(Item = "Item", "u64");
unit_message!(Fin = "Fin");

type Producer = Rec<SelectN<RC, branches![Branch<Item, Var<Z>>, Branch<Fin, End>]>>;
type Consumer = Rec<OfferN<RP, branches![Branch<Item, Var<Z>>, Branch<Fin, End>]>>;

fn err(f: Fault) -> ChorusError {
    f.into_error()
}

fn lossy(nodes: u32, seed: u64) -> WarpNetConfig {
    WarpNetConfig {
        nodes,
        seed,
        loss_ppm: 100_000,
        reorder_ppm: 50_000,
        duplicate_ppm: 10_000,
        ..WarpNetConfig::default()
    }
}

/// Producer on host `p`, consumer on host `c`; `between(i, net)` runs after
/// item `i` was sent and received.
fn stream_items(
    net: &WarpNet,
    p: u32,
    c: u32,
    items: u64,
    channel: u64,
    mut between: impl FnMut(u64, &WarpNet),
) -> Result<Vec<u64>, ChorusError> {
    let chor = parse_choreography(STREAM).unwrap();
    let auditor = LeakAuditor::new();
    let placement = [p, c];
    let tx = Session::<Producer>::bind(
        &chor,
        "P",
        channel,
        net.transport(p, &placement).unwrap(),
        &auditor,
    )?;
    let rx = Session::<Consumer>::bind(
        &chor,
        "C",
        channel,
        net.transport(c, &placement).unwrap(),
        &auditor,
    )?;
    let (mut tx, mut rx) = (tx.enter(), rx.enter());
    let mut got = Vec::new();
    for i in 0..items {
        tx = tx.select(&Item(i * 7)).map_err(err)?.repeat();
        rx = match rx.offer().map_err(err)?.split().map_err(err)? {
            Case::This(Item(v), s) => {
                got.push(v);
                s.repeat()
            }
            Case::Other(_) => panic!("Fin before the last item"),
        };
        between(i, net);
    }
    tx.select(&Fin).map_err(err)?.close();
    match rx.offer().map_err(err)?.split().map_err(err)? {
        Case::This(Item(_), _) => panic!("item after Fin"),
        Case::Other(rest) => match rest.split().map_err(err)? {
            Case::This(Fin, s) => s.close(),
            Case::Other(rest) => return Err(err(rest.exhausted())),
        },
    }
    assert_eq!(auditor.open(), 0);
    Ok(got)
}

#[test]
fn every_link_is_a_mutually_authenticated_tls_connection() {
    for seed in [1, 2, 3] {
        let net = WarpNet::new(lossy(4, seed), &pki()).unwrap();
        let st = net.stats();
        assert_eq!(st.handshakes, 6, "one handshake per host pair");
        assert!(
            st.dropped > 0,
            "the handshakes ran over a lossy wire: {st:?}"
        );
        for a in 0..4u32 {
            for b in a + 1..4 {
                let info = net.link_info(a, b).unwrap();
                assert_eq!((info.client, info.server), (a, b));
                assert_eq!(info.phase, [HandshakePhase::Confirmed; 2]);
                assert_eq!(info.alpn, [Some(ALPN.to_vec()), Some(ALPN.to_vec())]);
                // The client saw the server host's certificate and the
                // server saw the client host's: mutual authentication.
                assert_eq!(
                    info.peer_certificate[0].as_deref(),
                    Some(HOSTS[b as usize].0)
                );
                assert_eq!(
                    info.peer_certificate[1].as_deref(),
                    Some(HOSTS[a as usize].0)
                );
                assert!(info.cipher[0].is_some());
                assert_eq!(info.cipher[0], info.cipher[1]);
                assert!(info.handshake_steps > 0);
                assert_eq!(info.error, [None, None]);
            }
        }
        // Sessions run over the handshake keys, in both directions of a
        // link and across every pair.
        for (p, c) in [(0, 3), (3, 0), (1, 2), (2, 1)] {
            let got = stream_items(&net, p, c, 12, 100 + u64::from(p * 4 + c), |_, _| {}).unwrap();
            assert_eq!(got, (0..12).map(|i| i * 7).collect::<Vec<_>>());
        }
        let st = net.stats();
        assert_eq!(st.authentication_failures, 0, "{st:?}");
        assert_eq!(st.events_dropped, 0);
        assert!(net.link_failure().is_none());
        assert!(net.settle(200_000), "{:?}", net.describe_open_streams());
    }
}

fn handshake_error(tls: &WarpTls, nodes: u32) -> (u32, u32, String) {
    match WarpNet::new(
        WarpNetConfig {
            nodes,
            ..WarpNetConfig::default()
        },
        tls,
    ) {
        Err(WarpNetError::Handshake {
            client,
            server,
            reason,
        }) => (client, server, reason),
        Err(e) => panic!("expected a handshake failure, got {e}"),
        Ok(_) => panic!("expected a handshake failure, the network was built"),
    }
}

#[test]
fn untrusted_or_misnamed_hosts_fail_the_handshake() {
    // Host 1's certificate is from a CA the others do not trust. Host 0
    // (client) rejects it as a server certificate.
    let mut tls = pki();
    tls.hosts[1] = credential("node1.chorus.test", ROGUE_NODE1.0, ROGUE_NODE1.1);
    let (c, s, reason) = handshake_error(&tls, 3);
    assert_eq!((c, s), (0, 1));
    assert!(reason.contains("Handshake"), "{reason}");

    // The same rogue host as a client: host 2 (server) rejects its client
    // certificate. Link (0, 1) is not built because host 0 is checked
    // first, so use two hosts with host 0 rogue.
    let mut tls = pki();
    tls.hosts[0] = credential("node1.chorus.test", ROGUE_NODE1.0, ROGUE_NODE1.1);
    tls.hosts[0].name = "node0.chorus.test".into();
    let (c, s, _) = handshake_error(&tls, 2);
    assert_eq!((c, s), (0, 1));

    // A valid certificate of the wrong host. Host 2 presents node1's
    // certificate: as a server, host 0 rejects it for the name node2.
    let mut tls = pki();
    tls.hosts[2] = credential("node2.chorus.test", HOSTS[1].0, HOSTS[1].1);
    let (c, s, _) = handshake_error(&tls, 3);
    assert_eq!((c, s), (0, 2));

    // As a client: host 0 presents node1's certificate to host 1, whose
    // verifier expects a certificate for node0 (the chain itself is valid).
    let mut tls = pki();
    tls.hosts[0] = credential("node0.chorus.test", HOSTS[1].0, HOSTS[1].1);
    let (c, s, reason) = handshake_error(&tls, 2);
    assert_eq!((c, s), (0, 1));
    assert!(reason.contains("Handshake"), "{reason}");

    // Nobody trusts the real CA.
    let mut tls = pki();
    tls.roots = vec![CertificateDer::from(ROGUE_CA.to_vec())];
    assert_eq!(handshake_error(&tls, 2).0, 0);

    // Certificates checked after their validity (they end in 2126).
    let mut tls = pki();
    tls.time = Some(UnixTime::since_unix_epoch(Duration::from_secs(
        NOW_SECS + 200 * 365 * 86_400,
    )));
    assert_eq!(handshake_error(&tls, 2).0, 0);

    // A wire that drops everything never completes the handshake.
    let r = WarpNet::new(
        WarpNetConfig {
            nodes: 2,
            loss_ppm: 999_999,
            handshake_steps: 300,
            ..WarpNetConfig::default()
        },
        &pki(),
    );
    match r {
        Err(WarpNetError::Handshake { reason, .. }) => {
            assert!(reason.contains("not confirmed"), "{reason}")
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("built"),
    }

    // Configuration errors are typed, not handshake failures.
    let mut tls = pki();
    tls.hosts.truncate(1);
    assert!(matches!(
        WarpNet::new(WarpNetConfig::default(), &tls),
        Err(WarpNetError::Config(_))
    ));
    let mut tls = pki();
    tls.roots.clear();
    assert!(matches!(
        WarpNet::new(WarpNetConfig::default(), &tls),
        Err(WarpNetError::Config(_))
    ));
    let mut tls = pki();
    tls.roots = vec![CertificateDer::from(b"not a certificate".to_vec())];
    assert!(matches!(
        WarpNet::new(WarpNetConfig::default(), &tls),
        Err(WarpNetError::Tls(_))
    ));
    let mut tls = pki();
    tls.hosts[1].name = "not a dns name!".into();
    assert!(WarpNet::new(WarpNetConfig::default(), &tls).is_err());
    let _ = CA;
}

#[test]
fn key_updates_under_session_traffic_keep_the_session() {
    let net = WarpNet::new(lossy(2, 11), &pki()).unwrap();
    // Host 0 updates while it streams to host 1, then host 1 while it
    // streams back (a host that only sends ACKs has no acknowledged packet
    // in its current key phase, so RFC 9001 §6.1 does not let it update).
    let got = stream_items(&net, 0, 1, 40, 5, |i, net| {
        if i == 10 {
            net.key_update(0, 1).unwrap();
        }
    })
    .unwrap();
    assert_eq!(got.len(), 40);
    let got = stream_items(&net, 1, 0, 40, 6, |i, net| {
        if i == 10 {
            net.key_update(1, 0).unwrap();
        }
    })
    .unwrap();
    assert_eq!(got.len(), 40);
    let info = net.link_info(0, 1).unwrap();
    // Two updates: each side sends and receives under generation 2.
    assert_eq!(info.generations, [(2, 2), (2, 2)], "{info:?}");
    let st = net.stats();
    assert_eq!(st.key_updates, 2);
    assert_eq!(st.authentication_failures, 0, "{st:?}");
    assert!(net.link_failure().is_none());
    // A second update before the first is acknowledged is refused, as RFC
    // 9001 §6.5 requires; it is a typed error, not a failure of the link.
    let net = WarpNet::new(WarpNetConfig::default(), &pki()).unwrap();
    stream_items(&net, 0, 1, 3, 1, |_, _| {}).unwrap();
    net.key_update(0, 1).unwrap();
    assert!(matches!(net.key_update(0, 1), Err(WarpNetError::Warp(_))));
    assert!(net.key_update(0, 5).is_err());
}

#[test]
fn connection_close_fails_sessions_on_both_hosts() {
    let net = WarpNet::new(lossy(3, 5), &pki()).unwrap();
    let chor = parse_choreography(STREAM).unwrap();
    let auditor = LeakAuditor::new();
    let placement = [0, 1];
    let tx = Session::<Producer>::bind(
        &chor,
        "P",
        1,
        net.transport(0, &placement).unwrap(),
        &auditor,
    )
    .unwrap()
    .enter();
    let rx = Session::<Consumer>::bind(
        &chor,
        "C",
        1,
        net.transport(1, &placement).unwrap(),
        &auditor,
    )
    .unwrap()
    .enter();
    let tx = tx.select(&Item(1)).map_err(err).unwrap().repeat();
    let rx = match rx
        .offer()
        .map_err(err)
        .unwrap()
        .split()
        .map_err(err)
        .unwrap()
    {
        Case::This(Item(1), s) => s.repeat(),
        _ => panic!("first item"),
    };
    net.close_link(1, 0, 0x77).unwrap();
    // The consumer's next receive fails as Closed, well before the receive
    // timeout.
    let before = net.stats().steps;
    let e = rx.offer().map(drop).map_err(err).unwrap_err();
    assert!(matches!(e, ChorusError::Net(NetFail::Closed)), "{e:?}");
    assert!(net.stats().steps - before < 1_000);
    // The producer's host learns of the close from CONNECTION_CLOSE.
    for _ in 0..200 {
        net.step();
    }
    let info = net.link_info(0, 1).unwrap();
    assert_eq!(
        info.error[0],
        Some(ConnectionError::Peer {
            error_code: 0x77,
            frame_type: None
        }),
        "{info:?}"
    );
    assert_eq!(
        info.error[1],
        Some(ConnectionError::Application { error_code: 0x77 })
    );
    let e = tx.select(&Item(2)).map(drop).map_err(err).unwrap_err();
    assert!(matches!(e, ChorusError::Net(NetFail::Closed)), "{e:?}");
    assert!(net.link_failure().is_some());
    // Other links are unaffected.
    assert_eq!(stream_items(&net, 0, 2, 5, 2, |_, _| {}).unwrap().len(), 5);
    assert_eq!(auditor.open(), 0, "faulted sessions were released");
}
