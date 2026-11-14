//! BPE prompt encoding and temperature / top-p sampling.
//!
//! The encoder is checked against a literal transliteration of llama2.c
//! `run.c` `encode()` (the quadratic merge loop) on generated strings, and,
//! when `VESPER_LLAMA2C_TOKENIZER` points at llama2.c's `tokenizer.bin` (the
//! Llama 2 vocabulary, not bundled), against the token ids upstream's
//! `test.c` records. The sampler is checked against run.c's xorshift
//! generator outputs and against its distribution.
mod common;

use vesper_tensor::kv::KvFormat;
use vesper_tensor::llama::{generate, KvCache, RunOptions, Tokenizer, BOS};
use vesper_tensor::sampling::{sample_mult, sample_top_p, Rng, Sampler, SamplerConfig};
use vesper_tensor::tokenizer::EOS;
use vesper_tensor::Error;

const TOKENIZER: &[u8] = include_bytes!("../models/tok512.bin");

fn tok512() -> Tokenizer {
    Tokenizer::from_llama2c(TOKENIZER, 512).unwrap()
}

/// llama2.c run.c encode(), transliterated literally (O(n^2) merges), with
/// BOS excluded from merges as documented.
fn run_c_encode(t: &Tokenizer, text: &[u8]) -> Vec<u32> {
    let lookup = |s: &[u8]| (0..t.vocab() as u32).find(|&i| t.piece(i) == Some(s));
    let mut tokens = Vec::new();
    if !text.is_empty() {
        tokens.push(lookup(b" ").unwrap());
    }
    let mut buf: Vec<u8> = Vec::new();
    for (i, &c) in text.iter().enumerate() {
        if c & 0xc0 != 0x80 {
            buf.clear();
        }
        buf.push(c);
        if text.get(i + 1).is_some_and(|n| n & 0xc0 == 0x80) && buf.len() < 4 {
            continue;
        }
        match lookup(&buf) {
            Some(id) => tokens.push(id),
            None => tokens.extend(buf.iter().map(|b| u32::from(*b) + 3)),
        }
        buf.clear();
    }
    loop {
        let mut best = (-1e10f32, None);
        for i in 0..tokens.len().saturating_sub(1) {
            let mut pair = t.piece(tokens[i]).unwrap().to_vec();
            pair.extend_from_slice(t.piece(tokens[i + 1]).unwrap());
            if let Some(id) = lookup(&pair) {
                let score = t.score(id).unwrap();
                if score > best.0 {
                    best = (score, Some((i, id)));
                }
            }
        }
        let Some((i, id)) = best.1 else { break };
        tokens[i] = id;
        tokens.remove(i + 1);
    }
    let mut out = vec![BOS];
    out.extend(tokens);
    out
}

#[test]
fn encoder_matches_run_c_and_round_trips() {
    let t = tok512();
    let e = t.encoder();
    // Byte fallback ids in tok512 sit at 3 + byte, as run.c assumes.
    for b in 0..=255u8 {
        assert_eq!(
            e.id(format!("<0x{b:02X}>").as_bytes()),
            Some(3 + u32::from(b))
        );
    }
    assert_eq!(e.encode(b"", true, false).unwrap(), vec![BOS]);
    assert_eq!(e.encode(b"", true, true).unwrap(), vec![BOS, EOS]);
    assert_eq!(e.encode(b"", false, false).unwrap(), Vec::<u32>::new());
    let samples: Vec<&[u8]> = vec![
        b"Once upon a time, there was a little girl named Lily.",
        b"She liked to play outside.  Tom said: \"Hi!\"\n\nThe end.",
        "caf\u{e9} na\u{ef}ve \u{1f600} \u{4e2d}\u{6587}".as_bytes(),
        b"\xff\xfe stray \x80\x80 bytes \xe2\x82",
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbb ab ba",
        b" ",
        b"\x00",
    ];
    let mut rng = common::Rng(99);
    let mut generated = Vec::new();
    let alphabet = b"abcdefghijklmnopqrstuvwxyz        .,!?'\nLTS";
    for _ in 0..150 {
        let n = (rng.next_u64() % 60) as usize;
        let mut s: Vec<u8> = (0..n)
            .map(|_| alphabet[(rng.next_u64() % alphabet.len() as u64) as usize])
            .collect();
        if rng.next_u64().is_multiple_of(4) {
            s.extend_from_slice("\u{e9}\u{2014}".as_bytes());
        }
        generated.push(s);
    }
    for text in samples
        .iter()
        .copied()
        .chain(generated.iter().map(Vec::as_slice))
    {
        let ours = e.encode(text, true, false).unwrap();
        assert_eq!(
            ours,
            run_c_encode(&t, text),
            "{:?}",
            String::from_utf8_lossy(text)
        );
        // Decoding (which drops the dummy-prefix space after BOS) restores
        // the text, byte-fallback tokens included.
        if !text.is_empty() && !text.contains(&0) {
            let mut decoded = Vec::new();
            let mut prev = BOS;
            for &tok in &ours[1..] {
                let piece = t.piece(tok).unwrap();
                let raw = piece
                    .strip_prefix(b"<0x")
                    .and_then(|h| h.strip_suffix(b">"))
                    .map(
                        |h| vec![u8::from_str_radix(std::str::from_utf8(h).unwrap(), 16).unwrap()],
                    );
                let mut piece = raw.unwrap_or_else(|| piece.to_vec());
                if prev == BOS && piece.first() == Some(&b' ') {
                    piece.remove(0);
                }
                decoded.extend(piece);
                prev = tok;
            }
            assert_eq!(decoded, text);
        }
    }
    assert_eq!(
        e.encode(
            &vec![b'a'; vesper_tensor::tokenizer::MAX_PROMPT_BYTES + 1],
            true,
            false
        ),
        Err(Error::Limit("prompt bytes"))
    );
}

#[test]
fn llama2c_test_c_vectors_when_the_llama2_vocabulary_is_available() {
    let Ok(path) = std::env::var("VESPER_LLAMA2C_TOKENIZER") else {
        eprintln!("VESPER_LLAMA2C_TOKENIZER unset; llama2.c test.c vectors skipped");
        return;
    };
    let t = Tokenizer::from_llama2c(&std::fs::read(path).unwrap(), 32_000).unwrap();
    let e = t.encoder();
    let cases: [(&str, &[u32]); 5] = [
        ("", &[1]),
        ("I believe the meaning of life is", &[1, 306, 4658, 278, 6593, 310, 2834, 338]),
        (
            "Simply put, the theory of relativity states that ",
            &[1, 3439, 17632, 1925, 29892, 278, 6368, 310, 14215, 537, 5922, 393, 29871],
        ),
        (
            "A brief message congratulating the team on the launch:\n\n        Hi everyone,\n\n        I just ",
            &[
                1, 319, 11473, 2643, 378, 629, 271, 18099, 278, 3815, 373, 278, 6826, 29901, 13,
                13, 4706, 6324, 14332, 29892, 13, 13, 4706, 306, 925, 29871,
            ],
        ),
        (
            "Translate English to French:\n\n        sea otter => loutre de mer\n        peppermint => menthe poivrée\n        plush girafe => girafe peluche\n        cheese =>",
            &[
                1, 4103, 9632, 4223, 304, 5176, 29901, 13, 13, 4706, 7205, 4932, 357, 1149, 301,
                449, 276, 316, 2778, 13, 4706, 1236, 407, 837, 524, 1149, 6042, 354, 772, 440,
                29878, 1318, 13, 4706, 715, 1878, 330, 3055, 1725, 1149, 330, 3055, 1725, 4639,
                28754, 13, 4706, 923, 968, 1149,
            ],
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(
            e.encode(text.as_bytes(), true, false).unwrap(),
            expected,
            "{text:?}"
        );
    }
}

#[test]
fn rng_matches_run_c_xorshift() {
    // Values of run.c's random_u32() from state 42 (independently computed).
    let mut rng = Rng(42);
    let got: Vec<u32> = (0..5).map(|_| rng.next_u32()).collect();
    assert_eq!(
        got,
        [1456360119, 3359763283, 3393612768, 4054632243, 3283045847]
    );
    let mut rng = Rng(42);
    assert_eq!(rng.next_f32(), (1456360119u32 >> 8) as f32 / 16_777_216.0);
}

#[test]
fn sampling_follows_the_distribution_and_the_nucleus() {
    let probs = [0.5f32, 0.25, 0.125, 0.0625, 0.0625];
    // sample_mult: inverse CDF.
    assert_eq!(sample_mult(&probs, 0.0), 0);
    assert_eq!(sample_mult(&probs, 0.49), 0);
    assert_eq!(sample_mult(&probs, 0.5), 1);
    assert_eq!(sample_mult(&probs, 0.99), 4);
    assert_eq!(sample_mult(&[0.3, 0.3], 0.99), 1); // CDF below 1
                                                   // top-p 0.7 keeps {0, 1} (0.5 + 0.25 > 0.7) and rescales by 0.75.
    let mut scratch = Vec::new();
    for (coin, expected) in [(0.0, 0), (0.66, 0), (0.67, 1), (0.999, 1)] {
        assert_eq!(
            sample_top_p(&probs, 0.7, coin, &mut scratch),
            expected,
            "{coin}"
        );
    }
    // Ties sort by ascending id.
    assert_eq!(sample_top_p(&[0.25; 4], 0.3, 0.9, &mut scratch), 1);
    assert_eq!(sample_top_p(&[1.0], 0.5, 0.5, &mut scratch), 0);
    // top_p below 1/n prunes every token (cutoff 0.9 here); the nucleus is
    // still the most likely token, whatever the coin.
    for coin in [0.0, 0.5, 0.9, 0.999_999] {
        assert_eq!(sample_top_p(&[0.6, 0.4], 0.1, coin, &mut scratch), 0);
        assert_eq!(sample_top_p(&[0.3, 0.4, 0.3], 0.01, coin, &mut scratch), 1);
    }

    // Empirical frequencies at temperature 1 match softmax(logits).
    let logits: Vec<f32> = probs.iter().map(|p| p.ln()).collect();
    let mut sampler = Sampler::new(
        5,
        SamplerConfig {
            temperature: 1.0,
            top_p: 1.0,
            seed: 7,
        },
    )
    .unwrap();
    let n = 40_000;
    let mut counts = [0usize; 5];
    for _ in 0..n {
        counts[sampler.sample(&logits).unwrap() as usize] += 1;
    }
    let chi2: f64 = counts
        .iter()
        .zip(&probs)
        .map(|(c, p)| {
            let e = f64::from(*p) * n as f64;
            (*c as f64 - e).powi(2) / e
        })
        .sum();
    assert!(chi2 < 18.5, "chi-square {chi2} for {counts:?}"); // p ~ 0.001 at 4 dof
                                                              // Top-p never leaves the nucleus; temperature sharpens.
    let mut nucleus = Sampler::new(
        5,
        SamplerConfig {
            temperature: 1.0,
            top_p: 0.7,
            seed: 9,
        },
    )
    .unwrap();
    let mut seen = [0usize; 5];
    for _ in 0..5000 {
        seen[nucleus.sample(&logits).unwrap() as usize] += 1;
    }
    assert_eq!(seen[2..], [0, 0, 0]);
    assert!(seen[0] > 2 * seen[1] - 400 && seen[1] > 1300, "{seen:?}");
    let mut cold = Sampler::new(
        5,
        SamplerConfig {
            temperature: 0.25,
            top_p: 1.0,
            seed: 3,
        },
    )
    .unwrap();
    let zeros = (0..2000)
        .filter(|_| cold.sample(&logits).unwrap() == 0)
        .count();
    assert!(zeros > 1800, "{zeros}");
    let sum: f32 = cold.probabilities().iter().sum();
    assert!((sum - 1.0).abs() < 1e-5);
    // Greedy and rejections.
    let mut greedy = Sampler::new(5, SamplerConfig::GREEDY).unwrap();
    assert_eq!(greedy.sample(&[0.0, 3.0, 3.0, 1.0, -1.0]).unwrap(), 1);
    assert!(greedy.sample(&[0.0; 4]).is_err());
    assert!(greedy.sample(&[f32::NAN; 5]).is_err());
    for config in [
        SamplerConfig {
            temperature: -1.0,
            ..SamplerConfig::GREEDY
        },
        SamplerConfig {
            temperature: f32::NAN,
            ..SamplerConfig::GREEDY
        },
        SamplerConfig {
            top_p: f32::INFINITY,
            ..SamplerConfig::GREEDY
        },
        SamplerConfig {
            seed: 0,
            ..SamplerConfig::GREEDY
        },
    ] {
        assert!(Sampler::new(5, config).is_err(), "{config:?}");
    }
    assert!(Sampler::new(0, SamplerConfig::GREEDY).is_err());
}

#[test]
fn prompted_sampled_generation_is_reproducible() {
    let model = common::stories();
    let t = tok512();
    let prompt = t
        .encoder()
        .encode(b"Once upon a time", true, false)
        .unwrap();
    let config = SamplerConfig {
        temperature: 0.8,
        top_p: 0.9,
        seed: 1234,
    };
    let run = || {
        let mut cache = KvCache::new(model.config(), KvFormat::F32, 1).unwrap();
        let mut s = cache.begin().unwrap();
        let mut sampler = Sampler::new(512, config).unwrap();
        let out = generate(
            &model,
            &mut cache,
            &mut s,
            &prompt,
            64,
            &mut sampler,
            RunOptions::SCALAR,
        )
        .unwrap();
        cache.release(s).map_err(|(e, _)| e).unwrap();
        out
    };
    let a = run();
    assert_eq!(a, run());
    assert_eq!(&a[..prompt.len() - 1], &prompt[1..]);
    let text = String::from_utf8(t.decode_all(BOS, &a).unwrap()).unwrap();
    assert!(text.starts_with("Once upon a time"), "{text}");
    eprintln!("{text}");
}
