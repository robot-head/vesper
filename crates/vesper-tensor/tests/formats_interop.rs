//! Q4_K, MXFP4, FP8, F16/BF16, Q8_0 and Q4_0 against independent
//! implementations: the fixtures in fixtures/ were written by gguf-py 0.19.0
//! (llama.cpp's Python package), safetensors 0.8.0 and ml_dtypes 0.6.0 (see
//! fixtures/make_interop.py). Nothing here is produced by vesper-tensor.
use vesper_tensor::formats::{self, Fp8};
use vesper_tensor::gguf::{GgmlType, Gguf, Value};
use vesper_tensor::quant::{self, Matrix, WeightFormat};
use vesper_tensor::safetensors::{Dtype, SafeTensors};
use vesper_tensor::Error;

const INTEROP: &[u8] = include_bytes!("../fixtures/interop.gguf");
const EXPECTED: &[u8] = include_bytes!("../fixtures/interop-expected.safetensors");

fn expected() -> SafeTensors {
    SafeTensors::from_vec(EXPECTED.to_vec()).unwrap()
}
fn f32s(st: &SafeTensors, name: &str) -> Vec<f32> {
    let info = st.get(name).unwrap_or_else(|| panic!("{name}"));
    assert_eq!(info.dtype, Dtype::F32, "{name}");
    st.bytes(info)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}
fn u8s(st: &SafeTensors, name: &str) -> Vec<u8> {
    let info = st.get(name).unwrap_or_else(|| panic!("{name}"));
    assert_eq!(info.dtype, Dtype::U8, "{name}");
    st.bytes(info).to_vec()
}
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn gguf_py_metadata_parses_exactly() {
    let g = Gguf::from_vec(INTEROP.to_vec()).unwrap();
    assert_eq!(g.version(), 3);
    assert_eq!(g.alignment(), 32);
    assert_eq!(
        g.get("general.architecture").and_then(Value::as_str),
        Some("interop")
    );
    assert_eq!(
        g.get("interop.string").and_then(Value::as_str),
        Some("héllo ▁world")
    );
    assert_eq!(g.get("interop.u32"), Some(&Value::U32(4_000_000_000)));
    assert_eq!(g.get("interop.i32"), Some(&Value::I32(-7)));
    assert_eq!(g.get("interop.f32"), Some(&Value::F32(0.15625)));
    assert_eq!(g.get("interop.bool"), Some(&Value::Bool(true)));
    assert_eq!(g.get("interop.u64"), Some(&Value::U64(1 << 40)));
    let strings: Vec<&str> = g
        .get("interop.strings")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(strings, ["a", "▁b", "<0x0A>"]);
    let ints: Vec<u64> = g
        .get("interop.ints")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(ints, [1, 2, 3]);
    let types: Vec<(String, GgmlType)> =
        g.tensors().iter().map(|t| (t.name.clone(), t.ty)).collect();
    assert_eq!(
        types,
        [
            ("f32".to_string(), GgmlType::F32),
            ("vec".into(), GgmlType::F32),
            ("f16".into(), GgmlType::F16),
            ("bf16".into(), GgmlType::BF16),
            ("q8_0".into(), GgmlType::Q8_0),
            ("q4_0".into(), GgmlType::Q4_0),
            ("mxfp4".into(), GgmlType::MXFP4),
            ("q4_k".into(), GgmlType::Q4_K),
        ]
    );
}

#[test]
fn every_gguf_py_tensor_dequantizes_bit_identically_to_gguf_py() {
    let g = Gguf::from_vec(INTEROP.to_vec()).unwrap();
    let st = expected();
    for name in ["f32", "f16", "bf16", "q8_0", "q4_0", "mxfp4", "q4_k"] {
        let m = g.matrix(name).unwrap();
        let ours = m.to_f32().unwrap();
        assert_eq!(
            bits(&ours),
            bits(&f32s(&st, &format!("dequant.{name}"))),
            "{name}"
        );
        // Row-wise decoding agrees with whole-matrix decoding.
        let mut row = vec![0.0; m.cols()];
        m.dequantize_row(m.rows() - 1, &mut row).unwrap();
        assert_eq!(
            bits(&row),
            bits(&ours[(m.rows() - 1) * m.cols()..]),
            "{name} row"
        );
    }
    assert_eq!(
        bits(&g.vector("vec").unwrap()),
        bits(&f32s(&st, "dequant.vec"))
    );
    assert_eq!(g.matrix("mxfp4").unwrap().format(), WeightFormat::Mxfp4);
    assert_eq!(g.matrix("q4_k").unwrap().format(), WeightFormat::Q4K);
}

#[test]
fn our_quantizers_produce_gguf_py_bytes() {
    let st = expected();
    for (name, format, cols) in [
        ("q8_0", WeightFormat::Q8_0, 64),
        ("q4_0", WeightFormat::Q4_0, 64),
        ("mxfp4", WeightFormat::Mxfp4, 64),
        ("bf16", WeightFormat::Bf16, 8),
    ] {
        let source = f32s(&st, &format!("source.{name}"));
        let rows = source.len() / cols;
        let ours = Matrix::f32(rows, cols, source)
            .unwrap()
            .quantize(format)
            .unwrap();
        assert_eq!(
            ours.raw_bytes().unwrap(),
            u8s(&st, &format!("bytes.{name}")).as_slice(),
            "{name}"
        );
    }
}

#[test]
fn fp8_codes_match_ml_dtypes() {
    let st = expected();
    for (fp8, name) in [(Fp8::E4M3, "e4m3"), (Fp8::E5M2, "e5m2")] {
        let table = f32s(&st, &format!("fp8.{name}.decoded"));
        for code in 0..=255u8 {
            let ours = fp8.decode(code);
            let theirs = table[code as usize];
            if theirs.is_nan() {
                assert!(ours.is_nan(), "{name} {code:#x}");
            } else {
                assert_eq!(ours.to_bits(), theirs.to_bits(), "{name} {code:#x}");
            }
        }
        let inputs = f32s(&st, &format!("fp8.{name}.inputs"));
        let encoded = u8s(&st, &format!("fp8.{name}.encoded"));
        assert!(inputs.len() > 2000);
        for (x, code) in inputs.iter().zip(&encoded) {
            assert_eq!(fp8.encode_saturating(*x), *code, "{name} {x:e}");
        }
        // A tensor that safetensors itself stored with the FP8 dtype.
        let info = st.get(&format!("fp8.{name}.as_dtype")).unwrap();
        assert_eq!(
            info.dtype,
            if fp8 == Fp8::E4M3 {
                Dtype::F8E4M3
            } else {
                Dtype::F8E5M2
            }
        );
        assert_eq!(st.bytes(info), &encoded[..16]);
    }
    assert_eq!(Fp8::E4M3.encode_saturating(1e6), 0x7e);
    assert_eq!(Fp8::E4M3.encode_saturating(-1e6), 0xfe);
    assert_eq!(Fp8::E5M2.encode_saturating(f32::INFINITY), 0x7b);
    assert!(Fp8::E4M3
        .decode(Fp8::E4M3.encode_saturating(f32::NAN))
        .is_nan());
}

#[test]
fn q4_k_quantizer_reconstructs_within_its_stored_step() {
    let mut seed = 7u64;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    };
    let data: Vec<f32> = (0..4 * 512)
        .map(|i| next() * (1.0 + (i % 7) as f32))
        .collect();
    let m = Matrix::f32(4, 512, data.clone()).unwrap();
    let q = m.quantize(WeightFormat::Q4K).unwrap();
    assert_eq!(q.payload_bytes(), 4 * 2 * formats::Q4_K_BYTES);
    let back = q.to_f32().unwrap();
    for (block, (x, y)) in data.chunks(32).zip(back.chunks(32)).enumerate() {
        let lo = x.iter().copied().fold(0.0f32, f32::min);
        let hi = x.iter().copied().fold(f32::MIN, f32::max);
        // Half a quantization step plus the 6-bit scale/offset rounding.
        let step = (hi - lo) / 15.0;
        for (a, b) in x.iter().zip(y) {
            assert!(
                (a - b).abs() <= 0.5 * step + 0.06 * (hi - lo) + 1e-3,
                "block {block}: {a} vs {b}"
            );
        }
    }
    // Rejections: alignment and non-finite block scales.
    assert_eq!(
        Matrix::f32(1, 32, vec![0.0; 32])
            .unwrap()
            .quantize(WeightFormat::Q4K),
        Err(Error::Shape("quantized rows need whole blocks"))
    );
    let mut bad = q.raw_bytes().unwrap().to_vec();
    bad[0..2].copy_from_slice(&0x7c00u16.to_le_bytes());
    assert_eq!(
        Matrix::quantized(WeightFormat::Q4K, 4, 512, bad),
        Err(Error::NonFinite)
    );
}

#[test]
fn mxfp4_and_fp8_round_trips_and_rejections() {
    // MXFP4 represents E2M1 values times a power of two exactly.
    let exact: Vec<f32> = (0..32)
        .map(|i| {
            [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0][i % 8] * if i % 3 == 0 { -1.0 } else { 1.0 }
        })
        .collect();
    let m = Matrix::f32(1, 32, exact.clone()).unwrap();
    let q = m.quantize(WeightFormat::Mxfp4).unwrap();
    assert_eq!(q.raw_bytes().unwrap()[0], 127); // amax 6: floor(log2) - 2 + 127
    assert_eq!(
        bits(&q.to_f32().unwrap()),
        bits(&exact.iter().map(|v| v + 0.0).collect::<Vec<_>>())
    );
    let mut nan = q.raw_bytes().unwrap().to_vec();
    nan[0] = 0xff;
    assert_eq!(
        Matrix::quantized(WeightFormat::Mxfp4, 1, 32, nan),
        Err(Error::NonFinite)
    );
    assert_eq!(formats::e8m0_half(0), f32::from_bits(0x0020_0000)); // 2^-128
    assert_eq!(formats::e8m0_half(1), f32::from_bits(0x0040_0000)); // 2^-127
    assert_eq!(formats::e8m0_half(254), f32::from_bits(253 << 23)); // 2^126

    for format in [WeightFormat::F8E4M3, WeightFormat::F8E5M2] {
        let data: Vec<f32> = (0..96).map(|i| (i as f32 - 40.0) * 0.37).collect();
        let q = Matrix::f32(3, 32, data.clone())
            .unwrap()
            .quantize(format)
            .unwrap();
        let rel = if format == WeightFormat::F8E4M3 {
            1.0 / 16.0
        } else {
            1.0 / 8.0
        };
        for (a, b) in data.iter().zip(q.to_f32().unwrap()) {
            assert!(
                (a - b).abs() <= rel * a.abs() + 1e-6 * q.scale(),
                "{format:?} {a} {b}"
            );
        }
        // NaN and infinity codes, zero or non-finite scales are rejected.
        let bytes = q.raw_bytes().unwrap().to_vec();
        let mut nan = bytes.clone();
        nan[5] = 0x7f;
        assert_eq!(
            Matrix::from_bytes(
                format,
                3,
                32,
                vesper_tensor::mmap::WeightBytes::Owned(nan),
                1.0
            ),
            Err(Error::NonFinite)
        );
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                Matrix::from_bytes(
                    format,
                    3,
                    32,
                    vesper_tensor::mmap::WeightBytes::Owned(bytes.clone()),
                    scale
                ),
                Err(Error::NonFinite)
            );
        }
    }
    assert_eq!(
        Matrix::from_bytes(
            WeightFormat::Q8_0,
            1,
            32,
            vesper_tensor::mmap::WeightBytes::Owned(vec![0; 34]),
            2.0
        ),
        Err(Error::Shape("only FP8 matrices carry a scale"))
    );
    assert!(Fp8::E5M2.decode(0x7c).is_infinite());
    assert!(!Fp8::E5M2.is_finite_code(0x7c));
}

#[test]
fn every_format_rejects_misaligned_and_wrong_length_rows() {
    for format in [
        WeightFormat::Q8_0,
        WeightFormat::Q4_0,
        WeightFormat::Q4K,
        WeightFormat::Mxfp4,
    ] {
        let cols = format.block_elements() + 1;
        assert_eq!(
            Matrix::quantized(format, 1, cols, vec![0; 1024]),
            Err(Error::QuantBlockAlignment {
                cols,
                block: format.block_elements()
            })
        );
    }
    for format in [
        WeightFormat::F32,
        WeightFormat::F16,
        WeightFormat::Bf16,
        WeightFormat::F8E4M3,
        WeightFormat::Q4K,
    ] {
        let cols = format.block_elements();
        let n = format.row_bytes(cols).unwrap();
        assert_eq!(
            Matrix::quantized(format, 2, cols, vec![0; n]),
            Err(Error::Shape("matrix byte length"))
        );
        assert!(Matrix::quantized(format, 2, cols, vec![0; 2 * n]).is_ok());
    }
    // Existing Q4_0 layout helpers still agree with the generic decoder.
    let mut block = [0u8; quant::Q4_0_BYTES];
    block[0..2].copy_from_slice(&quant::encode_fp16(0.5).to_le_bytes());
    block[2] = 0x9f;
    let mut out = [0.0f32; 32];
    quant::dequantize_q4_0_block(&block, &mut out);
    let m = Matrix::quantized(WeightFormat::Q4_0, 1, 32, block.to_vec()).unwrap();
    assert_eq!(bits(&m.to_f32().unwrap()), bits(&out));
}
