//! Fuzz-style robustness: random and mutated input to every decoder never
//! panics and never succeeds silently with corrupted state; our encoder
//! round-trips the QIF corpus through our decoder under delayed and
//! reordered acknowledgments without breaking the eviction rules.

mod support;

use std::collections::VecDeque;
use std::path::Path;

use vesper_prism_h3::connection::{H3Config, ServerConnection};
use vesper_prism_h3::frame::{self, FrameReader};
use vesper_prism_h3::qpack::{Decoded, Decoder, DecoderConfig, Encoder, EncoderConfig, Field};
use vesper_prism_h3::transport::Transport;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
    /// Up to `max - 1` random bytes.
    fn some(&mut self, max: u64) -> Vec<u8> {
        let n = self.below(max) as usize;
        self.bytes(n)
    }
    fn mutate(&mut self, mut v: Vec<u8>) -> Vec<u8> {
        match self.below(4) {
            0 if !v.is_empty() => {
                let i = self.below(v.len() as u64) as usize;
                v[i] ^= 1 << self.below(8);
            }
            1 if !v.is_empty() => {
                let n = self.below(v.len() as u64) as usize;
                v.truncate(n);
            }
            2 => {
                let i = self.below(v.len() as u64 + 1) as usize;
                let mut extra = self.some(8);
                extra.push(0xff);
                v.splice(i..i, extra);
            }
            _ if !v.is_empty() => {
                let i = self.below(v.len() as u64) as usize;
                v[i] = self.next() as u8;
            }
            _ => {}
        }
        v
    }
}

fn sample_fields(rng: &mut Rng) -> Vec<Field> {
    let names = [
        "x-a",
        "x-b",
        "cookie",
        ":path",
        "user-agent",
        "accept",
        "x-long",
    ];
    (0..1 + rng.below(6))
        .map(|_| {
            let n = names[rng.below(names.len() as u64) as usize];
            let v = if n == "x-long" {
                "v".repeat(rng.below(300) as usize)
            } else {
                format!("value-{}", rng.below(20))
            };
            Field::new(n, v)
        })
        .collect()
}

#[test]
fn decoder_survives_random_and_mutated_input() {
    let mut rng = Rng(1);
    for round in 0..3000 {
        let mut d = Decoder::new(DecoderConfig {
            max_table_capacity: 256,
            max_blocked_streams: 2,
            max_field_section_size: 4096,
            max_blocked_bytes: 2048,
        });
        // A valid encoder stream and sections, then mutate them.
        let mut e = Encoder::new(EncoderConfig {
            max_table_capacity: 256,
            ..EncoderConfig::default()
        });
        e.apply_peer_settings(256, 2);
        let mut sections = Vec::new();
        for s in 0..3 {
            sections.push(e.encode(s * 4, &sample_fields(&mut rng)));
        }
        let enc = e.take_encoder_stream();
        let enc = if round % 3 == 0 {
            rng.some(64)
        } else {
            rng.mutate(enc)
        };
        let r = d.on_encoder_stream(&enc);
        for (i, s) in sections.into_iter().enumerate() {
            let s = if round % 2 == 0 { rng.mutate(s) } else { s };
            let _ = d.decode_section(i as u64 * 4, &s);
            while d.poll_unblocked().is_some() {}
        }
        let _ = d.take_decoder_stream();
        // A failed decoder stays failed.
        if r.is_err() {
            assert!(d.on_encoder_stream(&[0x20]).is_err());
        }
    }
}

#[test]
fn encoder_survives_random_decoder_instructions() {
    let mut rng = Rng(2);
    for _ in 0..2000 {
        let mut e = Encoder::new(EncoderConfig::default());
        e.apply_peer_settings(1024, 4);
        for s in 0..4 {
            let _ = e.encode(s * 4, &sample_fields(&mut rng));
        }
        let junk = rng.some(24);
        let _ = e.on_decoder_stream(&junk);
        // Whatever happened, encoding still works.
        let _ = e.encode(100, &sample_fields(&mut rng));
    }
}

#[test]
fn frame_reader_survives_random_input() {
    let mut rng = Rng(3);
    for _ in 0..5000 {
        let mut r = FrameReader::new(256);
        let data = rng.some(200);
        let mut at = 0;
        while at < data.len() {
            let n = (1 + rng.below(16) as usize).min(data.len() - at);
            r.push(&data[at..at + n]);
            at += n;
            while let Ok(Some(_)) = r.next_frame() {}
        }
        let _ = r.finish();
        assert!(r.buffered() <= 256 + 16, "bounded buffering");
    }
}

#[test]
fn server_survives_random_streams() {
    let mut rng = Rng(4);
    for round in 0..800 {
        let (mut c, mut s) = support::pair();
        let mut srv = ServerConnection::new(H3Config::default());
        // Sometimes a valid control stream first.
        if round % 2 == 0 {
            let id = c.raw_open(true);
            let mut w = Vec::new();
            frame::put_varint(&mut w, frame::STREAM_CONTROL);
            frame::Settings::default().encode(&mut w);
            c.write(id, &w).unwrap();
        }
        for _ in 0..1 + rng.below(4) {
            let uni = rng.below(2) == 0;
            let id = c.raw_open(uni);
            let mut payload = rng.some(80);
            if uni && !payload.is_empty() {
                payload[0] = (rng.below(5)) as u8;
            }
            let _ = c.write(id, &payload);
            if rng.below(2) == 0 {
                let _ = c.finish(id);
            }
        }
        for _ in 0..3 {
            srv.poll(&mut s);
        }
        // Either it failed with a typed, sent code, or it is still usable.
        if let Some(e) = srv.error() {
            assert_eq!(s.closed(), Some(e.code()));
        }
        while srv.poll_event().is_some() {}
    }
}

fn qif_blocks() -> Vec<Vec<Field>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qifs/qifs/netbsd.qif");
    let text = std::fs::read_to_string(path).unwrap();
    let mut blocks = Vec::new();
    let mut cur = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
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

#[test]
fn encoder_round_trips_through_decoder_with_delayed_acks() {
    let blocks = qif_blocks();
    for (capacity, blocked, seed) in [
        (0, 0, 1),
        (64, 0, 2),
        (256, 0, 3),
        (256, 4, 4),
        (1024, 100, 5),
        (4096, 2, 6),
        (4096, 100, 7),
    ] {
        let mut rng = Rng(seed);
        let mut e = Encoder::new(EncoderConfig {
            max_table_capacity: capacity,
            ..EncoderConfig::default()
        });
        e.apply_peer_settings(capacity, blocked);
        let mut d = Decoder::new(DecoderConfig {
            max_table_capacity: capacity,
            max_blocked_streams: blocked,
            max_field_section_size: 1 << 20,
            max_blocked_bytes: 1 << 20,
        });
        // In-flight encoder-stream bytes and sections, delivered late.
        let mut enc_wire: VecDeque<Vec<u8>> = VecDeque::new();
        let mut dec_wire: VecDeque<Vec<u8>> = VecDeque::new();
        let mut decoded = 0;
        for (i, block) in blocks.iter().enumerate() {
            let id = i as u64 * 4;
            let section = e.encode(id, block);
            enc_wire.push_back(e.take_encoder_stream());
            // Sections may overtake encoder-stream bytes (that is what
            // blocking is for) only within the blocked budget.
            let late = blocked > 0 && rng.below(3) == 0;
            if !late {
                while let Some(b) = enc_wire.pop_front() {
                    d.on_encoder_stream(&b).unwrap();
                }
            }
            match d.decode_section(id, &section).unwrap() {
                Decoded::Fields(s) => {
                    assert_eq!(&s.fields, block, "block {i} cap {capacity}");
                    decoded += 1;
                }
                Decoded::Blocked => assert!(late),
            }
            while let Some(b) = enc_wire.pop_front() {
                d.on_encoder_stream(&b).unwrap();
            }
            while let Some(s) = d.poll_unblocked() {
                let s = s.unwrap();
                assert_eq!(&s.fields, &blocks[s.stream_id as usize / 4]);
                decoded += 1;
            }
            // Decoder feedback reaches the encoder with a random delay.
            dec_wire.push_back(d.take_decoder_stream());
            while !dec_wire.is_empty() && rng.below(2) == 0 {
                e.on_decoder_stream(&dec_wire.pop_front().unwrap()).unwrap();
            }
        }
        while let Some(b) = dec_wire.pop_front() {
            e.on_decoder_stream(&b).unwrap();
        }
        assert_eq!(decoded, blocks.len());
        assert_eq!(e.outstanding_sections(), 0, "all acknowledged");
        if capacity >= 256 {
            assert!(
                e.stats().dynamic_refs > 0,
                "cap {capacity}: {:?}",
                e.stats()
            );
        }
    }
}
