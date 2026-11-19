//! Our QPACK encoder's output on the committed QIFs field lists, byte for
//! byte. `tests/fixtures/qpack-golden/` holds what the Rust build wrote
//! (`VESPER_H3_BLESS=1 cargo test -p vesper-prism-h3 --test
//! qpack_encode_golden` rewrites it), in the QIFs offline-interop layout
//! (stream ID u64, length u32, bytes; stream 0 is the encoder stream). Both
//! builds must reproduce it exactly, and our own decoder must read it back.

use std::path::{Path, PathBuf};

use vesper_prism_h3::qpack::{Decoded, Decoder, DecoderConfig, Encoder, EncoderConfig, Field};

fn qif_blocks() -> Vec<Vec<Field>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qifs/qifs/netbsd.qif");
    let text = std::fs::read_to_string(path).unwrap();
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

fn record(out: &mut Vec<u8>, stream: u64, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Encodes every block, acknowledging after every `ack_every` sections
/// (0: never), and checks that a decoder with the same settings reads the
/// blocks back.
fn encode_all(blocks: &[Vec<Field>], capacity: u64, blocked: u64, ack_every: usize) -> Vec<u8> {
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
    let mut out = Vec::new();
    let mut decoded = vec![None; blocks.len()];
    for (i, block) in blocks.iter().enumerate() {
        let stream = i as u64 + 1;
        let section = enc.encode(stream, block);
        let instructions = enc.take_encoder_stream();
        record(&mut out, 0, &instructions);
        record(&mut out, stream, &section);
        dec.on_encoder_stream(&instructions).unwrap();
        while let Some(s) = dec.poll_unblocked() {
            let s = s.unwrap();
            decoded[s.stream_id as usize - 1] = Some(s.fields);
        }
        if let Decoded::Fields(s) = dec.decode_section(stream, &section).unwrap() {
            decoded[i] = Some(s.fields);
        }
        if ack_every > 0 && (i + 1) % ack_every == 0 {
            enc.on_decoder_stream(&dec.take_decoder_stream()).unwrap();
        }
    }
    for (i, block) in blocks.iter().enumerate() {
        let got = decoded[i].as_ref().expect("every section decoded");
        let pairs = |f: &[Field]| -> Vec<(Vec<u8>, Vec<u8>)> {
            f.iter()
                .map(|f| (f.name.clone(), f.value.clone()))
                .collect()
        };
        assert_eq!(pairs(got), pairs(block), "block {}", i + 1);
    }
    out
}

#[test]
fn encoder_output_matches_the_rust_build_byte_for_byte() {
    let blocks = qif_blocks();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qpack-golden");
    let bless = std::env::var_os("VESPER_H3_BLESS").is_some();
    let mut checked = 0;
    for (capacity, blocked, ack_every) in [
        (0, 0, 1),
        (256, 0, 1),
        (256, 100, 0),
        (4096, 0, 1),
        (4096, 0, 3),
        (4096, 100, 0),
    ] {
        let bytes = encode_all(&blocks, capacity, blocked, ack_every);
        let path = dir.join(format!("netbsd.out.{capacity}.{blocked}.{ack_every}"));
        if bless {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
        let want = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(bytes == want, "{} differs", path.display());
        checked += 1;
    }
    assert_eq!(checked, 6);
}
