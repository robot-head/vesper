//! ABBA benchmark of the last Rust build of vesper-prism-h3 against the
//! Vesper port (port wave P3b lane 5, #1067; SH6 lane 5, #1400): QPACK decoding of the committed QIFs corpus,
//! QPACK encoding of its field lists, frame parsing (`frames`),
//! message validation (`messages`), end-to-end HTTP/3 connections (`connection`),
//! and WebTransport / HTTP Datagram sessions (`session`).
//!
//! The two builds are this crate and its Rust build from an older commit
//! (`scripts/bench.sh` builds it in a git worktree), so they cannot share a
//! process. Each arm is a worker process (this example
//! built one way or the other) that runs one batch of the workload per
//! request on stdin; the driver times each request with `vesper-bench`'s
//! `run_abba`, so both arms carry the same pipe round trip. A batch is long
//! (milliseconds) next to that round trip. Every batch returns a digest of
//! its results, and the driver checks that both builds agree. `once` runs
//! batches without the driver, for callgrind instruction counts.
//!
//! ```text
//! crates/vesper-prism-h3/scripts/bench.sh       # builds both, runs every workload
//! abba worker <decode|encode|frames|messages|connection|session>                # one arm
//! abba drive <workload> <rust-worker> <vesper-worker>
//! abba flip-gate <workload> <rust-worker> <vesper-worker>
//! abba once <workload> <batches>
//! ```
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::rc::Rc;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_prism_h3::capsule::{self, Capsule, CapsuleReader};
use vesper_prism_h3::client::{ClientConnection, ClientEvent, ClientRequest};
use vesper_prism_h3::connection::{Event, H3Config, Response, ServerConnection, Session};
use vesper_prism_h3::error::Code;
use vesper_prism_h3::frame::{self, Frame, FrameReader, Settings};
use vesper_prism_h3::message;
use vesper_prism_h3::priority::{Priority, PriorityUpdate};
use vesper_prism_h3::qpack::{Decoded, Decoder, DecoderConfig, Encoder, EncoderConfig, Field};
use vesper_prism_h3::session::{DatagramPath, SessionEvent};
use vesper_prism_h3::transport::{ReadOutcome, Transport, TransportError};

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qifs")
}

/// One QIFs offline-interop file: its table capacity, blocked-stream limit
/// and (stream, bytes) records.
struct Encoded {
    capacity: u64,
    blocked: u64,
    records: Vec<(u64, Vec<u8>)>,
}

fn corpus() -> Vec<Encoded> {
    let mut out = Vec::new();
    let root = fixtures().join("encoded/qpack-05");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dirs.sort();
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        for file in files {
            let name = file.file_name().unwrap().to_str().unwrap().to_owned();
            let Some((_, params)) = name.split_once(".out.") else {
                continue;
            };
            let p: Vec<u64> = params.split('.').map(|x| x.parse().unwrap()).collect();
            let data = std::fs::read(&file).unwrap();
            let mut records = Vec::new();
            let mut at = 0;
            while at < data.len() {
                let stream = u64::from_be_bytes(data[at..at + 8].try_into().unwrap());
                let len = u32::from_be_bytes(data[at + 8..at + 12].try_into().unwrap()) as usize;
                records.push((stream, data[at + 12..at + 12 + len].to_vec()));
                at += 12 + len;
            }
            out.push(Encoded {
                capacity: p[0],
                blocked: p[1],
                records,
            });
        }
    }
    out
}

fn qif_blocks() -> Vec<Vec<Field>> {
    let text = std::fs::read_to_string(fixtures().join("qifs/netbsd.qif")).unwrap();
    let mut blocks = Vec::new();
    let mut cur = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.is_empty() {
            if !cur.is_empty() {
                blocks.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let (n, v) = line.split_once('\t').unwrap_or((line, ""));
        cur.push(Field::new(n, v));
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    blocks
}

fn digest_fields(mut h: u64, fields: &[Field]) -> u64 {
    for f in fields {
        h = fnv(h, &f.name);
        h = fnv(h, &f.value);
    }
    h
}

/// Decodes all 70 corpus files (1,330 field sections).
fn decode_batch(corpus: &[Encoded]) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for file in corpus {
        let mut d = Decoder::new(DecoderConfig {
            max_table_capacity: file.capacity,
            max_blocked_streams: file.blocked,
            max_field_section_size: 1 << 20,
            max_blocked_bytes: 1 << 20,
        });
        if file.capacity > 0 {
            let mut set = Vec::new();
            vesper_prism_h3::qpack::prefix::encode_int(&mut set, 0x20, 5, file.capacity);
            d.on_encoder_stream(&set).unwrap();
        }
        for (stream, bytes) in &file.records {
            if *stream == 0 {
                d.on_encoder_stream(bytes).unwrap();
                while let Some(s) = d.poll_unblocked() {
                    digest = digest_fields(digest, &s.unwrap().fields);
                }
            } else if let Decoded::Fields(s) = d.decode_section(*stream, bytes).unwrap() {
                digest = digest_fields(digest, &s.fields);
            }
            digest = fnv(digest, &d.take_decoder_stream());
        }
    }
    digest
}

/// Encodes the corpus's field lists under four table settings, feeding
/// each section's acknowledgment back, as a server's encoder sees it.
fn encode_batch(blocks: &[Vec<Field>]) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for (capacity, blocked) in [(0, 0), (256, 0), (4096, 0), (4096, 100)] {
        let mut enc = Encoder::new(EncoderConfig {
            max_table_capacity: capacity,
            ..EncoderConfig::default()
        });
        let mut dec = Decoder::new(DecoderConfig {
            max_table_capacity: capacity,
            max_blocked_streams: blocked,
            max_field_section_size: 1 << 20,
            max_blocked_bytes: 1 << 20,
        });
        enc.apply_peer_settings(capacity, blocked);
        for round in 0..4u64 {
            for (i, block) in blocks.iter().enumerate() {
                let stream = 4 * (round * blocks.len() as u64 + i as u64);
                let section = enc.encode(stream, block);
                let instructions = enc.take_encoder_stream();
                digest = fnv(digest, &section);
                digest = fnv(digest, &instructions);
                dec.on_encoder_stream(&instructions).unwrap();
                while dec.poll_unblocked().is_some() {}
                let _ = dec.decode_section(stream, &section).unwrap();
                enc.on_decoder_stream(&dec.take_decoder_stream()).unwrap();
            }
        }
    }
    digest
}

/// The frame workload's streams (built once): 64 request streams, each
/// HEADERS (a section of the corpus) then DATA frames of 1 to 16 KiB; a
/// control stream; and a capsule stream.
struct Streams {
    requests: Vec<Vec<u8>>,
    control: Vec<u8>,
    capsules: Vec<u8>,
}

fn streams(corpus: &[Encoded]) -> Streams {
    let sections: Vec<&[u8]> = corpus
        .iter()
        .flat_map(|f| f.records.iter().filter(|(s, _)| *s != 0))
        .map(|(_, b)| b.as_slice())
        .collect();
    let mut requests = Vec::new();
    for i in 0..64usize {
        let mut w = Vec::new();
        frame::write_frame(&mut w, frame::HEADERS, sections[i * 7 % sections.len()]);
        for k in 0..4 {
            let len = 1024 * (1 + (i * 5 + k * 3) % 16);
            frame::write_frame(&mut w, frame::DATA, &vec![(i + k) as u8; len]);
        }
        frame::write_frame(&mut w, 0x21 + 0x1f * (i as u64 % 5), b"grease");
        requests.push(w);
    }
    let mut control = Vec::new();
    Settings {
        qpack_max_table_capacity: 4096,
        qpack_blocked_streams: 100,
        max_field_section_size: Some(65536),
        enable_connect_protocol: true,
        h3_datagram: true,
        webtransport_max_sessions: Some(8),
        no_rfc7540_priorities: true,
    }
    .encode(&mut control);
    for i in 0..256u64 {
        PriorityUpdate {
            push: false,
            element_id: 4 * i,
            priority: Priority::new((i % 8) as u8, i % 3 == 0),
        }
        .encode(&mut control);
    }
    frame::write_varint_frame(&mut control, frame::GOAWAY, 1 << 20);
    let mut capsules = Vec::new();
    for i in 0..512usize {
        capsule::write_capsule(
            &mut capsules,
            capsule::DATAGRAM,
            &vec![i as u8; 32 + i % 1000],
        );
        if i % 16 == 0 {
            capsule::write_capsule(&mut capsules, 0x17, &[0; 100]);
        }
    }
    capsule::write_close_session(&mut capsules, 0x1234, b"done");
    Streams {
        requests,
        control,
        capsules,
    }
}

/// Bytes arrive in packet-sized pieces.
const PACKET: usize = 1200;

fn frames_batch(s: &Streams) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    let frame_digest = |digest: &mut u64, f: Frame| {
        *digest = fnv(*digest, &f.frame_type().to_be_bytes());
        match f {
            Frame::Data(d) | Frame::Headers(d) => *digest = fnv(*digest, &d),
            Frame::PriorityUpdate(u) => {
                let p = u.priority;
                *digest = fnv(*digest, &[p.urgency, u8::from(p.incremental)]);
                // The scheduler's view of the update.
                *digest = fnv(
                    *digest,
                    Priority::parse(p.serialize().as_bytes())
                        .serialize()
                        .as_bytes(),
                );
            }
            Frame::Settings(st) => {
                *digest = fnv(*digest, &st.qpack_max_table_capacity.to_be_bytes())
            }
            _ => {}
        }
    };
    for w in s.requests.iter().chain([&s.control]) {
        let mut r = FrameReader::new(1 << 16);
        for chunk in w.chunks(PACKET) {
            r.push(chunk);
            while let Some(f) = r.next_frame().unwrap() {
                frame_digest(&mut digest, f);
            }
        }
        r.finish().unwrap();
    }
    let mut r = CapsuleReader::new(1 << 16);
    for chunk in s.capsules.chunks(PACKET) {
        r.push(chunk);
        while let Some(c) = r.next_capsule().unwrap() {
            match c {
                Capsule::Datagram(d) => digest = fnv(digest, &d),
                Capsule::CloseSession { code, reason } => {
                    digest = fnv(digest, &code.to_be_bytes());
                    digest = fnv(digest, &reason);
                }
                Capsule::DrainSession => digest = fnv(digest, b"drain"),
                Capsule::Unknown(t) => digest = fnv(digest, &t.to_be_bytes()),
            }
        }
    }
    r.finish().unwrap();
    digest
}

/// Validates every field list of the QIFs corpus (requests, and responses
/// when it starts with `:status`), 64 rounds.
fn messages_batch(lists: &[Vec<Field>]) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for _ in 0..64 {
        for list in lists {
            if list.first().is_some_and(|f| f.name == b":status") {
                match message::parse_response(list.clone()) {
                    Ok(r) => digest = fnv(digest, &r.status.to_be_bytes()),
                    Err(e) => digest = fnv(digest, format!("{e:?}").as_bytes()),
                }
            } else {
                match message::parse_request(list.clone()) {
                    Ok(r) => {
                        digest = fnv(digest, r.path.as_bytes());
                        digest = fnv(digest, &r.fields.len().to_be_bytes());
                    }
                    Err(e) => digest = fnv(digest, format!("{e:?}").as_bytes()),
                }
            }
        }
    }
    digest
}

fn frame_count(_s: &Streams) -> usize {
    // 64 requests x 6 frames, the control stream's 258, and the capsules.
    64 * 6 + 258 + 512 + 512 / 16 + 1
}

#[derive(Default)]
struct Half {
    data: VecDeque<u8>,
    fin: bool,
    reset: Option<u64>,
    stopped: Option<u64>,
}

#[derive(Default)]
struct Shared {
    halves: BTreeMap<(u64, usize), Half>,
    accept: [VecDeque<u64>; 2],
    next: [[u64; 2]; 2],
    closed: [Option<Code>; 2],
    datagrams: [VecDeque<Vec<u8>>; 2],
}

struct Mem {
    shared: Rc<RefCell<Shared>>,
    side: usize,
    max_datagram: Option<usize>,
}

fn mem_pair(max_datagram: Option<usize>) -> (Mem, Mem) {
    let shared = Rc::new(RefCell::new(Shared::default()));
    (
        Mem {
            shared: shared.clone(),
            side: 0,
            max_datagram,
        },
        Mem {
            shared,
            side: 1,
            max_datagram,
        },
    )
}

impl Mem {
    fn open(&mut self, uni: bool) -> u64 {
        let mut s = self.shared.borrow_mut();
        let k = usize::from(uni);
        let id = (s.next[self.side][k] << 2) | (u64::from(uni) << 1) | (self.side as u64);
        s.next[self.side][k] += 1;
        s.halves.entry((id, self.side)).or_default();
        if !uni {
            s.halves.entry((id, 1 - self.side)).or_default();
        }
        s.accept[1 - self.side].push_back(id);
        id
    }
}

impl Transport for Mem {
    fn open_uni(&mut self) -> Result<u64, TransportError> {
        Ok(self.open(true))
    }
    fn open_bidi(&mut self) -> Result<u64, TransportError> {
        Ok(self.open(false))
    }
    fn write(&mut self, id: u64, data: &[u8]) -> Result<usize, TransportError> {
        let mut s = self.shared.borrow_mut();
        if s.closed.iter().any(Option::is_some) {
            return Err(TransportError::Closed);
        }
        let h = s
            .halves
            .get_mut(&(id, self.side))
            .ok_or(TransportError::UnknownStream)?;
        if let Some(code) = h.reset {
            return Err(TransportError::Reset(code));
        }
        if h.fin {
            return Err(TransportError::UnknownStream);
        }
        h.data.extend(data);
        Ok(data.len())
    }
    fn finish(&mut self, id: u64) -> Result<(), TransportError> {
        let mut s = self.shared.borrow_mut();
        let h = s
            .halves
            .get_mut(&(id, self.side))
            .ok_or(TransportError::UnknownStream)?;
        h.fin = true;
        Ok(())
    }
    fn read(&mut self, id: u64, out: &mut [u8]) -> Result<ReadOutcome, TransportError> {
        let mut s = self.shared.borrow_mut();
        let h = s
            .halves
            .get_mut(&(id, 1 - self.side))
            .ok_or(TransportError::UnknownStream)?;
        if let Some(code) = h.reset {
            return Err(TransportError::Reset(code));
        }
        let n = out.len().min(h.data.len());
        for (o, b) in out.iter_mut().zip(h.data.drain(..n)) {
            *o = b;
        }
        Ok(ReadOutcome {
            len: n,
            fin: h.fin && h.data.is_empty(),
        })
    }
    fn reset(&mut self, id: u64, code: Code) {
        let mut s = self.shared.borrow_mut();
        if let Some(h) = s.halves.get_mut(&(id, self.side))
            && !h.fin
        {
            h.reset = Some(code.0);
            h.data.clear();
        }
    }
    fn stop_sending(&mut self, id: u64, code: Code) {
        let mut s = self.shared.borrow_mut();
        if let Some(h) = s.halves.get_mut(&(id, 1 - self.side)) {
            h.stopped = Some(code.0);
            if !(h.fin && h.data.is_empty()) {
                h.reset = Some(code.0);
                h.data.clear();
            }
        }
    }
    fn accept(&mut self) -> Option<u64> {
        self.shared.borrow_mut().accept[self.side].pop_front()
    }
    fn close(&mut self, code: Code) {
        self.shared.borrow_mut().closed[self.side].get_or_insert(code);
    }
    fn max_datagram_size(&mut self) -> Option<usize> {
        self.max_datagram
    }
    fn send_datagram(&mut self, data: &[u8]) -> Result<(), TransportError> {
        match self.max_datagram {
            None => Err(TransportError::Unsupported),
            Some(m) if data.len() > m => Err(TransportError::TooLarge),
            Some(_) => {
                self.shared.borrow_mut().datagrams[1 - self.side].push_back(data.to_vec());
                Ok(())
            }
        }
    }
    fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        self.shared.borrow_mut().datagrams[self.side].pop_front()
    }
}

fn digest_u64(h: u64, v: u64) -> u64 {
    fnv(h, &v.to_be_bytes())
}

/// Runs HTTP/3 client/server connections over an in-memory QUIC transport:
/// whole requests, interims, chunked responses with trailers, priority updates,
/// streaming request uploads, and GOAWAY.
fn connection_batch() -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    let payload: Vec<u8> = (0..512usize).map(|i| (i.wrapping_mul(37)) as u8).collect();
    for round in 0..8usize {
        let (mut c, mut s) = mem_pair(None);
        let cfg = H3Config {
            incremental_quantum: 4096,
            ..H3Config::default()
        };
        let mut client = ClientConnection::new(cfg);
        let mut server = ServerConnection::new(cfg);
        // Handshake settings exchange.
        for _ in 0..2 {
            client.poll(&mut c);
            server.poll(&mut s);
        }
        for wave in 0..4usize {
            let mut sent_ids = Vec::with_capacity(4);
            for k in 0..4usize {
                let mut req =
                    ClientRequest::get("example.com", &format!("/item/{round}/{wave}/{k}"));
                req.fields.push(Field::new(
                    "priority",
                    if k % 2 == 0 { "u=1, i" } else { "u=3" },
                ));
                req.fields
                    .push(Field::new("x-tag", format!("w{wave}-k{k}")));
                if k % 2 == 1 {
                    req.method = "POST".into();
                    req.body = payload[..64 + (k * 97 + wave * 31) % 384].to_vec();
                }
                let sid = client.send_request(&mut c, &req).unwrap();
                if k == 1 {
                    client
                        .update_priority(&mut c, sid, Priority::new(0, true))
                        .unwrap();
                }
                sent_ids.push(sid);
            }
            for _ in 0..3 {
                client.poll(&mut c);
                server.poll(&mut s);
            }
            while let Some(ev) = server.poll_event() {
                match ev {
                    Event::Request(r) => {
                        digest = digest_u64(digest, r.stream_id);
                        digest = fnv(digest, r.head.method.as_bytes());
                        digest = fnv(digest, r.head.path.as_bytes());
                        digest = digest_fields(digest, &r.head.fields);
                        digest = fnv(digest, &r.body);
                        let resp_len = 96 + (r.stream_id as usize % 5) * 64;
                        if (r.stream_id / 4) % 2 == 0 {
                            let mut resp = r.responder;
                            resp.send_interim(
                                &mut server,
                                &mut s,
                                103,
                                vec![Field::new("link", "</style.css>; rel=preload")],
                            )
                            .unwrap();
                            let _ = resp
                                .respond(
                                    &mut server,
                                    &mut s,
                                    Response {
                                        status: 200,
                                        fields: vec![Field::new("content-type", "text/plain")],
                                        body: payload[..resp_len].to_vec(),
                                    },
                                )
                                .unwrap();
                        } else {
                            let mut resp = r
                                .responder
                                .send_headers(
                                    &mut server,
                                    &mut s,
                                    200,
                                    vec![Field::new("x-streamed", "yes")],
                                    Some(resp_len as u64),
                                )
                                .unwrap();
                            resp.send_data(&mut server, &mut s, &payload[..resp_len / 2])
                                .unwrap();
                            resp.send_data(&mut server, &mut s, &payload[resp_len / 2..resp_len])
                                .unwrap();
                            let _ = resp
                                .finish(&mut server, &mut s, vec![Field::new("x-end", "ok")])
                                .unwrap();
                        }
                    }
                    other => panic!("unexpected server event {other:?}"),
                }
            }
            for _ in 0..3 {
                server.poll(&mut s);
                client.poll(&mut c);
            }
            while let Some(ev) = client.poll_event() {
                match ev {
                    ClientEvent::Interim { stream_id, head } => {
                        digest = digest_u64(digest, stream_id);
                        digest = digest_u64(digest, u64::from(head.status));
                        digest = digest_fields(digest, &head.fields);
                    }
                    ClientEvent::Response(r) => {
                        digest = digest_u64(digest, r.stream_id);
                        digest = digest_u64(digest, u64::from(r.head.status));
                        digest = digest_fields(digest, &r.head.fields);
                        digest = fnv(digest, &r.body);
                        digest = digest_fields(digest, &r.trailers);
                    }
                    other => panic!("unexpected client event {other:?}"),
                }
            }
        }
        server.shutdown(&mut s);
        for _ in 0..2 {
            server.poll(&mut s);
            client.poll(&mut c);
        }
        while let Some(ev) = client.poll_event() {
            match ev {
                ClientEvent::GoAway(id) => digest = digest_u64(digest, id),
                other => panic!("unexpected post-goaway client event {other:?}"),
            }
        }
        let ss = server.stats();
        let cs = client.stats();
        digest = digest_u64(digest, ss.requests);
        digest = digest_u64(digest, ss.responses);
        digest = digest_u64(digest, ss.interim_responses);
        digest = digest_u64(digest, ss.priority_updates);
        digest = digest_u64(digest, cs.responses);
        digest = digest_u64(digest, cs.interim_responses);

        // Streaming request upload pair.
        let (mut c, mut s) = mem_pair(None);
        let scfg = H3Config {
            streaming_request_bodies: true,
            stream_body_window: 1024,
            ..H3Config::default()
        };
        let mut client = ClientConnection::new(H3Config::default());
        let mut server = ServerConnection::new(scfg);
        for _ in 0..2 {
            client.poll(&mut c);
            server.poll(&mut s);
        }
        let mut responders = BTreeMap::new();
        for u in 0..4usize {
            let mut req = ClientRequest::get("example.com", &format!("/upload/{round}/{u}"));
            req.method = "POST".into();
            req.fields
                .push(Field::new("content-length", (256 * 3).to_string()));
            let sid = client.send_request_streaming(&mut c, &req).unwrap();
            for chunk_idx in 0..3usize {
                client
                    .send_body(&mut c, sid, &payload[chunk_idx * 16..chunk_idx * 16 + 256])
                    .unwrap();
                client.poll(&mut c);
                server.poll(&mut s);
            }
            client.finish_body(&mut c, sid).unwrap();
            client.poll(&mut c);
            server.poll(&mut s);
            let mut body_buf = Vec::new();
            let mut tmp = [0u8; 256];
            loop {
                server.poll(&mut s);
                let mut drained = false;
                while let Some(ev) = server.poll_event() {
                    drained = true;
                    match ev {
                        Event::StreamingRequest(sr) => {
                            digest = digest_u64(digest, sr.stream_id);
                            digest = fnv(digest, sr.head.path.as_bytes());
                            responders.insert(sr.stream_id, sr.responder);
                        }
                        Event::BodyReadable { stream_id } => loop {
                            let r = server.read_body(&mut s, stream_id, &mut tmp).unwrap();
                            body_buf.extend_from_slice(&tmp[..r.len]);
                            if r.end {
                                let trailers = server.take_trailers(stream_id);
                                digest = digest_fields(digest, &trailers);
                                digest = fnv(digest, &body_buf);
                                let resp = responders.remove(&stream_id).unwrap();
                                let _ = resp
                                    .respond(
                                        &mut server,
                                        &mut s,
                                        Response {
                                            status: 204,
                                            fields: Vec::new(),
                                            body: Vec::new(),
                                        },
                                    )
                                    .unwrap();
                                break;
                            }
                            if r.len == 0 {
                                break;
                            }
                        },
                        other => panic!("unexpected streaming server event {other:?}"),
                    }
                }
                if !drained {
                    break;
                }
            }
            client.poll(&mut c);
            while let Some(ev) = client.poll_event() {
                if let ClientEvent::Response(r) = ev {
                    digest = digest_u64(digest, u64::from(r.head.status));
                }
            }
        }
    }
    digest
}

/// Runs WebTransport and capsule HTTP Datagram sessions over an in-memory
/// transport: extended CONNECT, uni/bidi WT streams, QUIC + capsule
/// datagrams, and graceful CLOSE_WEBTRANSPORT_SESSION closure.
fn session_batch() -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    let mut stream_buf = [0u8; 512];
    for round in 0..16usize {
        for max_dgram in [Some(1200usize), None] {
            let (mut c, mut s) = mem_pair(max_dgram);
            let cfg = H3Config::default().with_webtransport(4);
            let mut client = ClientConnection::new(cfg);
            let mut server = ServerConnection::new(cfg);
            for _ in 0..2 {
                client.poll(&mut c);
                server.poll(&mut s);
            }
            let req =
                ClientRequest::connect("example.com", &format!("/wt/{round}"), "webtransport");
            let sid = client.open_session(&mut c, &req).unwrap();
            client.poll(&mut c);
            server.poll(&mut s);
            let mut sess: Option<Session> = None;
            while let Some(ev) = server.poll_event() {
                match ev {
                    Event::Connect(cr) => {
                        digest = digest_u64(digest, cr.stream_id);
                        digest = fnv(digest, cr.head.path.as_bytes());
                        sess = Some(
                            cr.responder
                                .accept_session(&mut server, &mut s, Vec::new())
                                .unwrap(),
                        );
                    }
                    other => panic!("unexpected connect event {other:?}"),
                }
            }
            let sess = sess.expect("accepted session");
            client.poll(&mut c);
            while let Some(ev) = client.poll_event() {
                match ev {
                    ClientEvent::SessionEstablished { session_id, head } => {
                        digest = digest_u64(digest, session_id);
                        digest = digest_u64(digest, u64::from(head.status));
                    }
                    other => panic!("unexpected session established event {other:?}"),
                }
            }
            // Client opens bidi + uni WT streams and sends datagrams.
            for k in 0..2usize {
                let bidi = client.open_stream(&mut c, sid, true).unwrap();
                let msg = format!("c-bidi-{round}-{k}");
                let _ = client.write_stream(&mut c, bidi, msg.as_bytes()).unwrap();
                client.finish_stream(&mut c, bidi).unwrap();

                let uni = client.open_stream(&mut c, sid, false).unwrap();
                let umsg = format!("c-uni-{round}-{k}");
                let _ = client.write_stream(&mut c, uni, umsg.as_bytes()).unwrap();
                client.finish_stream(&mut c, uni).unwrap();

                let path = client
                    .send_datagram(&mut c, sid, format!("c-dgram-{round}-{k}").as_bytes())
                    .unwrap();
                digest = digest_u64(digest, matches!(path, DatagramPath::Quic) as u64);
            }
            // Server opens bidi + uni WT streams and sends datagrams.
            let sbidi = server.open_stream(&mut s, &sess, true).unwrap();
            let _ = server.write_stream(&mut s, sbidi, b"s-bidi-hello").unwrap();
            server.finish_stream(&mut s, sbidi).unwrap();
            let suni = server.open_stream(&mut s, &sess, false).unwrap();
            let _ = server.write_stream(&mut s, suni, b"s-uni-hello").unwrap();
            server.finish_stream(&mut s, suni).unwrap();
            let spath = server.send_datagram(&mut s, &sess, b"s-dgram").unwrap();
            digest = digest_u64(digest, matches!(spath, DatagramPath::Quic) as u64);

            for _ in 0..3 {
                client.poll(&mut c);
                server.poll(&mut s);
            }
            while let Some(ev) = server.poll_event() {
                if let Event::Session(sev) = ev {
                    match sev {
                        SessionEvent::Stream {
                            session,
                            stream_id,
                            bidi,
                        } => {
                            digest = digest_u64(digest, session);
                            digest = digest_u64(digest, stream_id);
                            digest = digest_u64(digest, u64::from(bidi));
                            let r = server
                                .read_stream(&mut s, stream_id, &mut stream_buf)
                                .unwrap();
                            digest = fnv(digest, &stream_buf[..r.len]);
                            digest = digest_u64(digest, u64::from(r.fin));
                            if bidi {
                                let _ = server.write_stream(&mut s, stream_id, b"echo").unwrap();
                                server.finish_stream(&mut s, stream_id).unwrap();
                            }
                        }
                        SessionEvent::Datagram { session, payload } => {
                            digest = digest_u64(digest, session);
                            digest = fnv(digest, &payload);
                        }
                        other => panic!("unexpected server session event {other:?}"),
                    }
                }
            }
            for _ in 0..2 {
                server.poll(&mut s);
                client.poll(&mut c);
            }
            while let Some(ev) = client.poll_event() {
                if let ClientEvent::Session(sev) = ev {
                    match sev {
                        SessionEvent::Stream {
                            session,
                            stream_id,
                            bidi,
                        } => {
                            digest = digest_u64(digest, session);
                            digest = digest_u64(digest, stream_id);
                            digest = digest_u64(digest, u64::from(bidi));
                            let r = client
                                .read_stream(&mut c, stream_id, &mut stream_buf)
                                .unwrap();
                            digest = fnv(digest, &stream_buf[..r.len]);
                            digest = digest_u64(digest, u64::from(r.fin));
                        }
                        SessionEvent::Datagram { session, payload } => {
                            digest = digest_u64(digest, session);
                            digest = fnv(digest, &payload);
                        }
                        other => panic!("unexpected client session event {other:?}"),
                    }
                }
            }
            let _ = sess.close(&mut server, &mut s, 0x1234, b"done").unwrap();
            for _ in 0..2 {
                server.poll(&mut s);
                client.poll(&mut c);
            }
            while let Some(ev) = client.poll_event() {
                if let ClientEvent::Session(SessionEvent::Closed {
                    session,
                    code,
                    reason,
                    reset,
                }) = ev
                {
                    digest = digest_u64(digest, session);
                    digest = digest_u64(digest, u64::from(code));
                    digest = fnv(digest, &reason);
                    digest = digest_u64(digest, reset.unwrap_or(u64::MAX));
                }
            }
            let _ = client.close_session(&mut c, sid, 0, b"");
            for _ in 0..2 {
                client.poll(&mut c);
                server.poll(&mut s);
            }
            let sstat = server.session_stats();
            let cstat = client.session_stats();
            digest = digest_u64(digest, sstat.sessions);
            digest = digest_u64(digest, sstat.datagrams_received);
            digest = digest_u64(digest, sstat.streams_accepted);
            digest = digest_u64(digest, cstat.datagrams_received);
            digest = digest_u64(digest, cstat.streams_accepted);
        }
    }
    digest
}

struct Workload {
    corpus: Vec<Encoded>,
    blocks: Vec<Vec<Field>>,
    streams: Streams,
    lists: Vec<Vec<Field>>,
}

impl Workload {
    fn load(workload: &str) -> Self {
        if matches!(workload, "connection" | "session") {
            return Self {
                corpus: Vec::new(),
                blocks: Vec::new(),
                streams: Streams {
                    requests: Vec::new(),
                    control: Vec::new(),
                    capsules: Vec::new(),
                },
                lists: Vec::new(),
            };
        }
        let corpus = corpus();
        let streams = streams(&corpus);
        Self {
            corpus,
            blocks: qif_blocks(),
            streams,
            lists: qif_blocks(),
        }
    }

    fn run(&self, workload: &str) -> u64 {
        match workload {
            "decode" => decode_batch(&self.corpus),
            "encode" => encode_batch(&self.blocks),
            "frames" => frames_batch(&self.streams),
            "messages" => messages_batch(&self.lists),
            "connection" => connection_batch(),
            "session" => session_batch(),
            other => panic!("unknown workload {other}"),
        }
    }
}

/// Serve batches: each stdin line `go <n>` runs one batch and answers with
/// its digest.
fn worker(workload: &str) {
    let w = Workload::load(workload);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read request");
        assert!(line.starts_with("go "), "request `go <n>`");
        writeln!(out, "{}", w.run(workload)).expect("answer");
        out.flush().expect("flush");
    }
}

/// One arm: a worker process timed per request.
struct Arm {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    digests: Vec<u64>,
}

impl Arm {
    fn spawn(binary: &str, workload: &str) -> Arm {
        let mut child = Command::new(binary)
            .args(["worker", workload])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {binary}: {e}"));
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Arm {
            child,
            stdin,
            stdout,
            digests: Vec::new(),
        }
    }
}

impl BenchCase for Arm {
    type Input = usize;
    type Output = u64;
    type Error = std::io::Error;
    fn setup(&mut self, i: Invocation) -> Result<usize, std::io::Error> {
        Ok(i.operation)
    }
    fn run(&mut self, n: usize) -> Result<u64, std::io::Error> {
        writeln!(self.stdin, "go {n}")?;
        self.stdin.flush()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        line.trim()
            .parse()
            .map_err(|e| std::io::Error::other(format!("worker answered {line:?}: {e}")))
    }
    fn teardown(&mut self, output: u64) -> Result<(), std::io::Error> {
        self.digests.push(output);
        Ok(())
    }
}

fn drive(workload: &str, rust: &str, vesper: &str, epochs: usize, emit_pairs: bool) {
    let w = Workload::load(workload);
    let (per_batch, unit) = match workload {
        "decode" => (
            w.corpus
                .iter()
                .map(|f| f.records.iter().filter(|(s, _)| *s != 0).count())
                .sum::<usize>(),
            "section",
        ),
        "encode" => (16 * w.blocks.len(), "section"),
        "frames" => (frame_count(&w.streams), "frame"),
        "messages" => (64 * w.lists.len(), "message"),
        "connection" => (8 * (16 + 4), "exchange"),
        "session" => (16 * 2, "session"),
        other => panic!("unknown workload {other}"),
    };
    let operations = 16;
    let config = RunConfig {
        epochs,
        warmup_epochs: 2,
        operations_per_window: operations,
        max_elapsed_nanos: 600_000_000_000,
        ..RunConfig::default()
    };
    let mut baseline = Arm::spawn(rust, workload);
    let mut candidate = Arm::spawn(vesper, workload);
    let report = run_abba(config, &mut baseline, &mut candidate).expect("ABBA run");
    let want = w.run(workload);
    assert!(
        baseline
            .digests
            .iter()
            .chain(&candidate.digests)
            .all(|d| *d == want),
        "the Rust and Vesper builds computed different results"
    );
    let mut ratios: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let per_unit = |total: u64| total as f64 / (2 * operations) as f64 / per_batch as f64;
    let mut rust_ns: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| per_unit(p.baseline_total()))
        .collect();
    let mut vesper_ns: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| per_unit(p.candidate_total()))
        .collect();
    rust_ns.sort_by(f64::total_cmp);
    vesper_ns.sort_by(f64::total_cmp);
    let median = |v: &[f64]| (v[(v.len() - 1) / 2] + v[v.len() / 2]) / 2.0;
    println!(
        "{workload}: {} measured epochs x A/B/B/A x {operations} batches of {per_batch} {unit}s; \
         Rust median {:.0} ns/{unit}, Vesper median {:.0} ns/{unit}, \
         Vesper/Rust median {:.2}x (min {:.2}x, max {:.2}x)",
        report.pairs().len(),
        median(&rust_ns),
        median(&vesper_ns),
        median(&ratios),
        ratios[0],
        ratios[ratios.len() - 1],
    );
    let analysis = analyze_pairs(report.pairs(), 5_000).expect("analysis");
    println!(
        "{workload}: 1.5x gate (candidate at most 5000 bp slower): {:?}; digests agree over {} batches",
        analysis.decision,
        baseline.digests.len() + candidate.digests.len()
    );
    if let Some(interval) = analysis.interval {
        println!(
            "{workload}: 95% median interval of Vesper/Rust: {:.3}x..{:.3}x",
            ratios[interval.lower_rank - 1],
            ratios[interval.upper_rank - 1]
        );
    }
    if emit_pairs {
        let pairs_str: Vec<String> = report
            .pairs()
            .iter()
            .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
            .collect();
        println!("flip-gate pairs {workload} {}", pairs_str.join(" "));
    }
    for arm in [&mut baseline, &mut candidate] {
        let _ = arm.child.kill();
        let _ = arm.child.wait();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[1..] {
        ["worker", workload] => worker(workload),
        ["drive", workload, rust, vesper] => drive(workload, rust, vesper, 30, false),
        ["flip-gate", workload, rust, vesper] => drive(workload, rust, vesper, 30, true),
        ["once", workload, batches] => {
            let w = Workload::load(workload);
            let mut digest = 0;
            for _ in 0..batches.parse::<u32>().expect("batches") {
                digest ^= w.run(workload);
            }
            println!("{digest}");
        }
        _ => {
            eprintln!(
                "usage: abba worker <workload> | abba drive <workload> <rust-worker> <vesper-worker> | abba flip-gate <workload> <rust-worker> <vesper-worker> | abba once <workload> <batches>"
            );
            std::process::exit(2);
        }
    }
}
