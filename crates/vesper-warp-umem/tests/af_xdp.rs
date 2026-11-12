//! Real Linux AF_XDP execution on `lo` (copy or zero-copy, as the kernel grants).
//!
//! Each test probes first and prints `unavailable: <step>: errno ...` and
//! returns when the host refuses (no CAP_NET_RAW/CAP_BPF, no AF_XDP, no
//! generic XDP on `lo`). Passing here is evidence about this host's kernel
//! path for `lo`, not about any NIC driver, zero-copy DMA or line rate.
#![cfg(target_os = "linux")]

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use vesper_warp_umem::af_xdp::{AfXdpBackend, XdpRedirect, probe};
use vesper_warp_umem::{
    RetireOutcome, RxHarvested, UmemConfig, UmemFrame, XdpNic, Xsk, XskBackend, audit_driver,
};

fn config() -> UmemConfig {
    UmemConfig {
        frame_count: 64,
        frame_size: 2048,
        headroom: 0,
        fill_size: 64,
        rx_size: 64,
        tx_size: 64,
        completion_size: 64,
    }
}

fn open() -> Option<Xsk<AfXdpBackend>> {
    if let Err(e) = probe() {
        eprintln!("AF_XDP {e}");
        return None;
    }
    match AfXdpBackend::open(&config(), "lo", 0) {
        Ok(backend) => {
            eprintln!(
                "AF_XDP bound to lo queue 0 in {:?} mode (zero-copy refusal: {})",
                backend.bind_mode(),
                backend
                    .zero_copy_error()
                    .map_or("none".to_string(), |e| e.to_string())
            );
            Some(Xsk::new(config(), backend).expect("valid config"))
        }
        Err(e) => {
            eprintln!("AF_XDP {e}");
            None
        }
    }
}

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in bytes.chunks(2) {
        let word = u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
        sum += u32::from(word);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Ethernet (zero MACs, as on `lo`) + IPv4 127.0.0.1 → 127.0.0.1 + UDP
/// (checksum 0, which IPv4 permits) + payload.
fn udp_frame(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 12];
    f.extend_from_slice(&[0x08, 0x00]);
    let total = (20 + 8 + payload.len()) as u16;
    let mut ip = vec![
        0x45, 0, 0, 0, 0, 0, 0x40, 0, 64, 17, 0, 0, 127, 0, 0, 1, 127, 0, 0, 1,
    ];
    ip[2..4].copy_from_slice(&total.to_be_bytes());
    let csum = checksum(&ip);
    ip[10..12].copy_from_slice(&csum.to_be_bytes());
    f.extend_from_slice(&ip);
    f.extend_from_slice(&src_port.to_be_bytes());
    f.extend_from_slice(&dst_port.to_be_bytes());
    f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(payload);
    f
}

fn wait_completion(xsk: &mut Xsk<AfXdpBackend>) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let report = xsk.harvest_completions(16);
        if report.completed_leased + report.freed > 0 {
            return true;
        }
        let _ = xsk.backend_mut().wake_tx();
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn wait_rx(xsk: &mut Xsk<AfXdpBackend>) -> UmemFrame<RxHarvested> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match xsk.harvest_rx() {
            Ok(Some(frame)) => return frame,
            Ok(None) if Instant::now() < deadline => {
                xsk.backend_mut().wake_rx();
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => panic!("no AF_XDP rx frame within 2 s"),
            Err(e) => panic!("malformed rx descriptor from the kernel: {e:?}"),
        }
    }
}

fn payload(xsk: &Xsk<AfXdpBackend>, frame: &UmemFrame<RxHarvested>) -> Vec<u8> {
    let bytes = xsk.view_frame_bytes(frame).unwrap();
    bytes[42..].to_vec()
}

/// One test, because a queue admits one AF_XDP socket at a time.
///
/// The kernel drops packets with 127.0.0.0/8 addresses that arrive on `lo`
/// without a route attached (martians), so transmitted frames are observed by
/// redirecting them straight back into the socket with the same XDP program,
/// not by a kernel UDP socket.
#[test]
fn af_xdp_on_loopback_rx_turn_around_tx_completion() {
    let Some(mut xsk) = open() else { return };
    assert_eq!(xsk.replenish_fill(32), 32);
    // An unused destination port: only the XDP program will see it.
    let reserve = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let redirect = match XdpRedirect::attach_udp_port(xsk.backend(), port) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AF_XDP receive {e}");
            return;
        }
    };

    // 1. Kernel UDP socket -> lo -> generic XDP -> XSKMAP -> rx ring.
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.send_to(b"ping", ("127.0.0.1", port)).unwrap();
    let rx = wait_rx(&mut xsk);
    assert_eq!(payload(&xsk, &rx), b"ping");
    let bytes = xsk.view_frame_bytes(&rx).unwrap();
    assert_eq!(u16::from_be_bytes([bytes[36], bytes[37]]), port);

    // 2. In-place turn-around: same chunk, new payload, sent back to `port`,
    //    so the program redirects our own transmission into the rx ring.
    let len = rx.pkt_len();
    let chunk = rx.chunk_idx();
    let mut tx = xsk.turn_around_tx_in_place(rx, len).expect("same window");
    assert_eq!(tx.chunk_idx(), chunk);
    {
        let b = xsk.tx_bytes_mut(&mut tx).unwrap();
        let n = b.len();
        b[n - 4..].copy_from_slice(b"pong");
        b[40..42].copy_from_slice(&[0, 0]); // UDP checksum: none
    }
    let lease = xsk.submit_paced_tx(tx, 1, 0).expect("tx ring space");
    assert!(
        wait_completion(&mut xsk),
        "no completion for the turned-around frame"
    );
    assert_eq!(xsk.retire(lease).expect("own lease"), RetireOutcome::Freed);
    let echoed = wait_rx(&mut xsk);
    assert_eq!(payload(&xsk, &echoed), b"pong");
    xsk.recycle_to_fill_ring(echoed).expect("own frame");

    // 3. A fresh transmit frame from the free list, same loop.
    let packet = udp_frame(40000, port, b"fresh");
    let mut fresh = xsk.alloc_tx(packet.len() as u16).expect("free frame");
    xsk.tx_bytes_mut(&mut fresh)
        .unwrap()
        .copy_from_slice(&packet);
    let lease = xsk.submit_paced_tx(fresh, 2, 0).expect("tx ring space");
    assert!(
        wait_completion(&mut xsk),
        "no completion for the fresh frame"
    );
    let _ = xsk.retire(lease).expect("own lease");
    let looped = wait_rx(&mut xsk);
    assert_eq!(payload(&xsk, &looped), b"fresh");
    xsk.recycle_to_fill_ring(looped).expect("own frame");

    // 4. Layouts the filter must pass to the stack untouched: IPv4 with
    //    options (whose UDP *source* port sits where an option-free header
    //    keeps the destination port) and a non-initial fragment.
    let mut with_options = udp_frame(port, 40001, b"options");
    with_options[14] = 0x46;
    with_options.splice(34..34, [1u8, 1, 1, 1]); // four NOP options
    let total = (with_options.len() - 14) as u16;
    with_options[16..18].copy_from_slice(&total.to_be_bytes());
    with_options[24..26].copy_from_slice(&[0, 0]);
    let csum = checksum(&with_options[14..38]);
    with_options[24..26].copy_from_slice(&csum.to_be_bytes());
    let mut fragment = udp_frame(40002, port, b"fragment");
    fragment[20..22].copy_from_slice(&0x2001u16.to_be_bytes()); // MF, offset 8
    fragment[24..26].copy_from_slice(&[0, 0]);
    let csum = checksum(&fragment[14..34]);
    fragment[24..26].copy_from_slice(&csum.to_be_bytes());
    for (pn, packet) in [(3, with_options), (4, fragment)] {
        let mut frame = xsk.alloc_tx(packet.len() as u16).expect("free frame");
        xsk.tx_bytes_mut(&mut frame)
            .unwrap()
            .copy_from_slice(&packet);
        let lease = xsk.submit_paced_tx(frame, pn, 0).expect("tx ring space");
        assert!(wait_completion(&mut xsk), "no completion");
        let _ = xsk.retire(lease).expect("own lease");
    }
    std::thread::sleep(Duration::from_millis(200));
    xsk.backend_mut().wake_rx();
    assert!(
        xsk.harvest_rx().expect("no malformed descriptor").is_none(),
        "the filter redirected an IP-options packet or a fragment"
    );

    drop(redirect);
    audit_driver(&mut xsk).expect("driver accounting conserved");
    let stats = xsk.stats();
    assert_eq!(stats.rx_rejected, 0);
    eprintln!(
        "AF_XDP on lo executed ({:?} mode): UMEM_REG, ring mmaps, bind, XDP redirect rx, \
         in-place turn-around tx, completion; stats {stats:?}",
        xsk.backend().bind_mode()
    );
}
