//! The QPACK data tables and the Required Insert Count encoding, through the
//! public API (SH6 lane 5, #1400). These were unit tests of the Rust
//! implementation's private items (`CODES`, `STATIC_TABLE`, `encode_ric` and
//! `decode_ric`) before the Vesper port became the only build.
use vesper_prism_h3::qpack::{
    Decoded, Decoder, DecoderConfig, Encoder, EncoderConfig, Field, huffman,
};

/// The bit length of each symbol's code: eight copies of a byte encode to
/// exactly that many bytes.
fn code_len(b: u8) -> usize {
    huffman::encoded_len(&[b; 8])
}

#[test]
fn huffman_code_is_complete_and_round_trips() {
    // Kraft equality over the 256 byte codes and EOS (30 bits): the code is
    // complete, so every bit string decodes.
    const MAX: usize = 30;
    let kraft: u64 = (0..=255u8)
        .map(|b| 1u64 << (MAX - code_len(b)))
        .sum::<u64>()
        + 1;
    assert_eq!(kraft, 1 << MAX);
    // RFC 7541 Appendix B spot checks: `0` is 5 bits, `a` 5, `A` 6, NUL 13.
    assert_eq!(code_len(b'0'), 5);
    assert_eq!(code_len(b'a'), 5);
    assert_eq!(code_len(b'A'), 6);
    assert_eq!(code_len(0), 13);
    // Every symbol, alone and after each other symbol, decodes back.
    for b in 0..=255u8 {
        for data in [vec![b], vec![b'x', b], vec![b, 0xff, b]] {
            let mut out = Vec::new();
            huffman::encode(&data, &mut out);
            assert_eq!(out.len(), huffman::encoded_len(&data));
            assert_eq!(huffman::decode(&out, 64).unwrap(), data, "{data:?}");
        }
    }
}

/// The one-field section `[0, 0, byte]`: an indexed static line, with no
/// dynamic table (Required Insert Count and Base both 0).
fn static_index(name: &str, value: &str) -> Option<u8> {
    let mut e = Encoder::new(EncoderConfig::default());
    match e.encode(0, &[Field::new(name, value)])[..] {
        [0, 0, b] if b & 0xc0 == 0xc0 => Some(b & 0x3f),
        _ => None,
    }
}

#[test]
fn static_table_matches_rfc_spot_checks() {
    // RFC 9204 Appendix A.
    assert_eq!(static_index(":authority", ""), Some(0));
    assert_eq!(static_index(":method", "GET"), Some(17));
    assert_eq!(static_index(":status", "200"), Some(25));
    assert_eq!(
        static_index("accept-encoding", "gzip, deflate, br"),
        Some(31)
    );
    assert_eq!(static_index(":path", "/index.html"), None);
}

#[test]
fn required_insert_count_wraps_with_the_table() {
    // RFC 9204 §4.5.1.1: a 100-byte table holds MaxEntries = 3 entries, so
    // the Required Insert Count is sent modulo 2 * 3. Each section inserts
    // a new entry and references it; 40 of them wrap the encoding many
    // times, and each must decode to its full count.
    let mut e = Encoder::new(EncoderConfig {
        max_table_capacity: 100,
        max_entry_share: 256,
        ..EncoderConfig::default()
    });
    // Blocked streams allowed, so a section may reference the entry its
    // own encoder-stream bytes insert (they reach the decoder first here).
    e.apply_peer_settings(100, 10);
    let mut d = Decoder::new(DecoderConfig {
        max_table_capacity: 100,
        max_blocked_streams: 10,
        ..DecoderConfig::default()
    });
    for i in 0..40u64 {
        let field = Field::new("x", format!("{i}"));
        let section = e.encode(i * 4, std::slice::from_ref(&field));
        d.on_encoder_stream(&e.take_encoder_stream()).unwrap();
        let Decoded::Fields(s) = d.decode_section(i * 4, &section).unwrap() else {
            panic!("section {i} blocked");
        };
        assert_eq!(s.fields, [field], "section {i}");
        assert_eq!(s.required_insert_count, i + 1, "section {i}");
        e.on_decoder_stream(&d.take_decoder_stream()).unwrap();
    }
}
