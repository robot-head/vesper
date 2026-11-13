//! RFC 9204 Appendix B: the encoder and decoder exchanges, byte for byte,
//! replayed against our decoder, plus our encoder's view of the same
//! acknowledgments.

use vesper_prism_h3::qpack::{Decoded, Decoder, DecoderConfig, Encoder, EncoderConfig, Field};

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn fields(section: &Decoded) -> Vec<(String, String)> {
    let Decoded::Fields(s) = section else {
        panic!("blocked")
    };
    s.fields
        .iter()
        .map(|f| {
            (
                String::from_utf8(f.name.clone()).unwrap(),
                String::from_utf8(f.value.clone()).unwrap(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect()
}

#[test]
fn appendix_b_decoder_side() {
    let mut d = Decoder::new(DecoderConfig {
        max_table_capacity: 220,
        ..DecoderConfig::default()
    });
    // B.1: literal with static name reference.
    let s = d
        .decode_section(0, &hex("0000 510b 2f69 6e64 6578 2e68 746d 6c"))
        .unwrap();
    assert_eq!(fields(&s), pairs(&[(":path", "/index.html")]));
    assert!(d.take_decoder_stream().is_empty(), "no dynamic references");

    // B.2: the section arrives first and blocks; the encoder stream unblocks it.
    let section = hex("0381 10 11");
    assert_eq!(d.decode_section(4, &section).unwrap(), Decoded::Blocked);
    assert_eq!(d.blocked_streams(), 1);
    let enc =
        hex("3fbd01 c00f 7777 772e 6578 616d 706c 652e 636f 6d c10c 2f73 616d 706c 652f 7061 7468");
    // Byte by byte: partial instructions wait.
    for b in &enc {
        d.on_encoder_stream(std::slice::from_ref(b)).unwrap();
    }
    assert_eq!(d.table_size(), 106);
    let s = d.poll_unblocked().unwrap().unwrap();
    assert_eq!(
        fields(&Decoded::Fields(s)),
        pairs(&[(":authority", "www.example.com"), (":path", "/sample/path")])
    );
    assert_eq!(
        d.take_decoder_stream(),
        hex("84"),
        "Section Acknowledgment (stream=4)"
    );

    // B.3: speculative insert, acknowledged by an Insert Count Increment.
    d.on_encoder_stream(&hex(
        "4a63 7573 746f 6d2d 6b65 790c 6375 7374 6f6d 2d76 616c 7565",
    ))
    .unwrap();
    assert_eq!(d.table_size(), 160);
    assert_eq!(d.take_decoder_stream(), hex("01"));

    // B.4: duplicate, then a section that the decoder cancels.
    d.on_encoder_stream(&hex("02")).unwrap();
    assert_eq!(d.table_size(), 217);
    let s = d.decode_section(8, &hex("0500 80 c1 81")).unwrap();
    assert_eq!(
        fields(&s),
        pairs(&[
            (":authority", "www.example.com"),
            (":path", "/"),
            ("custom-key", "custom-value")
        ])
    );
    // (The RFC's decoder never saw the section: it cancels instead.)
    let _ = d.take_decoder_stream();
    d.cancel_stream(8);
    assert_eq!(
        d.take_decoder_stream(),
        hex("48"),
        "Stream Cancellation (Stream=8)"
    );

    // B.5: insert with dynamic name reference evicts the oldest entry.
    d.on_encoder_stream(&hex("810d 6375 7374 6f6d 2d76 616c 7565 32"))
        .unwrap();
    assert_eq!(d.table_size(), 215);
    assert_eq!(d.insert_count(), 5);
    assert_eq!(d.table_len(), 4);
}

#[test]
fn appendix_b_encoder_side() {
    let mut e = Encoder::new(EncoderConfig {
        max_table_capacity: 220,
        ..EncoderConfig::default()
    });
    e.apply_peer_settings(220, 100);
    assert_eq!(
        e.take_encoder_stream(),
        hex("3fbd01"),
        "Set Dynamic Table Capacity=220"
    );
    // B.1 with no dynamic table use: the same bytes as the RFC.
    let mut plain = Encoder::new(EncoderConfig::default());
    let s = plain.encode(0, &[Field::new(":path", "/index.html")]);
    assert_eq!(s[..2], [0, 0]);
    let mut d = Decoder::new(DecoderConfig::default());
    assert_eq!(
        fields(&d.decode_section(0, &s).unwrap()),
        pairs(&[(":path", "/index.html")])
    );

    // B.2: two inserts and a blocking section referencing them post-base.
    let s = e.encode(
        4,
        &[
            Field::new(":authority", "www.example.com"),
            Field::new(":path", "/sample/path"),
        ],
    );
    assert_eq!(s, hex("0381 10 11"));
    let enc = e.take_encoder_stream();
    // Our encoder Huffman-codes the values; the RFC example does not, so
    // check the instructions by decoding them.
    let mut d = Decoder::new(DecoderConfig {
        max_table_capacity: 220,
        ..DecoderConfig::default()
    });
    d.on_encoder_stream(&hex("3fbd01")).unwrap();
    d.on_encoder_stream(&enc).unwrap();
    assert_eq!(d.table_size(), 106);
    assert_eq!(e.blocking_streams(), 1);
    assert_eq!(e.known_received_count(), 0);
    // The decoder's Section Acknowledgment (stream 4).
    e.on_decoder_stream(&hex("84")).unwrap();
    assert_eq!(e.known_received_count(), 2);
    assert_eq!(e.blocking_streams(), 0);
    assert_eq!(e.outstanding_sections(), 0);
    // A second acknowledgment for stream 4 is a decoder stream error.
    assert!(e.on_decoder_stream(&hex("84")).is_err());
}

#[test]
fn encoder_respects_acknowledgment_rules() {
    let mut e = Encoder::new(EncoderConfig::default());
    e.apply_peer_settings(4096, 1);
    let _ = e.take_encoder_stream();
    // Zero increment and an increment past the inserts are errors.
    assert!(e.on_decoder_stream(&hex("00")).is_err());
    let mut e = Encoder::new(EncoderConfig::default());
    e.apply_peer_settings(4096, 1);
    assert!(
        e.on_decoder_stream(&hex("01")).is_err(),
        "nothing was inserted"
    );

    // With one blocked stream allowed, the second stream may not block.
    let mut e = Encoder::new(EncoderConfig::default());
    e.apply_peer_settings(4096, 1);
    let first = e.encode(0, &[Field::new("x-one", "1")]);
    assert_ne!(first[0], 0, "first section references its insert");
    let second = e.encode(4, &[Field::new("x-two", "2")]);
    assert_eq!(second[0], 0, "second may not block: literal only");
    // Once acknowledged, the inserted entry is referenced without blocking.
    e.on_decoder_stream(&hex("80")).unwrap();
    let third = e.encode(8, &[Field::new("x-one", "1")]);
    assert_eq!(e.blocking_streams(), 0);
    assert_ne!(third[0], 0);
    // Sensitive fields are never inserted and carry the N bit.
    let inserts = e.stats().inserts;
    let s = e.encode(12, &[Field::sensitive("authorization", "secret")]);
    assert_eq!(e.stats().inserts, inserts);
    let mut d = Decoder::new(DecoderConfig::default());
    let Decoded::Fields(f) = d.decode_section(12, &s).unwrap() else {
        panic!()
    };
    assert!(f.fields[0].sensitive);
}
