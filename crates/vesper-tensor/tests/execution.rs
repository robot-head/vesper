use vesper_tensor::{host::*, Error};

fn bytes_for(rows: usize, cols: usize) -> Vec<u8> {
    let scales: [u16; 8] = [0x3c00, 0xb800, 1, 0x7bff, 0x8000, 0x0400, 0x3555, 0xbc00];
    let mut bytes = Vec::new();
    for block in 0..rows * (cols / 32) {
        bytes.extend(scales[block % scales.len()].to_le_bytes());
        bytes.extend(
            (0..16).map(|i| (((block + i * 3) % 16) | (((block * 5 + i * 7) % 16) << 4)) as u8),
        );
    }
    bytes
}
fn fixture(rows: usize, cols: usize, residual: bool) -> Result<(Plan, Plan, Bindings), Error> {
    let limits = Limits::default();
    let mut bindings = Bindings::from([
        (
            "x".into(),
            Tensor::vector(
                (0..cols)
                    .map(|i| (i as f32 % 17.0 - 8.0) * 0.03125)
                    .collect(),
            ),
        ),
        (
            "norm".into(),
            Tensor::vector((0..cols).map(|i| (i as f32 % 11.0 - 5.0) * 0.125).collect()),
        ),
        (
            "matrix".into(),
            Tensor::q4_matrix(rows, cols, bytes_for(rows, cols)),
        ),
    ]);
    if residual {
        bindings.insert(
            "residual".into(),
            Tensor::vector((0..rows).map(|i| i as f32 * 0.25 - 2.0).collect()),
        );
    }
    let schema = Schema::new(
        bindings.iter().map(|(n, t)| (n.clone(), t.spec.clone())),
        limits,
    )?;
    let mut graph = Graph::new(schema, limits)?;
    let x = graph.input("x")?;
    let norm = graph.rms_norm(x, "norm", 1e-5)?;
    let mut out = graph.q4_matvec(norm, "matrix")?;
    if residual {
        let r = graph.input("residual")?;
        out = graph.residual(out, r)?;
    }
    Ok((
        graph.compile(out, false)?,
        graph.compile(out, true)?,
        bindings,
    ))
}
fn f32s<'a>(bindings: &'a Bindings, name: &str) -> &'a [f32] {
    let Storage::F32(v) = &bindings[name].storage else {
        panic!("test type")
    };
    v
}
// Independent materialized reference: decode a complete row into FP32 values,
// normalize first, then dot and residual with the specified scalar order.
fn reference(bindings: &Bindings, residual: bool) -> Vec<f32> {
    let x = f32s(bindings, "x");
    let norm = f32s(bindings, "norm");
    let mut ss = 0f32;
    for a in x {
        ss += a * a;
    }
    let inv = 1.0 / (ss / x.len() as f32 + 1e-5).sqrt();
    let activation: Vec<f32> = x.iter().zip(norm).map(|(a, w)| (a * inv) * w).collect();
    let matrix = &bindings["matrix"];
    let Storage::Q4Bytes(raw) = &matrix.storage else {
        panic!("test type")
    };
    let cols = matrix.spec.shape[1];
    let mut output = Vec::new();
    for row in raw.chunks_exact(cols / 32 * 18) {
        let mut dequant = Vec::new();
        for block in row.as_chunks::<18>().0.iter() {
            let scale = half_reference(u16::from_le_bytes([block[0], block[1]]));
            for high in [false, true] {
                for packed in &block[2..] {
                    let nibble = if high { packed >> 4 } else { packed & 15 };
                    dequant.push(scale * (i32::from(nibble) - 8) as f32);
                }
            }
        }
        let mut acc = 0f32;
        for (w, x) in dequant.iter().zip(&activation) {
            acc += w * x;
        }
        if residual {
            acc += f32s(bindings, "residual")[output.len()];
        }
        output.push(acc);
    }
    output
}
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|v| v.to_bits()).collect()
}
fn half_reference(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let e = (bits >> 10) & 31;
    let f = bits & 1023;
    if e == 0 {
        sign * f32::from(f) * 2.0f32.powi(-24)
    } else if e == 31 {
        if f == 0 {
            sign * f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        sign * f32::from(1024 + f) * 2.0f32.powi(i32::from(e) - 25)
    }
}

#[test]
fn exhaustive_binary16_decoding_and_special_payloads() {
    for bits in 0..=u16::MAX {
        let actual = decode_fp16(bits);
        let expected = half_reference(bits);
        if expected.is_nan() {
            assert!(actual.is_nan());
            assert_eq!(actual.to_bits() & 0x007f_ffff, u32::from(bits & 1023) << 13);
        } else {
            assert_eq!(actual.to_bits(), expected.to_bits(), "half={bits:04x}");
        }
    }
    assert_eq!(decode_fp16(1), 2.0f32.powi(-24));
    assert_eq!(decode_fp16(0x8000).to_bits(), (-0.0f32).to_bits());
}

#[test]
fn actual_fused_unfused_and_reference_values_match_on_quant_boundaries_and_row_tails(
) -> Result<(), Error> {
    for cols in [32, 64, 96] {
        for rows in [1, 3, 17] {
            for residual in [false, true] {
                let (plain, fused, bindings) = fixture(rows, cols, residual)?;
                let actual = plain.execute(&bindings)?;
                assert_eq!(bits(&actual), bits(&fused.execute(&bindings)?));
                assert_eq!(bits(&actual), bits(&reference(&bindings, residual)));
                assert_eq!(fused.stats().fused_nodes, 1);
                assert_eq!(plain.stats().values - fused.stats().values, cols);
            }
        }
    }
    Ok(())
}

#[test]
fn rms_supports_non_quantized_vector_tails_and_zero_values() -> Result<(), Error> {
    for n in [1, 7, 31, 33, 65] {
        let bindings = Bindings::from([
            ("x".into(), Tensor::vector(vec![0.0; n])),
            ("w".into(), Tensor::vector(vec![-1.0; n])),
        ]);
        let limits = Limits::default();
        let schema = Schema::new(
            bindings.iter().map(|(n, t)| (n.clone(), t.spec.clone())),
            limits,
        )?;
        let mut g = Graph::new(schema, limits)?;
        let x = g.input("x")?;
        let y = g.rms_norm(x, "w", 1e-5)?;
        assert_eq!(
            bits(&g.compile(y, true)?.execute(&bindings)?),
            vec![(-0.0f32).to_bits(); n]
        );
    }
    Ok(())
}

#[test]
fn packed_nibble_order_and_signed_scales_have_known_values() -> Result<(), Error> {
    let limits = Limits::default();
    let mut raw = Vec::from(0xbc00u16.to_le_bytes()); // -1
    raw.extend([0xf0; 16]); // first half -8, second half 7
    let mut x = vec![0.0; 32];
    x[0] = 2.0;
    x[16] = 3.0;
    let bindings = Bindings::from([
        ("x".into(), Tensor::vector(x)),
        ("m".into(), Tensor::q4_matrix(1, 32, raw)),
    ]);
    let schema = Schema::new(
        bindings.iter().map(|(n, t)| (n.clone(), t.spec.clone())),
        limits,
    )?;
    let mut g = Graph::new(schema, limits)?;
    let x = g.input("x")?;
    let out = g.q4_matvec(x, "m")?;
    assert_eq!(g.compile(out, true)?.execute(&bindings)?, vec![-5.0]);
    Ok(())
}

#[test]
fn all_bindings_are_validated_before_compute_and_schema_changes_fail_closed() -> Result<(), Error> {
    let (plain, fused, bindings) = fixture(3, 32, true)?;
    for plan in [&plain, &fused] {
        let mut b = bindings.clone();
        b.get_mut("x").unwrap().spec.shape[0] = 31;
        assert_eq!(plan.execute(&b), Err(Error::SchemaMismatch));
        let mut b = bindings.clone();
        b.remove("x");
        assert!(plan.execute(&b).is_err());
        let mut b = bindings.clone();
        b.insert("extra".into(), Tensor::vector(vec![1.0]));
        assert_eq!(plan.execute(&b), Err(Error::SchemaMismatch));
        let mut b = bindings.clone();
        b.get_mut("x").unwrap().storage = Storage::Q4Bytes(vec![0; 18]);
        assert_eq!(plan.execute(&b), Err(Error::SchemaMismatch));
        for len in [17, 19, 53, 55] {
            let mut b = bindings.clone();
            b.get_mut("matrix").unwrap().storage = Storage::Q4Bytes(vec![0; len]);
            assert_eq!(plan.execute(&b), Err(Error::SchemaMismatch));
        }
        let mut b = bindings.clone();
        b.get_mut("x").unwrap().storage = Storage::F32(vec![0.0; 31]);
        assert_eq!(plan.execute(&b), Err(Error::SchemaMismatch));
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut b = bindings.clone();
            if let Storage::F32(x) = &mut b.get_mut("x").unwrap().storage {
                x[31] = bad;
            }
            assert_eq!(plan.execute(&b), Err(Error::NonFinite));
        }
        for scale in [0x7c00u16, 0xfc00, 0x7c01, 0x7fff, 0xffff] {
            let mut b = bindings.clone();
            if let Storage::Q4Bytes(v) = &mut b.get_mut("matrix").unwrap().storage {
                v[..2].copy_from_slice(&scale.to_le_bytes());
            }
            assert_eq!(plan.execute(&b), Err(Error::NonFinite));
        }
    }
    // Unused malformed weights also reject an otherwise valid input-only plan.
    let limits = Limits::default();
    let schema = Schema::new(
        bindings.iter().map(|(n, t)| (n.clone(), t.spec.clone())),
        limits,
    )?;
    let mut g = Graph::new(schema, limits)?;
    let x = g.input("x")?;
    let plan = g.compile(x, true)?;
    let mut b = bindings;
    b.get_mut("matrix").unwrap().storage = Storage::Q4Bytes(vec![]);
    assert_eq!(plan.execute(&b), Err(Error::SchemaMismatch));
    Ok(())
}

#[test]
fn overflow_in_actual_fp32_arithmetic_is_a_result_error() -> Result<(), Error> {
    let (plain, fused, mut b) = fixture(1, 32, false)?;
    if let Storage::F32(x) = &mut b.get_mut("x").unwrap().storage {
        x[0] = f32::MAX;
    }
    assert_eq!(plain.execute(&b), Err(Error::NonFinite));
    assert_eq!(fused.execute(&b), Err(Error::NonFinite));
    Ok(())
}

#[test]
fn graph_schema_dtype_depth_work_and_storage_limits_are_enforced() -> Result<(), Error> {
    let limits = Limits::default();
    for spec in [
        TensorSpec::vector(0),
        TensorSpec::q4_matrix(1, 31),
        TensorSpec::q4_matrix(0, 32),
        TensorSpec {
            dtype: DType::F32,
            shape: vec![1, 32],
        },
        TensorSpec::q4_matrix(usize::MAX, 32),
    ] {
        assert!(Schema::new([("x".into(), spec)], limits).is_err());
    }
    assert!(Schema::new(
        [
            ("x".into(), TensorSpec::vector(1)),
            ("x".into(), TensorSpec::vector(1))
        ],
        limits
    )
    .is_err());
    let schema = Schema::new(
        [
            ("x".into(), TensorSpec::vector(32)),
            ("wrong".into(), TensorSpec::vector(31)),
            ("m".into(), TensorSpec::q4_matrix(1, 32)),
        ],
        limits,
    )?;
    let mut g = Graph::new(schema.clone(), limits)?;
    let x = g.input("x")?;
    assert!(g.input("m").is_err());
    assert!(g.q4_matvec(x, "wrong").is_err());
    assert!(g.rms_norm(x, "wrong", 1e-5).is_err());
    for epsilon in [0.0, -0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(g.rms_norm(x, "x", epsilon).is_err());
    }
    let wrong = g.input("wrong")?;
    assert!(g.residual(x, wrong).is_err());
    let mut other = Graph::new(schema.clone(), limits)?;
    let foreign = other.input("x")?;
    assert_eq!(g.residual(x, foreign), Err(Error::InvalidId));
    assert!(g.compile(foreign, true).is_err());
    let tiny = Limits {
        max_depth: 1,
        ..limits
    };
    let mut g = Graph::new(schema.clone(), tiny)?;
    let x = g.input("x")?;
    assert!(matches!(g.rms_norm(x, "x", 1e-5), Err(Error::Limit(_))));
    let tiny = Limits {
        max_work: 1,
        ..limits
    };
    let mut g = Graph::new(schema.clone(), tiny)?;
    let x = g.input("x")?;
    assert!(matches!(g.compile(x, true), Err(Error::Limit(_))));
    let tiny = Limits {
        max_values: 95,
        ..limits
    };
    let mut g = Graph::new(schema, tiny)?;
    let x = g.input("x")?;
    assert!(matches!(g.compile(x, true), Err(Error::Limit(_))));
    let tiny = Limits {
        max_nodes: 1,
        ..limits
    };
    let schema = Schema::new([("x".into(), TensorSpec::vector(1))], tiny)?;
    let mut g = Graph::new(schema, tiny)?;
    let x = g.input("x")?;
    assert!(matches!(g.rms_norm(x, "x", 1e-5), Err(Error::Limit(_))));
    Ok(())
}

#[test]
fn sharing_and_nested_fusion_preserve_the_actual_dag_values() -> Result<(), Error> {
    let limits = Limits::default();
    let bindings = Bindings::from([
        (
            "x".into(),
            Tensor::vector((0..32).map(|i| i as f32 * 0.0625 - 1.0).collect()),
        ),
        ("norm".into(), Tensor::vector(vec![0.5; 32])),
        (
            "matrix".into(),
            Tensor::q4_matrix(32, 32, bytes_for(32, 32)),
        ),
    ]);
    let schema = Schema::new(
        bindings.iter().map(|(n, t)| (n.clone(), t.spec.clone())),
        limits,
    )?;
    let mut g = Graph::new(schema, limits)?;
    let x = g.input("x")?;
    let norm = g.rms_norm(x, "norm", 1e-5)?;
    let projection = g.q4_matvec(norm, "matrix")?;
    // The normalized vector remains live through another consumer.
    let shared = g.residual(norm, projection)?;
    let plain = g.compile(shared, false)?;
    let fused = g.compile(shared, true)?;
    assert_eq!(
        bits(&plain.execute(&bindings)?),
        bits(&fused.execute(&bindings)?)
    );
    assert_eq!(plain.stats().values, fused.stats().values);
    let norm2 = g.rms_norm(shared, "norm", 1e-5)?;
    let out = g.q4_matvec(norm2, "matrix")?;
    let plain = g.compile(out, false)?;
    let fused = g.compile(out, true)?;
    assert_eq!(fused.stats().fused_nodes, 2);
    assert_eq!(
        bits(&plain.execute(&bindings)?),
        bits(&fused.execute(&bindings)?)
    );
    Ok(())
}

#[test]
fn scalar_accumulation_order_is_not_reassociated() -> Result<(), Error> {
    let limits = Limits::default();
    let mut x = vec![0.0; 32];
    // ((2^24 + 1) - 2^24) rounds to 0; reassociation could yield 1.
    x[0] = 16_777_216.0;
    x[1] = 1.0;
    x[2] = -16_777_216.0;
    let mut raw = Vec::from(0x3c00u16.to_le_bytes());
    raw.extend([0x99; 16]); // all weights +1
    let bindings = Bindings::from([
        ("x".into(), Tensor::vector(x)),
        ("m".into(), Tensor::q4_matrix(1, 32, raw)),
    ]);
    let schema = Schema::new(
        bindings.iter().map(|(n, t)| (n.clone(), t.spec.clone())),
        limits,
    )?;
    let mut g = Graph::new(schema, limits)?;
    let x = g.input("x")?;
    let out = g.q4_matvec(x, "m")?;
    assert_eq!(bits(&g.compile(out, true)?.execute(&bindings)?), vec![0]);
    Ok(())
}

#[test]
fn byte_budget_is_applied_to_inputs_and_materialized_outputs() -> Result<(), Error> {
    let limits = Limits {
        max_bytes: 8,
        ..Limits::default()
    };
    assert!(Schema::new([("x".into(), TensorSpec::vector(3))], limits).is_err());
    let schema = Schema::new([("x".into(), TensorSpec::vector(2))], limits)?;
    let mut g = Graph::new(schema, limits)?;
    let x = g.input("x")?;
    assert!(matches!(g.compile(x, false), Err(Error::Limit(_))));
    Ok(())
}
