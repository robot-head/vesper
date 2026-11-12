//! Self-checking actual CPU execution; exits nonzero on any mismatch.
use vesper_tensor::{host::*, Error};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = Limits::default();
    let mut bytes = Vec::new();
    for scale in [0x3c00u16, 0xb800, 1] {
        bytes.extend(scale.to_le_bytes());
        bytes.extend((0..16).map(|i| (i | ((15 - i) << 4)) as u8));
    }
    let bindings = Bindings::from([
        (
            "hidden".into(),
            Tensor::vector((0..32).map(|i| (i as f32 - 16.0) / 32.0).collect()),
        ),
        ("norm_weight".into(), Tensor::vector(vec![1.0; 32])),
        ("projection".into(), Tensor::q4_matrix(3, 32, bytes)),
        ("residual".into(), Tensor::vector(vec![1.0, -2.0, 0.25])),
    ]);
    let schema = Schema::new(
        bindings
            .iter()
            .map(|(name, t)| (name.clone(), t.spec.clone())),
        limits,
    )?;
    let mut graph = Graph::new(schema, limits)?;
    let x = graph.input("hidden")?;
    let norm = graph.rms_norm(x, "norm_weight", 1e-5)?;
    let projected = graph.q4_matvec(norm, "projection")?;
    let r = graph.input("residual")?;
    let output = graph.residual(projected, r)?;
    let plain = graph.compile(output, false)?;
    let fused = graph.compile(output, true)?;
    let a = plain.execute(&bindings)?;
    let b = fused.execute(&bindings)?;
    if a.iter()
        .map(|x| x.to_bits())
        .ne(b.iter().map(|x| x.to_bits()))
    {
        return Err(Error::Shape("fusion changed output bits").into());
    }
    if fused.stats().fused_nodes != 1 || plain.stats().values - fused.stats().values != 32 {
        return Err(Error::Shape("fusion did not eliminate intermediate").into());
    }
    println!(
        "FP32/Q4_0 fused execution matched all {} output bit patterns: {:?}",
        a.len(),
        a
    );
    println!("unfused={:?}\nfused={:?}", plain.stats(), fused.stats());
    Ok(())
}
