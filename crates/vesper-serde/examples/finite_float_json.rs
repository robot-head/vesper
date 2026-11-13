//! Bounded JSON-lines corpus for an independent binary64 consumer.
use std::io::Write;
use vesper_serde::{EncodeLimits, FloatLimits, IndexedJson, Limits, Value, encode_into};

const EDGES: &[u64] = &[
    0x0000_0000_0000_0000,
    0x8000_0000_0000_0000,
    0x0000_0000_0000_0001,
    0x8000_0000_0000_0001,
    0x000f_ffff_ffff_ffff,
    0x800f_ffff_ffff_ffff,
    0x0010_0000_0000_0000,
    0x8010_0000_0000_0000,
    0x0010_0000_0000_0001,
    0x8010_0000_0000_0001,
    0x7fef_ffff_ffff_ffff,
    0xffef_ffff_ffff_ffff,
    0x7fef_ffff_ffff_fffe,
    0xffef_ffff_ffff_fffe,
    0x3ff0_0000_0000_0000,
    0xbff0_0000_0000_0000,
    0x3fef_ffff_ffff_ffff,
    0x3ff0_0000_0000_0001,
    0x3cb0_0000_0000_0000,
    0x3fb9_9999_9999_999a,
    0xbfb9_9999_9999_999a,
    0x4340_0000_0000_0000,
    0x4340_0000_0000_0001,
    0xc340_0000_0000_0001,
];

fn emit(bits: u64, output: &mut impl Write) -> Result<(), Box<dyn std::error::Error>> {
    let value = f64::from_bits(bits);
    assert!(value.is_finite());
    let mut spelling = [b'0'; 16];
    for (i, byte) in spelling.iter_mut().enumerate() {
        *byte = b"0123456789abcdef"[((bits >> (60 - i * 4)) & 15) as usize];
    }
    let bits_text = std::str::from_utf8(&spelling)?;
    let fields = [
        ("bits", Value::String(bits_text)),
        ("value", Value::F64(value)),
    ];
    let mut bytes = [0xa5; 1024];
    let length = encode_into(&Value::Object(&fields), &mut bytes, EncodeLimits::default())?;
    assert!(bytes[length..].iter().all(|byte| *byte == 0xa5));
    let parsed = IndexedJson::parse(&bytes[..length], Limits::default())?;
    let restored = parsed
        .root()
        .field("value", 4096)?
        .expect("encoded field")
        .as_f64(FloatLimits::default())?;
    assert_eq!(restored.to_bits(), bits);
    output.write_all(&bytes[..length])?;
    output.write_all(b"\n")?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut output = std::io::stdout().lock();
    for &bits in EDGES {
        emit(bits, &mut output)?;
    }
    let mut state = 0x6a09_e667_f3bc_c909_u64;
    for _ in 0..4096 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        if state & 0x7ff0_0000_0000_0000 != 0x7ff0_0000_0000_0000 {
            emit(state, &mut output)?;
        }
    }
    output.flush()?;
    Ok(())
}
