//! NDJSON batches and EventEnvelope column projection, scalar engine versus
//! the SIMD structural index on every available path.
mod common;

use common::{Rng, document, mutate};
use vesper_serde::event_columns::{EventColumns, ProjectionLimits, project_into};
use vesper_serde::ndjson::{BatchLimits, Engine, NdjsonBatch};
use vesper_serde::simd::Isa;

fn engines() -> Vec<Engine> {
    Isa::available().map(Engine::Simd).collect()
}

fn events(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        out.extend_from_slice(
            format!("{{\"id\":{i},\"topic\":\"t\\n{i}\",\"raw_meta\":").as_bytes(),
        );
        out.extend(
            document(rng)
                .into_iter()
                .filter(|b| *b != b'\n' && *b != b'\r'),
        );
        out.extend_from_slice(if i % 3 == 0 { b"}\r\n" } else { b"}\n" });
    }
    out
}

fn projected(
    batch: &NdjsonBatch<'_>,
) -> Result<Vec<(u64, String, String)>, vesper_serde::ndjson::Error> {
    let n = batch.len();
    let mut ids = vec![0u64; n];
    let mut topics = vec![None; n];
    let mut meta = vec![None; n];
    let mut cursor = batch.cursor();
    let count = project_into(
        &mut cursor,
        EventColumns {
            ids: &mut ids,
            topics: &mut topics,
            raw_meta: &mut meta,
        },
        ProjectionLimits::default(),
    )?;
    Ok((0..count)
        .map(|i| {
            (
                ids[i],
                topics[i].unwrap().raw_content().to_owned(),
                meta[i].unwrap().raw().to_owned(),
            )
        })
        .collect())
}

#[test]
fn simd_fed_batches_and_columns_equal_scalar() {
    let mut rng = Rng(0xc0ffee);
    for round in 0..300 {
        let n = 1 + rng.below(12);
        let mut input = events(&mut rng, n);
        if round % 2 == 1 {
            mutate(&mut rng, &mut input);
        }
        let scalar = NdjsonBatch::parse_with(&input, BatchLimits::default(), Engine::Scalar);
        for engine in engines() {
            let simd = NdjsonBatch::parse_with(&input, BatchLimits::default(), engine);
            match (&scalar, &simd) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a.stats(), b.stats(), "{engine:?}");
                    for i in 0..a.len() {
                        let (x, y) = (a.get(i).unwrap(), b.get(i).unwrap());
                        assert_eq!(x.root().raw(), y.root().raw());
                        assert_eq!(x.payload_span(), y.payload_span());
                    }
                    let (pa, pb) = (projected(a), projected(b));
                    assert_eq!(pa.is_ok(), pb.is_ok());
                    if let (Ok(pa), Ok(pb)) = (pa, pb) {
                        assert_eq!(pa, pb);
                    }
                }
                (Err(a), Err(b)) => assert_eq!(a, b, "{engine:?}"),
                _ => panic!(
                    "{engine:?} disagrees: {:?} vs {:?}",
                    scalar.is_ok(),
                    simd.is_ok()
                ),
            }
        }
    }
}

#[test]
fn default_parse_uses_the_detected_simd_path() {
    assert_eq!(Engine::detect(), Engine::Simd(Isa::detect()));
    let batch = NdjsonBatch::parse(
        b"{\"id\":1,\"topic\":\"a\",\"raw_meta\":{}}\n",
        BatchLimits::default(),
    )
    .unwrap();
    assert_eq!(
        projected(&batch).unwrap(),
        vec![(1, "a".into(), "{}".into())]
    );
    for isa in Isa::ALL {
        let result = NdjsonBatch::parse_with(b"1\n", BatchLimits::default(), Engine::Simd(isa));
        assert_eq!(result.is_ok(), isa.is_available());
    }
}
