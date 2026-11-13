//! QPACK offline interop (the QIFs corpus, github.com/qpackers/qifs):
//! encoder outputs from other QPACK implementations, decoded by ours and
//! compared with the original QIF field lists.
//!
//! The committed subset (`tests/fixtures/qifs/`) is the `netbsd` QIF with
//! every encoder and table configuration published in the corpus. The
//! whole corpus (also `fb-req` and `fb-resp`) is checked when
//! `VESPER_QIFS_DIR` points at a checkout with `qifs/` and `encoded/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vesper_prism_h3::qpack::{Decoded, Decoder, DecoderConfig, FieldSection};

/// Field lists of a QIF file, one per header block.
fn parse_qif(text: &str) -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
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
        cur.push((n.as_bytes().to_vec(), v.as_bytes().to_vec()));
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    blocks
}

/// Decodes one offline-interop file: (stream ID u64, length u32, bytes)*,
/// stream 0 being the encoder stream.
fn decode_file(data: &[u8], capacity: u64, blocked: u64) -> BTreeMap<u64, FieldSection> {
    let mut d = Decoder::new(DecoderConfig {
        max_table_capacity: capacity,
        max_blocked_streams: blocked,
        max_field_section_size: 1 << 20,
        max_blocked_bytes: 1 << 20,
    });
    // Draft 05 (the corpus's wire version) started the table at the
    // decoder's maximum; RFC 9204 starts at zero and needs an explicit Set
    // Dynamic Table Capacity, so the harness supplies that instruction.
    if capacity > 0 {
        let mut set = Vec::new();
        vesper_prism_h3::qpack::prefix::encode_int(&mut set, 0x20, 5, capacity);
        d.on_encoder_stream(&set).unwrap();
    }
    let mut out = BTreeMap::new();
    let mut at = 0;
    while at < data.len() {
        let stream = u64::from_be_bytes(data[at..at + 8].try_into().unwrap());
        let len = u32::from_be_bytes(data[at + 8..at + 12].try_into().unwrap()) as usize;
        let chunk = &data[at + 12..at + 12 + len];
        at += 12 + len;
        if stream == 0 {
            d.on_encoder_stream(chunk).unwrap();
            while let Some(s) = d.poll_unblocked() {
                let s = s.unwrap();
                out.insert(s.stream_id, s);
            }
        } else {
            match d.decode_section(stream, chunk).unwrap() {
                Decoded::Fields(s) => {
                    out.insert(stream, s);
                }
                Decoded::Blocked => assert!(blocked > 0, "blocked with no budget"),
            }
        }
        // Acknowledgments are produced but there is no encoder to read them.
        let _ = d.take_decoder_stream();
    }
    assert_eq!(d.blocked_streams(), 0, "sections left blocked");
    out
}

/// Checks every `<qif>.out.<capacity>.<blocked>.<ack>` under `encoded`.
fn check_corpus(qifs: &Path, encoded: &Path) -> usize {
    let mut checked = 0;
    let mut impls: Vec<PathBuf> = std::fs::read_dir(encoded)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    impls.sort();
    for dir in impls {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        for file in files {
            let name = file.file_name().unwrap().to_str().unwrap().to_owned();
            let Some((qif, params)) = name.split_once(".out.") else {
                continue;
            };
            let p: Vec<u64> = params.split('.').map(|x| x.parse().unwrap()).collect();
            let text = std::fs::read_to_string(qifs.join(format!("{qif}.qif"))).unwrap();
            let expected = parse_qif(&text);
            let got = decode_file(&std::fs::read(&file).unwrap(), p[0], p[1]);
            assert_eq!(got.len(), expected.len(), "{}", file.display());
            for (i, want) in expected.iter().enumerate() {
                let section = &got[&(i as u64 + 1)];
                let have: Vec<_> = section
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.value.clone()))
                    .collect();
                assert_eq!(&have, want, "{} block {}", file.display(), i + 1);
            }
            checked += 1;
        }
    }
    checked
}

#[test]
fn committed_qifs_subset_decodes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qifs");
    let n = check_corpus(&root.join("qifs"), &root.join("encoded/qpack-05"));
    // Six encoders (f5, ls-qpack, nghttp3, proxygen, qthingey, quinn)
    // across their published table/blocking/ack configurations.
    assert_eq!(n, 70, "files checked");
}

#[test]
fn full_qifs_corpus_when_available() {
    let Ok(dir) = std::env::var("VESPER_QIFS_DIR") else {
        eprintln!("VESPER_QIFS_DIR unset: skipping the full QIFs corpus");
        return;
    };
    let root = Path::new(&dir);
    let n = check_corpus(&root.join("qifs"), &root.join("encoded/qpack-05"));
    eprintln!("decoded {n} QIFs corpus files");
    assert!(n > 0);
}
