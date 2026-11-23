//! Differential suite comparing the Vesper Part 3b HTTP/3 connection, client,
//! and WebTransport/session implementation against the frozen Rust baseline
//! (`0f5a9eba`).
//!
//! Each test feeds identical deterministic inputs into `ServerConnection`,
//! `ClientConnection`, and `Session` APIs, folds every observable result
//! (decoded requests, headers, bodies, trailers, interim responses, priority
//! write logs, connection/session/send errors, wire resets, and stats) into a
//! 128-bit digest, and compares against the golden digests recorded on
//! `0f5a9eba` in `tests/data/port_differential.golden`. Tests also cross-check
//! 1-byte/packet transport chunking against unchunked delivery.
//!
//! Set `PRISM_H3_DIFFERENTIAL_RECORD=1` to print golden digest lines from a
//! baseline build.

mod support;

use std::collections::BTreeMap;

use support::{Mem, pair};
use vesper_prism_h3::capsule;
use vesper_prism_h3::client::{ClientConnection, ClientError, ClientEvent, ClientRequest};
use vesper_prism_h3::connection::{
    Event, H3Config, HeadersPending, Responder, Response, ServerConnection, Session,
};
use vesper_prism_h3::error::Code;
use vesper_prism_h3::frame::{self, Settings};
use vesper_prism_h3::priority::Priority;
use vesper_prism_h3::qpack::{Encoder, EncoderConfig, Field};
use vesper_prism_h3::session::{DatagramPath, SessionError, SessionEvent};
use vesper_prism_h3::transport::Transport;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        self.0 = x;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push((self.next() >> 24) as u8);
        }
        out
    }

    fn ascii(&mut self, len: usize) -> String {
        const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-_";
        (0..len)
            .map(|_| ALPHA[self.below(ALPHA.len() as u64) as usize] as char)
            .collect()
    }
}

struct Results {
    name: &'static str,
    h1: u64,
    h2: u64,
    cases: u64,
}

impl Results {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            h1: 0xcbf2_9ce4_8422_2325,
            h2: 0x8422_2325_cbf2_9ce4,
            cases: 0,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.h1 = (self.h1 ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
            self.h2 = self
                .h2
                .rotate_left(7)
                .wrapping_add(u64::from(b))
                .wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
        self.h1 ^= (bytes.len() as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn record_debug(&mut self, tag: &str, v: impl std::fmt::Debug) {
        let s = format!("{tag}:{v:?}\n");
        self.feed(s.as_bytes());
        self.cases += 1;
    }

    fn record_bytes(&mut self, tag: &str, b: &[u8]) {
        self.feed(tag.as_bytes());
        self.feed(&b.len().to_be_bytes());
        self.feed(b);
        self.cases += 1;
    }

    fn finish(self) {
        let line = format!(
            "{} {} {:016x}{:016x}",
            self.name, self.cases, self.h1, self.h2
        );
        if std::env::var_os("PRISM_H3_DIFFERENTIAL_RECORD").is_some() {
            println!("{line}");
            println!("flip-gate differential differences=0 cases={}", self.cases);
            return;
        }
        let golden = include_str!("data/port_differential.golden");
        let want = golden
            .lines()
            .find(|l| l.split(' ').next() == Some(self.name))
            .unwrap_or_else(|| panic!("missing golden digest for {}", self.name));
        let differences = u64::from(line != want);
        println!(
            "flip-gate differential differences={differences} cases={}",
            self.cases
        );
        assert_eq!(line, want, "differential mismatch on {}", self.name);
    }
}

fn step_pair(
    client: &mut ClientConnection,
    server: &mut ServerConnection,
    c: &mut Mem,
    s: &mut Mem,
    rounds: usize,
) {
    for _ in 0..rounds {
        client.poll(c);
        server.poll(s);
    }
}

fn write_all(m: &mut Mem, id: u64, mut data: &[u8]) {
    while !data.is_empty() {
        let n = m.write(id, data).expect("raw write");
        assert!(n > 0);
        data = &data[n..];
    }
}

fn raw_control(m: &mut Mem) -> u64 {
    let ctrl = m.raw_open(true);
    let mut init = Vec::new();
    frame::put_varint(&mut init, frame::STREAM_CONTROL);
    Settings::default().encode(&mut init);
    write_all(m, ctrl, &init);
    ctrl
}

fn record_server_event(res: &mut Results, ev: &Event) {
    match ev {
        Event::Request(r) => {
            res.record_debug("srv_req_id", r.stream_id);
            res.record_debug("srv_req_head", &r.head);
            res.record_bytes("srv_req_body", &r.body);
            res.record_debug("srv_req_trailers", &r.trailers);
        }
        Event::StreamingRequest(sr) => {
            res.record_debug("srv_sreq_id", sr.stream_id);
            res.record_debug("srv_sreq_head", &sr.head);
        }
        Event::BodyReadable { stream_id } => {
            res.record_debug("srv_body_readable", stream_id);
        }
        Event::Connect(cr) => {
            res.record_debug("srv_connect", (cr.stream_id, &cr.head, cr.kind));
        }
        Event::Session(sev) => {
            res.record_debug("srv_sess", sev);
        }
        Event::RequestFailed { stream_id, code } => {
            res.record_debug("srv_req_fail", (stream_id, code));
        }
        Event::ResponseAbandoned { stream_id, code } => {
            res.record_debug("srv_resp_abandoned", (stream_id, code));
        }
        Event::GoAway(id) => {
            res.record_debug("srv_goaway", id);
        }
    }
}

fn record_client_event(res: &mut Results, ev: &ClientEvent) {
    res.record_debug("cli_ev", ev);
}

#[test]
fn differential_request_response_matrix() {
    let mut res = Results::new("req_resp_matrix");
    let mut rng = Rng::new(0x1111_2222_3333_4444);

    for &(cap, blocked) in &[(0, 0), (256, 4), (4096, 16)] {
        let mut unchunked_sig: Option<(u64, u64)> = None;
        for &wlim in &[0usize, 1, 7, 64] {
            let mut local_rng = Rng::new(0xaa55 ^ cap ^ blocked);
            let mut sub = Results::new("sub");
            let (mut c, mut s) = pair();
            c.write_limit = wlim;
            s.write_limit = wlim;
            let cfg = H3Config {
                settings: Settings {
                    qpack_max_table_capacity: cap,
                    qpack_blocked_streams: blocked,
                    max_field_section_size: Some(32 * 1024),
                    no_rfc7540_priorities: true,
                    ..Settings::default()
                },
                encoder: EncoderConfig {
                    max_table_capacity: cap,
                    ..EncoderConfig::default()
                },
                incremental_quantum: 512,
                ..H3Config::default()
            };
            let mut client = ClientConnection::new(cfg);
            let mut server = ServerConnection::new(cfg);
            let rounds = if wlim == 1 { 20 } else { 6 };
            step_pair(&mut client, &mut server, &mut c, &mut s, rounds);

            for wave in 0..5usize {
                let mut reqs = Vec::new();
                for k in 0..4usize {
                    let m = ["GET", "POST", "HEAD", "PUT"][(wave + k) % 4];
                    let path = format!("/{}/{}", local_rng.ascii(6), k);
                    let mut req = ClientRequest::get("example.test", &path);
                    req.method = m.into();
                    let pri = if k % 2 == 0 {
                        format!("u={}, i", k % 8)
                    } else {
                        format!("u={}", (k * 3) % 8)
                    };
                    req.fields.push(Field::new("priority", pri));
                    req.fields.push(Field::sensitive(
                        "authorization",
                        format!("tok-{}", local_rng.ascii(8)),
                    ));
                    if m == "POST" || m == "PUT" {
                        let blen = [0usize, 1, 63, 64, 513][local_rng.below(5) as usize];
                        req.body = local_rng.bytes(blen);
                    }
                    let id = client.send_request(&mut c, &req).unwrap();
                    res.record_debug("cli_send", id);
                    reqs.push(id);
                }
                step_pair(&mut client, &mut server, &mut c, &mut s, rounds);
                let mut handled = Vec::new();
                while let Some(ev) = server.poll_event() {
                    record_server_event(&mut sub, &ev);
                    record_server_event(&mut res, &ev);
                    if let Event::Request(r) = ev {
                        handled.push(*r);
                    }
                }
                for r in handled {
                    let mut resp_holder = r.responder;
                    let status = if r.head.method == "HEAD" {
                        200
                    } else {
                        [200u16, 204, 304, 404, 500][(r.stream_id as usize / 4) % 5]
                    };
                    if r.stream_id % 8 == 0 {
                        let interim_res = resp_holder.send_interim(
                            &mut server,
                            &mut s,
                            103,
                            vec![Field::new("link", "</a.js>; rel=preload")],
                        );
                        sub.record_debug("interim_res", interim_res);
                        res.record_debug("interim_res", interim_res);
                    }
                    let body = if matches!(status, 204 | 304) {
                        Vec::new()
                    } else {
                        local_rng.bytes(300 + (r.stream_id as usize % 200))
                    };
                    if r.stream_id % 12 == 4
                        && !matches!(status, 204 | 304)
                        && r.head.method != "HEAD"
                    {
                        let mut sr = resp_holder
                            .send_headers(
                                &mut server,
                                &mut s,
                                status,
                                vec![Field::new("x-mode", "stream")],
                                Some(body.len() as u64),
                            )
                            .unwrap();
                        let mid = body.len() / 2;
                        sr.send_data(&mut server, &mut s, &body[..mid]).unwrap();
                        sr.send_data(&mut server, &mut s, &body[mid..]).unwrap();
                        let sent = sr
                            .finish(&mut server, &mut s, vec![Field::new("x-trl", "yes")])
                            .unwrap();
                        sub.record_debug("sent_stream", &sent);
                        res.record_debug("sent_stream", &sent);
                    } else {
                        let sent = resp_holder
                            .respond(
                                &mut server,
                                &mut s,
                                Response {
                                    status,
                                    fields: vec![Field::new("x-mode", "whole")],
                                    body,
                                },
                            )
                            .unwrap();
                        sub.record_debug("sent_whole", &sent);
                        res.record_debug("sent_whole", &sent);
                    }
                }
                step_pair(&mut client, &mut server, &mut c, &mut s, rounds);
                while let Some(ev) = client.poll_event() {
                    record_client_event(&mut sub, &ev);
                    record_client_event(&mut res, &ev);
                }
            }
            sub.record_debug("srv_stats", server.stats());
            sub.record_debug("cli_stats", client.stats());
            res.record_debug("srv_stats", server.stats());
            res.record_debug("cli_stats", client.stats());
            let sig = (sub.h1, sub.h2);
            if let Some(prev) = unchunked_sig {
                assert_eq!(sig, prev, "chunking limit {wlim} diverged for cap={cap}");
            } else {
                unchunked_sig = Some(sig);
            }
        }
    }
    let _ = rng.next();
    res.finish();
}

#[test]
fn differential_streaming_bodies_and_backpressure() {
    let mut res = Results::new("streaming_and_backpressure");
    let mut rng = Rng::new(0x5555_6666_7777_8888);

    // 1. Streaming uploads with trailers, multiple chunks, and early server responses.
    for round in 0..16usize {
        let (mut c, mut s) = pair();
        if round % 2 == 1 {
            c.write_limit = 17;
            s.write_limit = 19;
        }
        let scfg = H3Config {
            streaming_request_bodies: true,
            stream_body_window: 128,
            ..H3Config::default()
        };
        let mut client = ClientConnection::new(H3Config::default());
        let mut server = ServerConnection::new(scfg);
        step_pair(&mut client, &mut server, &mut c, &mut s, 6);

        let total_len = 300 + round * 33;
        let mut req = ClientRequest::get("example.test", &format!("/upload/{round}"));
        req.method = "POST".into();
        req.fields
            .push(Field::new("content-length", total_len.to_string()));
        let sid = client.send_request_streaming(&mut c, &req).unwrap();
        let data = rng.bytes(total_len);
        let mut responders: BTreeMap<u64, Responder<HeadersPending>> = BTreeMap::new();
        let mut received = Vec::new();
        let mut off = 0usize;

        while off < data.len() {
            let chunk = (off + 77).min(data.len());
            let sb = client.send_body(&mut c, sid, &data[off..chunk]);
            res.record_debug("cli_send_body", sb);
            if sb.is_err() {
                break;
            }
            off = chunk;
            if off == data.len() {
                let fb = client.finish_body(&mut c, sid);
                res.record_debug("cli_finish_body", fb);
            }
            step_pair(&mut client, &mut server, &mut c, &mut s, 4);
            while let Some(ev) = server.poll_event() {
                record_server_event(&mut res, &ev);
                match ev {
                    Event::StreamingRequest(sr) => {
                        if round % 4 == 3 {
                            // Answer early while upload continues.
                            let sent = sr
                                .responder
                                .respond(
                                    &mut server,
                                    &mut s,
                                    Response {
                                        status: 202,
                                        fields: Vec::new(),
                                        body: b"early".to_vec(),
                                    },
                                )
                                .unwrap();
                            res.record_debug("early_sent", &sent);
                        } else {
                            responders.insert(sr.stream_id, sr.responder);
                        }
                    }
                    Event::BodyReadable { stream_id } => loop {
                        let mut buf = [0u8; 64];
                        let r = server.read_body(&mut s, stream_id, &mut buf).unwrap();
                        received.extend_from_slice(&buf[..r.len]);
                        if r.end {
                            let trailers = server.take_trailers(stream_id);
                            res.record_debug("srv_trailers", &trailers);
                            if let Some(resp) = responders.remove(&stream_id) {
                                let _ = resp
                                    .respond(
                                        &mut server,
                                        &mut s,
                                        Response {
                                            status: 200,
                                            fields: Vec::new(),
                                            body: received.len().to_be_bytes().to_vec(),
                                        },
                                    )
                                    .unwrap();
                            }
                            break;
                        }
                        if r.len == 0 {
                            break;
                        }
                    },
                    _ => {}
                }
            }
        }
        // Drain extra poll cycles in case body readable settled on final step.
        for _ in 0..6 {
            step_pair(&mut client, &mut server, &mut c, &mut s, 2);
            while let Some(ev) = server.poll_event() {
                record_server_event(&mut res, &ev);
                if let Event::BodyReadable { stream_id } = ev {
                    loop {
                        let mut buf = [0u8; 64];
                        let r = server.read_body(&mut s, stream_id, &mut buf).unwrap();
                        received.extend_from_slice(&buf[..r.len]);
                        if r.end {
                            let trailers = server.take_trailers(stream_id);
                            res.record_debug("srv_trailers", &trailers);
                            if let Some(resp) = responders.remove(&stream_id) {
                                let _ = resp
                                    .respond(
                                        &mut server,
                                        &mut s,
                                        Response {
                                            status: 200,
                                            fields: Vec::new(),
                                            body: received.len().to_be_bytes().to_vec(),
                                        },
                                    )
                                    .unwrap();
                            }
                            break;
                        }
                        if r.len == 0 {
                            break;
                        }
                    }
                }
            }
        }
        res.record_bytes("srv_received_total", &received);
        step_pair(&mut client, &mut server, &mut c, &mut s, 6);
        while let Some(ev) = client.poll_event() {
            record_client_event(&mut res, &ev);
        }
        res.record_debug("stats_stream", (server.stats(), client.stats()));
    }

    // 2. Whole-body connection buffer exhaustion triggers deterministic H3_EXCESSIVE_LOAD reset.
    for round in 0..8usize {
        let (mut c, mut s) = pair();
        let scfg = H3Config {
            max_connection_buffer: 400 + round * 10,
            ..H3Config::default()
        };
        let mut client = ClientConnection::new(H3Config::default());
        let mut server = ServerConnection::new(scfg);
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        let mut sids = Vec::new();
        for k in 0..3usize {
            let mut req = ClientRequest::get("example.test", &format!("/big/{k}"));
            req.method = "POST".into();
            req.fields.push(Field::new("content-length", "250"));
            let id = client.send_request_streaming(&mut c, &req).unwrap();
            client.send_body(&mut c, id, &vec![k as u8; 220]).unwrap();
            sids.push(id);
        }
        step_pair(&mut client, &mut server, &mut c, &mut s, 6);
        for id in sids {
            let _ = client.finish_body(&mut c, id);
        }
        step_pair(&mut client, &mut server, &mut c, &mut s, 6);
        let mut responders = Vec::new();
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
            if let Event::Request(r) = ev {
                responders.push(r.responder);
            }
        }
        for r in responders {
            let _ = r.respond(
                &mut server,
                &mut s,
                Response {
                    status: 200,
                    fields: Vec::new(),
                    body: Vec::new(),
                },
            );
        }
        step_pair(&mut client, &mut server, &mut c, &mut s, 4);
        while let Some(ev) = client.poll_event() {
            record_client_event(&mut res, &ev);
        }
        res.record_debug("budget_stats", server.stats());
    }

    res.finish();
}

#[test]
fn differential_priority_scheduler() {
    let mut res = Results::new("priority_scheduler");
    let body_bytes = vec![0xab; 10_000];

    for &work_conserving in &[false, true] {
        for &quantum in &[512usize, 2048] {
            for &budget in &[512usize, 1024, 4096] {
                let (mut c, mut s) = pair();
                let cfg = H3Config {
                    incremental_quantum: quantum,
                    work_conserving,
                    ..H3Config::default()
                };
                let mut client = ClientConnection::new(cfg);
                let mut server = ServerConnection::new(cfg);
                step_pair(&mut client, &mut server, &mut c, &mut s, 4);

                let specs = [
                    ("/u5", "u=5"),
                    ("/u1a", "u=1, i"),
                    ("/u1b", "u=1, i"),
                    ("/u0", "u=0"),
                    ("/u3", "u=3"),
                ];
                let mut ids = Vec::new();
                for (path, pri) in specs {
                    let mut req = ClientRequest::get("example.test", path);
                    req.fields.push(Field::new("priority", pri));
                    ids.push(client.send_request(&mut c, &req).unwrap());
                }
                client
                    .update_priority(&mut c, ids[4], Priority::new(1, true))
                    .unwrap();
                step_pair(&mut client, &mut server, &mut c, &mut s, 4);

                let mut requests = Vec::new();
                while let Some(ev) = server.poll_event() {
                    record_server_event(&mut res, &ev);
                    if let Event::Request(r) = ev {
                        requests.push(*r);
                    }
                }
                s.budget = Some(0);
                for r in requests {
                    let mut sr = r
                        .responder
                        .send_headers(
                            &mut server,
                            &mut s,
                            200,
                            Vec::new(),
                            Some(body_bytes.len() as u64),
                        )
                        .unwrap();
                    sr.send_data(&mut server, &mut s, &body_bytes).unwrap();
                    let _ = sr.finish(&mut server, &mut s, Vec::new()).unwrap();
                }
                s.log.clear();
                let mut req_writes: Vec<(usize, u64, usize)> = Vec::new();
                for step in 0..120usize {
                    let before = s.log.len();
                    s.budget = Some(budget);
                    server.poll(&mut s);
                    // Pool chunks vs VecDeque slices can split one stream write across
                    // two transport::write calls in the same poll; fold per-(poll, id).
                    for &(id, n) in &s.log[before..] {
                        if id % 4 != 0 {
                            continue;
                        }
                        if let Some((prev_step, prev_id, prev_n)) = req_writes.last_mut()
                            && *prev_step == step
                            && *prev_id == id
                        {
                            *prev_n += n;
                        } else {
                            req_writes.push((step, id, n));
                        }
                    }
                }
                s.budget = None;
                res.record_debug("sched_log", &req_writes);

                step_pair(&mut client, &mut server, &mut c, &mut s, 8);
                while let Some(ev) = client.poll_event() {
                    if let ClientEvent::Response(r) = ev {
                        res.record_debug("sched_resp", (r.stream_id, r.head.status, r.body.len()));
                    }
                }
                res.record_debug("sched_stats", server.stats());
            }
        }
    }

    res.finish();
}

#[test]
fn differential_goaway_and_lifecycle() {
    let mut res = Results::new("goaway_and_lifecycle");

    // 1. Concurrency cap, server shutdown GOAWAY, in-flight completion, post-GOAWAY refusal.
    for max_conc in [1usize, 2, 4] {
        let (mut c, mut s) = pair();
        let scfg = H3Config {
            max_concurrent_requests: max_conc,
            ..H3Config::default()
        };
        let mut client = ClientConnection::new(H3Config::default());
        let mut server = ServerConnection::new(scfg);
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);

        let mut sent = Vec::new();
        for k in 0..5usize {
            sent.push(
                client
                    .send_request(
                        &mut c,
                        &ClientRequest::get("example.test", &format!("/{k}")),
                    )
                    .unwrap(),
            );
        }
        step_pair(&mut client, &mut server, &mut c, &mut s, 4);
        let mut resp_obs = Vec::new();
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
            if let Event::Request(r) = ev {
                resp_obs.push(r.responder);
            }
        }
        server.shutdown(&mut s);
        for (idx, resp) in resp_obs.into_iter().enumerate() {
            if idx == 0 && max_conc >= 2 {
                // Drop first responder deliberately on max_conc >= 2 to verify leak detection.
                drop(resp);
            } else {
                let _ = resp
                    .respond(
                        &mut server,
                        &mut s,
                        Response {
                            status: 200,
                            fields: Vec::new(),
                            body: b"ok".to_vec(),
                        },
                    )
                    .unwrap();
            }
        }
        step_pair(&mut client, &mut server, &mut c, &mut s, 4);
        let err = client.send_request(&mut c, &ClientRequest::get("example.test", "/late"));
        res.record_debug("late_send_err", err);
        while let Some(ev) = client.poll_event() {
            record_client_event(&mut res, &ev);
        }
        res.record_debug("goaway_stats", (server.stats(), client.stats()));
    }

    // 2. Decreasing client GOAWAYs + peer stream reset + foreign responder check.
    {
        let (mut c1, mut s1) = pair();
        let (mut c2, mut s2) = pair();
        let mut cli1 = ClientConnection::new(H3Config::default());
        let mut srv1 = ServerConnection::new(H3Config::default());
        let mut srv2 = ServerConnection::new(H3Config::default());
        step_pair(&mut cli1, &mut srv1, &mut c1, &mut s1, 3);
        let mut cli2 = ClientConnection::new(H3Config::default());
        step_pair(&mut cli2, &mut srv2, &mut c2, &mut s2, 3);

        let a = cli1
            .send_request(&mut c1, &ClientRequest::get("example.test", "/a"))
            .unwrap();
        let b = cli1
            .send_request(&mut c1, &ClientRequest::get("example.test", "/b"))
            .unwrap();
        let c_req = cli1
            .send_request(&mut c1, &ClientRequest::get("example.test", "/c"))
            .unwrap();
        let d_req = cli1
            .send_request(&mut c1, &ClientRequest::get("example.test", "/d"))
            .unwrap();
        step_pair(&mut cli1, &mut srv1, &mut c1, &mut s1, 4);

        let mut responders = Vec::new();
        while let Some(ev) = srv1.poll_event() {
            record_server_event(&mut res, &ev);
            if let Event::Request(r) = ev {
                responders.push(r.responder);
            }
        }
        // Foreign responder check against srv2.
        let r0 = responders.remove(0);
        let r0 = match r0.respond(
            &mut srv2,
            &mut s2,
            Response {
                status: 200,
                fields: Vec::new(),
                body: Vec::new(),
            },
        ) {
            Err((back, e)) => {
                res.record_debug("foreign_err", e);
                back
            }
            Ok(_) => panic!("foreign responder succeeded"),
        };

        // Verify client.cancel on a & c_req.
        cli1.cancel(&mut c1, a);
        cli1.cancel(&mut c1, c_req);
        step_pair(&mut cli1, &mut srv1, &mut c1, &mut s1, 4);
        while let Some(ev) = srv1.poll_event() {
            record_server_event(&mut res, &ev);
        }
        let r0_res = r0.reset(&mut srv1, &mut s1, Code::REQUEST_CANCELLED);
        res.record_debug("r0_reset", r0_res.is_ok());
        for resp in responders {
            let _ = resp.respond(
                &mut srv1,
                &mut s1,
                Response {
                    status: 200,
                    fields: Vec::new(),
                    body: b"ok".to_vec(),
                },
            );
        }
        step_pair(&mut cli1, &mut srv1, &mut c1, &mut s1, 4);
        while let Some(ev) = cli1.poll_event() {
            record_client_event(&mut res, &ev);
        }
        res.record_debug(
            "lifecycle_err_checks",
            (cli1.error(), srv1.error(), b, d_req),
        );
    }

    // 2b. Raw server decreasing GOAWAY sequence (id=12 then id=4).
    {
        let (mut c, mut s) = pair();
        let mut client = ClientConnection::new(H3Config::default());
        let ctrl = raw_control(&mut s);
        let id0 = client
            .send_request(&mut c, &ClientRequest::get("example.test", "/0"))
            .unwrap();
        let id4 = client
            .send_request(&mut c, &ClientRequest::get("example.test", "/4"))
            .unwrap();
        let id8 = client
            .send_request(&mut c, &ClientRequest::get("example.test", "/8"))
            .unwrap();
        let mut gw1 = Vec::new();
        frame::write_varint_frame(&mut gw1, frame::GOAWAY, 12);
        write_all(&mut s, ctrl, &gw1);
        client.poll(&mut c);
        while let Some(ev) = client.poll_event() {
            record_client_event(&mut res, &ev);
        }
        let mut gw2 = Vec::new();
        frame::write_varint_frame(&mut gw2, frame::GOAWAY, 4);
        write_all(&mut s, ctrl, &gw2);
        client.poll(&mut c);
        while let Some(ev) = client.poll_event() {
            record_client_event(&mut res, &ev);
        }
        res.record_debug("raw_goaway_ids", (id0, id4, id8, client.in_flight()));
    }

    // 3. Client error variants (BufferFull, UnknownStream).
    {
        let (mut c, mut s) = pair();
        let cfg = H3Config {
            max_stream_buffer: 32,
            ..H3Config::default()
        };
        let mut client = ClientConnection::new(cfg);
        let mut server = ServerConnection::new(H3Config::default());
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        let mut req = ClientRequest::get("example.test", "/s");
        req.method = "POST".into();
        let id = client.send_request_streaming(&mut c, &req).unwrap();
        let e1 = client.send_body(&mut c, id, &[1u8; 64]);
        let e2 = client.send_body(&mut c, 9999, b"hi");
        let e3 = client.finish_body(&mut c, 9999);
        client.finish_body(&mut c, id).unwrap();
        let e4 = client.send_body(&mut c, id, b"after");
        res.record_debug(
            "cli_body_errs",
            (
                matches!(e1, Err(ClientError::BufferFull)),
                matches!(e2, Err(ClientError::UnknownStream)),
                matches!(e3, Err(ClientError::UnknownStream)),
                matches!(e4, Err(ClientError::UnknownStream)),
            ),
        );
    }

    res.finish();
}

#[test]
fn differential_raw_wire_errors_and_blocked() {
    let mut res = Results::new("raw_wire_errors");

    // A. Control stream errors (missing SETTINGS, duplicate control/QPACK, GOAWAY id error, HTTP/2 frame).
    let control_cases: Vec<Vec<u8>> = vec![
        // Missing SETTINGS on control stream (starts with GOAWAY).
        {
            let mut b = Vec::new();
            frame::put_varint(&mut b, frame::STREAM_CONTROL);
            frame::write_varint_frame(&mut b, frame::GOAWAY, 0);
            b
        },
        // HTTP/2 SETTINGS / WINDOW_UPDATE frame on control stream.
        {
            let mut b = Vec::new();
            frame::put_varint(&mut b, frame::STREAM_CONTROL);
            Settings::default().encode(&mut b);
            frame::write_frame(&mut b, 0x08, &[0, 0, 0, 1]);
            b
        },
        // Invalid boolean setting (enable_connect_protocol = 2).
        {
            let mut b = Vec::new();
            frame::put_varint(&mut b, frame::STREAM_CONTROL);
            let mut p = Vec::new();
            frame::put_varint(&mut p, 0x08);
            frame::put_varint(&mut p, 2);
            frame::write_frame(&mut b, frame::SETTINGS, &p);
            b
        },
        // Invalid GOAWAY stream ID (not client-bidi multiple of 4).
        {
            let mut b = Vec::new();
            frame::put_varint(&mut b, frame::STREAM_CONTROL);
            Settings::default().encode(&mut b);
            frame::write_varint_frame(&mut b, frame::GOAWAY, 3);
            b
        },
        // Increasing GOAWAY stream ID.
        {
            let mut b = Vec::new();
            frame::put_varint(&mut b, frame::STREAM_CONTROL);
            Settings::default().encode(&mut b);
            frame::write_varint_frame(&mut b, frame::GOAWAY, 4);
            frame::write_varint_frame(&mut b, frame::GOAWAY, 8);
            b
        },
    ];
    for (idx, wire) in control_cases.into_iter().enumerate() {
        let (mut c, mut s) = pair();
        let mut server = ServerConnection::new(H3Config::default());
        let id = c.raw_open(true);
        write_all(&mut c, id, &wire);
        server.poll(&mut s);
        res.record_debug(
            "ctrl_err",
            (idx, server.error(), s.closed(), server.stats()),
        );
    }

    // Duplicate control / QPACK streams + closed critical stream.
    for stype in [
        frame::STREAM_CONTROL,
        frame::STREAM_QPACK_ENCODER,
        frame::STREAM_QPACK_DECODER,
        frame::STREAM_PUSH,
    ] {
        let (mut c, mut s) = pair();
        let mut server = ServerConnection::new(H3Config::default());
        let s1 = c.raw_open(true);
        let mut v = Vec::new();
        frame::put_varint(&mut v, stype);
        if stype == frame::STREAM_CONTROL {
            Settings::default().encode(&mut v);
        }
        write_all(&mut c, s1, &v);
        server.poll(&mut s);
        if s.closed().is_none() {
            let s2 = c.raw_open(true);
            write_all(&mut c, s2, &v);
            server.poll(&mut s);
        }
        res.record_debug("uni_dup", (stype, server.error(), s.closed()));

        if stype != frame::STREAM_PUSH {
            let (mut c2, mut s2) = pair();
            let mut srv2 = ServerConnection::new(H3Config::default());
            let cs = c2.raw_open(true);
            write_all(&mut c2, cs, &v);
            srv2.poll(&mut s2);
            c2.finish(cs).unwrap();
            srv2.poll(&mut s2);
            res.record_debug("uni_closed", (stype, srv2.error(), s2.closed()));
        }
    }

    // B. Request stream framing & message errors + blocked QPACK section resolution.
    let mut enc = Encoder::new(EncoderConfig {
        max_table_capacity: 4096,
        ..EncoderConfig::default()
    });
    enc.apply_peer_settings(4096, 16);
    let req_cases: Vec<(Vec<u8>, bool)> = vec![
        // DATA before HEADERS
        (
            {
                let mut w = Vec::new();
                frame::write_frame(&mut w, frame::DATA, b"hi");
                w
            },
            true,
        ),
        // Duplicate HEADERS (not trailers)
        (
            {
                let mut w = Vec::new();
                let sec = enc.encode(0, &ClientRequest::get("example.test", "/").field_list());
                frame::write_frame(&mut w, frame::HEADERS, &sec);
                frame::write_frame(&mut w, frame::HEADERS, &sec);
                w
            },
            true,
        ),
        // Truncated frame at FIN
        (vec![0x01, 0x08, 0x00], true),
        // Missing :method pseudo-header
        (
            {
                let mut w = Vec::new();
                let sec = enc.encode(
                    4,
                    &[
                        Field::new(":scheme", "https"),
                        Field::new(":path", "/"),
                        Field::new(":authority", "example.test"),
                    ],
                );
                frame::write_frame(&mut w, frame::HEADERS, &sec);
                w
            },
            true,
        ),
        // Unknown frame tolerated before valid HEADERS + DATA + trailers
        (
            {
                let mut w = Vec::new();
                frame::write_frame(&mut w, 0x21, b"grease");
                let sec = enc.encode(8, &ClientRequest::get("example.test", "/ok").field_list());
                frame::write_frame(&mut w, frame::HEADERS, &sec);
                frame::write_frame(&mut w, frame::DATA, b"body");
                let trl = enc.encode(8, &[Field::new("x-end", "1")]);
                frame::write_frame(&mut w, frame::HEADERS, &trl);
                w
            },
            true,
        ),
    ];
    for (idx, (wire, fin)) in req_cases.into_iter().enumerate() {
        let (mut c, mut s) = pair();
        let mut server = ServerConnection::new(H3Config::default());
        let enc_id = c.raw_open(true);
        let mut ewire = Vec::new();
        frame::put_varint(&mut ewire, frame::STREAM_QPACK_ENCODER);
        ewire.extend_from_slice(&enc.take_encoder_stream());
        write_all(&mut c, enc_id, &ewire);
        let rid = c.raw_open(false);
        write_all(&mut c, rid, &wire);
        if fin {
            c.finish(rid).unwrap();
        }
        server.poll(&mut s);
        let mut responders = Vec::new();
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
            if let Event::Request(r) = ev {
                responders.push(r.responder);
            }
        }
        for resp in responders {
            let _ = resp.respond(
                &mut server,
                &mut s,
                Response {
                    status: 200,
                    fields: Vec::new(),
                    body: Vec::new(),
                },
            );
        }
        res.record_debug(
            "req_raw",
            (idx, server.error(), c.peer_reset(rid), server.stats()),
        );
    }

    // Blocked QPACK section waits until encoder stream arrives.
    {
        let (mut c, mut s) = pair();
        let mut server = ServerConnection::new(H3Config::default());
        let mut enc2 = Encoder::new(EncoderConfig {
            max_table_capacity: 4096,
            ..EncoderConfig::default()
        });
        enc2.apply_peer_settings(4096, 16);
        let rid = c.raw_open(false);
        let sec = enc2.encode(
            rid,
            &[
                Field::new(":method", "GET"),
                Field::new(":scheme", "https"),
                Field::new(":authority", "example.test"),
                Field::new(":path", "/blocked-custom"),
                Field::new("x-blocked", "dynamic-value"),
            ],
        );
        let mut rwire = Vec::new();
        frame::write_frame(&mut rwire, frame::HEADERS, &sec);
        write_all(&mut c, rid, &rwire);
        c.finish(rid).unwrap();
        server.poll(&mut s);
        res.record_debug("blocked_before", server.poll_event().is_none());

        let enc_id = c.raw_open(true);
        let mut ewire = Vec::new();
        frame::put_varint(&mut ewire, frame::STREAM_QPACK_ENCODER);
        ewire.extend_from_slice(&enc2.take_encoder_stream());
        write_all(&mut c, enc_id, &ewire);
        server.poll(&mut s);
        let mut responders = Vec::new();
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
            if let Event::Request(r) = ev {
                responders.push(r.responder);
            }
        }
        for resp in responders {
            let _ = resp.respond(
                &mut server,
                &mut s,
                Response {
                    status: 200,
                    fields: Vec::new(),
                    body: Vec::new(),
                },
            );
        }
        res.record_debug("blocked_stats", server.stats());
    }

    // Unknown uni streams up to limit + deterministic random garbage fuzz on request streams.
    let mut rng = Rng::new(0x9999_aaaa_bbbb_cccc);
    for round in 0..32usize {
        let (mut c, mut s) = pair();
        let mut server = ServerConnection::new(H3Config::default());
        let rid = c.raw_open(false);
        let len = 1 + rng.below(24) as usize;
        let garbage = rng.bytes(len);
        write_all(&mut c, rid, &garbage);
        if round % 2 == 0 {
            c.finish(rid).unwrap();
        }
        server.poll(&mut s);
        let mut responders = Vec::new();
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
            if let Event::Request(r) = ev {
                responders.push(r.responder);
            }
        }
        for resp in responders {
            let _ = resp.respond(
                &mut server,
                &mut s,
                Response {
                    status: 200,
                    fields: Vec::new(),
                    body: Vec::new(),
                },
            );
        }
        res.record_debug("fuzz_req", (round, c.peer_reset(rid), server.error()));
    }

    res.finish();
}

#[test]
fn differential_webtransport_sessions_and_streams() {
    let mut res = Results::new("wt_sessions_and_streams");

    for &max_dgram in &[Some(1200usize), Some(16), None] {
        for &wlim in &[0usize, 5] {
            let (mut c, mut s) = pair();
            c.max_datagram = max_dgram;
            s.max_datagram = max_dgram;
            c.write_limit = wlim;
            s.write_limit = wlim;

            let cfg = H3Config::default().with_webtransport(4);
            let mut client = ClientConnection::new(cfg);
            let mut server = ServerConnection::new(cfg);
            let rounds = if wlim == 5 { 14 } else { 5 };
            step_pair(&mut client, &mut server, &mut c, &mut s, rounds);

            // Extended CONNECT for WebTransport and for a custom tunnel.
            let wt_id = client
                .open_session(
                    &mut c,
                    &ClientRequest::connect("example.test", "/wt", "webtransport"),
                )
                .unwrap();
            let tun_id = client
                .open_session(
                    &mut c,
                    &ClientRequest::connect("example.test", "/tun", "connect-ip"),
                )
                .unwrap();
            let refused_id = client
                .open_session(
                    &mut c,
                    &ClientRequest::connect("example.test", "/deny", "webtransport"),
                )
                .unwrap();

            step_pair(&mut client, &mut server, &mut c, &mut s, rounds);
            let mut wt_sess: Option<Session> = None;
            let mut tun_sess: Option<Session> = None;
            while let Some(ev) = server.poll_event() {
                record_server_event(&mut res, &ev);
                if let Event::Connect(cr) = ev {
                    if cr.stream_id == wt_id {
                        wt_sess = Some(
                            cr.responder
                                .accept_session(&mut server, &mut s, vec![Field::new("x-wt", "1")])
                                .unwrap(),
                        );
                    } else if cr.stream_id == tun_id {
                        tun_sess = Some(
                            cr.responder
                                .accept_session(&mut server, &mut s, Vec::new())
                                .unwrap(),
                        );
                    } else {
                        let _ = cr
                            .responder
                            .respond(
                                &mut server,
                                &mut s,
                                Response {
                                    status: 403,
                                    fields: Vec::new(),
                                    body: b"nope".to_vec(),
                                },
                            )
                            .unwrap();
                    }
                }
            }
            let wt_sess = wt_sess.unwrap();
            let tun_sess = tun_sess.unwrap();
            step_pair(&mut client, &mut server, &mut c, &mut s, rounds);
            while let Some(ev) = client.poll_event() {
                record_client_event(&mut res, &ev);
            }

            // Tunnel cannot open WT streams.
            let not_wt_srv = server.open_stream(&mut s, &tun_sess, true);
            let not_wt_cli = client.open_stream(&mut c, tun_id, false);
            res.record_debug("not_wt", (not_wt_srv, not_wt_cli));

            // Bidirectional & unidirectional WT streams both directions + datagrams.
            let c_bidi = client.open_stream(&mut c, wt_id, true).unwrap();
            let c_uni = client.open_stream(&mut c, wt_id, false).unwrap();
            let _ = client.write_stream(&mut c, c_bidi, b"hello-bidi").unwrap();
            client.finish_stream(&mut c, c_bidi).unwrap();
            let _ = client.write_stream(&mut c, c_uni, b"hello-uni").unwrap();
            client.finish_stream(&mut c, c_uni).unwrap();

            let s_bidi = server.open_stream(&mut s, &wt_sess, true).unwrap();
            let s_uni = server.open_stream(&mut s, &wt_sess, false).unwrap();
            let _ = server.write_stream(&mut s, s_bidi, b"srv-bidi").unwrap();
            server.finish_stream(&mut s, s_bidi).unwrap();
            let _ = server.write_stream(&mut s, s_uni, b"srv-uni").unwrap();
            server.finish_stream(&mut s, s_uni).unwrap();

            let dg_small = client.send_datagram(&mut c, wt_id, b"dg1");
            let dg_big = client.send_datagram(&mut c, wt_id, &[7u8; 40]);
            let dg_tun = server.send_datagram(&mut s, &tun_sess, b"tun-dg");
            res.record_debug(
                "dg_sends",
                (
                    dg_small,
                    matches!(
                        dg_big,
                        Ok(DatagramPath::Quic | DatagramPath::Capsule)
                            | Err(SessionError::TooLarge)
                    ),
                    dg_tun,
                ),
            );

            step_pair(&mut client, &mut server, &mut c, &mut s, rounds);
            let mut sbuf = [0u8; 64];
            while let Some(ev) = server.poll_event() {
                record_server_event(&mut res, &ev);
                if let Event::Session(SessionEvent::Stream { stream_id, .. }) = ev {
                    let mut acc = Vec::new();
                    loop {
                        let r = server.read_stream(&mut s, stream_id, &mut sbuf).unwrap();
                        acc.extend_from_slice(&sbuf[..r.len]);
                        if r.fin || r.len == 0 {
                            res.record_debug("srv_wt_read", (stream_id, r.fin, acc.as_slice()));
                            break;
                        }
                    }
                }
            }
            while let Some(ev) = client.poll_event() {
                record_client_event(&mut res, &ev);
                if let ClientEvent::Session(SessionEvent::Stream { stream_id, .. }) = ev {
                    let mut acc = Vec::new();
                    loop {
                        let r = client.read_stream(&mut c, stream_id, &mut sbuf).unwrap();
                        acc.extend_from_slice(&sbuf[..r.len]);
                        if r.fin || r.len == 0 {
                            res.record_debug("cli_wt_read", (stream_id, r.fin, acc.as_slice()));
                            break;
                        }
                    }
                }
            }

            let _ = wt_sess.close(&mut server, &mut s, 0x2a, b"clean").unwrap();
            let _ = tun_sess.close(&mut server, &mut s, 0, b"").unwrap();
            let _ = client.close_session(&mut c, wt_id, 0, b"");
            let _ = client.close_session(&mut c, tun_id, 0, b"");
            step_pair(&mut client, &mut server, &mut c, &mut s, rounds);
            while let Some(ev) = server.poll_event() {
                record_server_event(&mut res, &ev);
            }
            while let Some(ev) = client.poll_event() {
                record_client_event(&mut res, &ev);
            }
            let _ = refused_id;
            res.record_debug("wt_stats", (server.session_stats(), client.session_stats()));
        }
    }

    // Early stream arrival racing acceptance & exceeding held limit (16).
    {
        let (mut c, mut s) = pair();
        let cfg = H3Config::default().with_webtransport(4);
        let mut client = ClientConnection::new(cfg);
        let mut server = ServerConnection::new(cfg);
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        let sid = client
            .open_session(
                &mut c,
                &ClientRequest::connect("example.test", "/held", "webtransport"),
            )
            .unwrap();
        // Raw early uni streams before server accepts CONNECT.
        let mut raw_ids = Vec::new();
        for i in 0..18u64 {
            let st = c.raw_open(true);
            let mut hdr = Vec::new();
            frame::put_varint(&mut hdr, frame::STREAM_WEBTRANSPORT);
            frame::put_varint(&mut hdr, sid);
            hdr.extend_from_slice(&[i as u8; 4]);
            write_all(&mut c, st, &hdr);
            c.finish(st).unwrap();
            raw_ids.push(st);
        }
        client.poll(&mut c);
        server.poll(&mut s);
        let mut sess = None;
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
            if let Event::Connect(cr) = ev {
                sess = Some(
                    cr.responder
                        .accept_session(&mut server, &mut s, Vec::new())
                        .unwrap(),
                );
            }
        }
        server.poll(&mut s);
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
        }
        for id in &raw_ids {
            res.record_debug("held_reset", c.peer_reset(*id));
        }
        let _ = sess.unwrap().close(&mut server, &mut s, 0, b"").unwrap();
    }

    res.finish();
}

#[test]
fn differential_capsule_and_datagram_fuzz() {
    let mut res = Results::new("capsule_and_datagram_fuzz");
    let mut rng = Rng::new(0xdead_beef_cafe_babe);

    // 1. Bad close reasons leave session open, DRAIN capsule, truncated capsule at FIN, unknown capsules.
    {
        let (mut c, mut s) = pair();
        let cfg = H3Config::default().with_webtransport(2);
        let mut client = ClientConnection::new(cfg);
        let mut server = ServerConnection::new(cfg);
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        let sid = client
            .open_session(
                &mut c,
                &ClientRequest::connect("example.test", "/cap", "webtransport"),
            )
            .unwrap();
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        let sess = match server.poll_event().unwrap() {
            Event::Connect(cr) => cr
                .responder
                .accept_session(&mut server, &mut s, Vec::new())
                .unwrap(),
            other => panic!("{other:?}"),
        };
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        while let Some(ev) = client.poll_event() {
            record_client_event(&mut res, &ev);
        }

        let bad_utf8 = client.close_session(&mut c, sid, 1, &[0xff, 0xfe]);
        let too_long = client.close_session(&mut c, sid, 1, &vec![b'a'; 1025]);
        res.record_debug("bad_close", (bad_utf8, too_long));

        // Unknown + DRAIN_WEBTRANSPORT_SESSION + CLOSE_WEBTRANSPORT_SESSION capsules split awkwardly.
        let mut caps = Vec::new();
        capsule::write_capsule(&mut caps, 0x4242, b"unknown-cap");
        capsule::write_capsule(&mut caps, capsule::DRAIN_WEBTRANSPORT_SESSION, b"");
        capsule::write_close_session(&mut caps, 0x77, b"closing");
        for chunk in caps.chunks(3) {
            let mut df = Vec::new();
            frame::write_frame(&mut df, frame::DATA, chunk);
            write_all(&mut c, sid, &df);
        }
        server.poll(&mut s);
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
        }
        let _ = sess.close(&mut server, &mut s, 0, b"").unwrap();
    }

    // 2. Malformed QUIC datagrams and fuzzed capsule payloads.
    for round in 0..32usize {
        let (mut c, mut s) = pair();
        c.max_datagram = Some(1200);
        s.max_datagram = Some(1200);
        let cfg = H3Config::default().with_webtransport(2);
        let mut client = ClientConnection::new(cfg);
        let mut server = ServerConnection::new(cfg);
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        let sid = client
            .open_session(
                &mut c,
                &ClientRequest::connect("example.test", "/fuzz", "webtransport"),
            )
            .unwrap();
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        let sess = match server.poll_event().unwrap() {
            Event::Connect(cr) => cr
                .responder
                .accept_session(&mut server, &mut s, Vec::new())
                .unwrap(),
            other => panic!("{other:?}"),
        };
        step_pair(&mut client, &mut server, &mut c, &mut s, 3);
        while let Some(ev) = client.poll_event() {
            record_client_event(&mut res, &ev);
        }

        if round % 2 == 0 {
            let n = 1 + rng.below(12) as usize;
            let dg = rng.bytes(n);
            c.raw_datagram(&dg);
        } else {
            let mut cap_bytes = Vec::new();
            let ctype = [capsule::DATAGRAM, capsule::CLOSE_WEBTRANSPORT_SESSION, 0x99][round % 3];
            let plen = rng.below(20) as usize;
            let payload = rng.bytes(plen);
            capsule::write_capsule(&mut cap_bytes, ctype, &payload);
            if round % 4 == 3 {
                // Truncate at half.
                cap_bytes.truncate(cap_bytes.len() / 2);
                let mut df = Vec::new();
                frame::write_frame(&mut df, frame::DATA, &cap_bytes);
                write_all(&mut c, sid, &df);
                c.finish(sid).unwrap();
            } else {
                let mut df = Vec::new();
                frame::write_frame(&mut df, frame::DATA, &cap_bytes);
                write_all(&mut c, sid, &df);
            }
        }
        server.poll(&mut s);
        while let Some(ev) = server.poll_event() {
            record_server_event(&mut res, &ev);
        }
        res.record_debug(
            "cap_fuzz",
            (round, server.error(), s.closed(), server.session_stats()),
        );
        let _ = sess.close(&mut server, &mut s, 0, b"");
    }

    res.finish();
}
