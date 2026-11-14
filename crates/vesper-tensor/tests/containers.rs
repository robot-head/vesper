//! Malformed, ambiguous, truncated and over-limit GGUF and safetensors inputs
//! are typed errors, never panics; a memory-mapped file parses exactly like
//! the same bytes in memory.
use vesper_tensor::gguf::{self, GgmlType, Gguf, TensorData, Value};
use vesper_tensor::mmap::Mapping;
use vesper_tensor::safetensors::{self, Dtype, SafeTensors};
use vesper_tensor::Error;

const INTEROP: &[u8] = include_bytes!("../fixtures/interop.gguf");
const EXPECTED: &[u8] = include_bytes!("../fixtures/interop-expected.safetensors");

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn small_gguf() -> Vec<u8> {
    gguf::write(
        &[
            ("general.architecture".into(), Value::String("t".into())),
            ("a".into(), Value::Array(vec![Value::U32(1), Value::U32(2)])),
        ],
        &[
            TensorData {
                name: "x".into(),
                dims: vec![4, 2],
                ty: GgmlType::F32,
                bytes: (0..8u32).flat_map(|v| (v as f32).to_le_bytes()).collect(),
            },
            TensorData {
                name: "y".into(),
                dims: vec![32],
                ty: GgmlType::Q8_0,
                bytes: vec![0; 34],
            },
        ],
    )
    .unwrap()
}

#[test]
fn every_truncation_and_random_corruption_is_an_error_or_a_valid_parse() {
    for bytes in [INTEROP.to_vec(), small_gguf()] {
        for n in 0..bytes.len() {
            assert!(Gguf::from_vec(bytes[..n].to_vec()).is_err(), "prefix {n}");
        }
        let mut rng = Rng(0x5eed);
        for _ in 0..3000 {
            let mut b = bytes.clone();
            for _ in 0..1 + rng.next() % 4 {
                let i = (rng.next() as usize) % b.len();
                b[i] = rng.next() as u8;
            }
            if let Ok(g) = Gguf::from_vec(b) {
                for t in g.tensors() {
                    let _ = g.matrix(&t.name);
                    let _ = g.vector(&t.name);
                }
            }
        }
    }
    let st = EXPECTED.to_vec();
    for n in 0..st.len().min(4096) {
        assert!(
            SafeTensors::from_vec(st[..n].to_vec()).is_err(),
            "prefix {n}"
        );
    }
    let mut rng = Rng(0xfeed);
    let header = 8 + u64::from_le_bytes(st[..8].try_into().unwrap()) as usize;
    for _ in 0..3000 {
        let mut b = st.clone();
        let i = (rng.next() as usize) % header;
        b[i] = rng.next() as u8;
        if let Ok(s) = SafeTensors::from_vec(b) {
            for t in s.tensors() {
                let _ = s.matrix(&t.name);
                let _ = s.vector(&t.name);
            }
        }
    }
}

fn patch(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes[at..at + value.len()].copy_from_slice(value);
}

#[test]
fn gguf_rejections_are_typed() {
    let good = small_gguf();
    assert!(Gguf::from_vec(good.clone()).is_ok());
    let mut b = good.clone();
    b[0] = b'X';
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::Malformed("not a GGUF file")
    );
    let mut b = good.clone();
    patch(&mut b, 4, &1u32.to_le_bytes());
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::Unsupported("GGUF version 1".into())
    );
    let mut b = good.clone();
    patch(&mut b, 8, &u64::MAX.to_le_bytes());
    assert_eq!(Gguf::from_vec(b).unwrap_err(), Error::Limit("GGUF tensors"));
    let mut b = good.clone();
    patch(&mut b, 16, &u64::MAX.to_le_bytes());
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::Limit("GGUF metadata entries")
    );

    let dup = gguf::write(
        &[("k".into(), Value::U8(1)), ("k".into(), Value::U8(2))],
        &[],
    )
    .unwrap();
    assert_eq!(
        Gguf::from_vec(dup).unwrap_err(),
        Error::Malformed("duplicate GGUF metadata key")
    );
    let t = |name: &str| TensorData {
        name: name.into(),
        dims: vec![1],
        ty: GgmlType::F32,
        bytes: vec![0; 4],
    };
    assert_eq!(
        Gguf::from_vec(gguf::write(&[], &[t("a"), t("a")]).unwrap()).unwrap_err(),
        Error::Malformed("duplicate GGUF tensor name")
    );
    // Find the second tensor's offset field and make it overlap / misalign.
    let two = gguf::write(&[], &[t("a"), t("b")]).unwrap();
    let pos = two
        .windows(1)
        .enumerate()
        .filter(|(_, w)| w[0] == b'b')
        .map(|(i, _)| i)
        .next()
        .unwrap();
    let offset_at = pos + 1 + 4 + 8 + 4;
    assert_eq!(&two[offset_at..offset_at + 8], &32u64.to_le_bytes());
    let mut b = two.clone();
    patch(&mut b, offset_at, &0u64.to_le_bytes());
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::Malformed("overlapping GGUF tensors")
    );
    let mut b = two.clone();
    patch(&mut b, offset_at, &4u64.to_le_bytes());
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::Malformed("misaligned GGUF tensor")
    );
    let mut b = two.clone();
    patch(&mut b, offset_at, &(1u64 << 40).to_le_bytes());
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::Malformed("GGUF tensor data outside the file")
    );
    // Unknown and unsupported tensor types.
    let mut b = two.clone();
    patch(&mut b, offset_at - 4, &99u32.to_le_bytes());
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::Unsupported("GGML tensor type 99".into())
    );
    let q6 = gguf::write(
        &[],
        &[TensorData {
            name: "q6".into(),
            dims: vec![256, 1],
            ty: GgmlType(14),
            bytes: vec![0; 210],
        }],
    )
    .unwrap();
    let g = Gguf::from_vec(q6).unwrap();
    assert_eq!(
        g.matrix("q6").unwrap_err(),
        Error::Unsupported("GGML tensor type 14".into())
    );
    assert_eq!(
        g.matrix("zz").unwrap_err(),
        Error::MissingTensor("zz".into())
    );
    // Misaligned quantized rows.
    let mut b = small_gguf();
    let y = b
        .windows(9)
        .position(|w| w == b"\x01\0\0\0\0\0\0\0y")
        .unwrap()
        + 8;
    patch(&mut b, y + 1 + 4, &33u64.to_le_bytes());
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::QuantBlockAlignment {
            cols: 33,
            block: 32
        }
    );
    // A huge array count in a small file is a truncation error, not an
    // allocation.
    let mut b = small_gguf();
    let a = b
        .windows(9)
        .position(|w| w == b"\x01\0\0\0\0\0\0\0a")
        .unwrap()
        + 9;
    patch(&mut b, a + 4 + 4, &(1u64 << 23).to_le_bytes());
    assert_eq!(
        Gguf::from_vec(b).unwrap_err(),
        Error::Malformed("truncated GGUF array")
    );
    // Bad general.alignment.
    // (The writer refuses it too, so patch a valid file's value.)
    let mut bad = gguf::write(&[("general.alignment".into(), Value::U32(32))], &[]).unwrap();
    let at = bad
        .windows(17)
        .position(|w| w == b"general.alignment")
        .unwrap()
        + 17
        + 4;
    patch(&mut bad, at, &3u32.to_le_bytes());
    assert_eq!(
        Gguf::from_vec(bad).unwrap_err(),
        Error::Malformed("general.alignment")
    );
    // Writer refuses inconsistent tensors.
    assert!(gguf::write(
        &[],
        &[TensorData {
            bytes: vec![0; 3],
            ..t("a")
        }]
    )
    .is_err());
}

fn st_with_header(header: &str, data: &[u8]) -> Vec<u8> {
    let mut b = (header.len() as u64).to_le_bytes().to_vec();
    b.extend_from_slice(header.as_bytes());
    b.extend_from_slice(data);
    b
}

#[test]
fn safetensors_rejections_are_typed() {
    let ok = st_with_header(
        r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[0,8]}}  "#,
        &[0; 8],
    );
    let st = SafeTensors::from_vec(ok).unwrap();
    assert_eq!(st.vector("a").unwrap(), vec![0.0, 0.0]);
    let cases: &[(&str, usize, Error)] = &[
        (
            r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[0,8]}}"#,
            9,
            Error::Malformed("trailing bytes after safetensors data"),
        ),
        (
            r#"{"a":{"dtype":"F32","shape":[3],"data_offsets":[0,8]}}"#,
            8,
            Error::Malformed("safetensors tensor byte length"),
        ),
        (
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[4,8]}}"#,
            8,
            Error::Malformed("safetensors tensors must tile the data buffer"),
        ),
        (
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0,4]},"b":{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}"#,
            4,
            Error::Malformed("safetensors tensors must tile the data buffer"),
        ),
        (
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0,4]},"a":{"dtype":"F32","shape":[1],"data_offsets":[4,8]}}"#,
            8,
            Error::Malformed("duplicate JSON key"),
        ),
        (
            r#"{"a":{"dtype":"F4","shape":[1],"data_offsets":[0,1]}}"#,
            1,
            Error::Unsupported("safetensors dtype F4".into()),
        ),
        (
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0,4],"x":1}}"#,
            4,
            Error::Malformed("unknown safetensors tensor field"),
        ),
        (
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[8,4]}}"#,
            8,
            Error::Malformed("safetensors data_offsets out of range"),
        ),
        (
            r#"{"a":{"dtype":"F32","shape":[4294967296,4294967296,16],"data_offsets":[0,4]}}"#,
            4,
            Error::Overflow,
        ),
        (
            r#"{"__metadata__":{"k":1}}"#,
            0,
            Error::Malformed("safetensors metadata value"),
        ),
        (
            r#"[]"#,
            0,
            Error::Malformed("safetensors header must be a JSON object"),
        ),
        (
            r#"{"a":1.5}"#,
            0,
            Error::Unsupported("non-integer JSON number".into()),
        ),
    ];
    for (header, data, error) in cases {
        assert_eq!(
            SafeTensors::from_vec(st_with_header(header, &vec![0; *data])).unwrap_err(),
            *error,
            "{header}"
        );
    }
    let mut huge = (200_000_000u64).to_le_bytes().to_vec();
    huge.extend_from_slice(b"{}");
    assert_eq!(
        SafeTensors::from_vec(huge).unwrap_err(),
        Error::Limit("safetensors header bytes")
    );
    // Typed lookups.
    let st = SafeTensors::from_vec(
        safetensors::write(
            &[
                safetensors::TensorData {
                    name: "i".into(),
                    dtype: Dtype::I64,
                    shape: vec![1, 1],
                    bytes: vec![0; 8],
                },
                safetensors::TensorData {
                    name: "v".into(),
                    dtype: Dtype::F32,
                    shape: vec![2],
                    bytes: 1f32.to_le_bytes().repeat(2),
                },
                safetensors::TensorData {
                    name: "nan".into(),
                    dtype: Dtype::F32,
                    shape: vec![1, 1],
                    bytes: f32::NAN.to_le_bytes().to_vec(),
                },
            ],
            &[("format".into(), "pt".into())],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(st.metadata(), &[("format".to_string(), "pt".to_string())]);
    assert_eq!(
        st.matrix("i").unwrap_err(),
        Error::Unsupported("safetensors dtype I64 as weights".into())
    );
    assert_eq!(
        st.matrix("v").unwrap_err(),
        Error::Shape("safetensors matrix must be 2-D")
    );
    assert_eq!(st.matrix("nan").unwrap_err(), Error::NonFinite);
    assert_eq!(
        st.matrix("q").unwrap_err(),
        Error::MissingTensor("q".into())
    );
}

#[test]
fn mapped_files_parse_like_memory_and_borrow_their_bytes() {
    let dir = std::env::temp_dir().join(format!("vesper-tensor-mmap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("interop.gguf");
    std::fs::write(&path, INTEROP).unwrap();
    let mapped = Gguf::open(&path).unwrap();
    assert!(mapped.mapping().is_mapped());
    let memory = Gguf::from_vec(INTEROP.to_vec()).unwrap();
    assert!(!memory.mapping().is_mapped());
    assert_eq!(mapped.tensors(), memory.tensors());
    assert_eq!(mapped.metadata(), memory.metadata());
    for t in mapped.tensors() {
        if t.dims.len() == 2 {
            let m = mapped.matrix(&t.name).unwrap();
            assert!(m.is_file_mapped(), "{}", t.name);
            assert_eq!(m, memory.matrix(&t.name).unwrap());
            assert!(!memory.matrix(&t.name).unwrap().is_file_mapped());
        }
    }
    let read = Mapping::read(&path).unwrap();
    assert_eq!(read.bytes(), INTEROP);
    let empty = dir.join("empty");
    std::fs::write(&empty, b"").unwrap();
    assert!(Gguf::open(&empty).is_err());
    assert_eq!(
        Gguf::open(&dir.join("missing")).unwrap_err(),
        Error::Io("open weight file")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn gguf_writer_pads_to_the_alignment_its_metadata_declares() {
    for alignment in [1u32, 8, 16, 32, 64, 256] {
        let tensors = [
            TensorData {
                name: "xxxxxxxx".into(),
                dims: vec![3],
                ty: GgmlType::F32,
                bytes: [1.0f32, 2.0, 3.0]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
            },
            TensorData {
                name: "y".into(),
                dims: vec![1],
                ty: GgmlType::F32,
                bytes: 4.0f32.to_le_bytes().to_vec(),
            },
        ];
        let meta = [("general.alignment".to_string(), Value::U32(alignment))];
        let g = Gguf::from_vec(gguf::write(&meta, &tensors).unwrap()).unwrap();
        assert_eq!(g.alignment(), alignment as usize);
        for t in &tensors {
            assert_eq!(
                g.bytes(g.tensor(&t.name).unwrap()),
                &t.bytes[..],
                "{alignment}"
            );
        }
    }
    for bad in [Value::U32(0), Value::U32(24), Value::U64(32)] {
        let meta = [("general.alignment".to_string(), bad)];
        assert_eq!(
            gguf::write(&meta, &[]),
            Err(Error::Malformed("general.alignment"))
        );
    }
    let empty_dims = TensorData {
        name: "z".into(),
        dims: vec![],
        ty: GgmlType::F32,
        bytes: vec![],
    };
    assert!(gguf::write(&[], &[empty_dims]).is_err());
}
