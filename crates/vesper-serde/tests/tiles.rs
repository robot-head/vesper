//! §20.5 NDJSON-to-AoSoA ingestion fed by the SIMD index.
use vesper_serde::simd::Isa;
use vesper_serde::tiles::{
    InlineStr32, TILE_WIDTH, TelemetryField, TelemetryIngestor, TelemetryPoint, TelemetryTiles,
    TileErrorKind,
};
use vesper_serde::{ErrorKind, Limits};

fn line(i: usize) -> String {
    format!(
        "{{\"ts_unix_ms\":{},\"latency_us\":{},\"extra\":{{\"nested\":[1,2,{{\"x\":null}}]}},\"status_code\":{},\"flags\":{},\"trace_id_hex\":\"{:032x}\"}}\n",
        1_700_000_000_000i64 + i as i64,
        i * 7,
        200 + (i % 300),
        i % 65536,
        i as u128 * 0x1234_5678_9abc_def1
    )
}

fn expected(i: usize) -> TelemetryPoint {
    TelemetryPoint {
        ts_unix_ms: 1_700_000_000_000 + i as i64,
        latency_us: (i * 7) as u32,
        status_code: (200 + i % 300) as u16,
        flags: (i % 65536) as u16,
        trace_id_hex: InlineStr32::new(&format!("{:032x}", i as u128 * 0x1234_5678_9abc_def1))
            .unwrap(),
    }
}

#[test]
fn ingests_into_hot_and_cold_tiles_on_every_path() {
    let input: String = (0..100).map(line).collect();
    for isa in Isa::available() {
        let mut ingestor = TelemetryIngestor::with_isa(isa, Limits::default()).unwrap();
        let mut tiles = TelemetryTiles::with_capacity(100).unwrap();
        assert_eq!(tiles.capacity(), 112);
        assert_eq!(ingestor.ingest(input.as_bytes(), &mut tiles).unwrap(), 100);
        assert_eq!(tiles.len(), 100);
        assert_eq!(tiles.hot_tiles().len(), 100usize.div_ceil(TILE_WIDTH));
        for i in 0..100 {
            assert_eq!(tiles.get(i).unwrap(), expected(i), "{isa:?} point {i}");
        }
        let tile = &tiles.hot_tiles()[1];
        assert_eq!(tile.ts_unix_ms[3], 1_700_000_000_000 + 19);
        assert_eq!(
            tiles.cold_tiles()[6].trace_id_hex[3],
            expected(99).trace_id_hex
        );
        // Unused lanes of the final tile stay zero.
        assert_eq!(tiles.hot_tiles()[6].ts_unix_ms[4], 0);
        assert_eq!(core::mem::align_of_val(tile), 64);
        assert_eq!(core::mem::size_of_val(tile), 256);
    }
}

#[test]
fn escaped_keys_crlf_and_missing_final_newline() {
    let input = b"{\"ts_unix_ms\":-5,\"latency_us\":0,\"status_code\":0,\"flags\":65535,\"trace_id_hex\":\"\\u0061b\"}\r\n{\"\\u0074s_unix_ms\":1,\"latency_us\":4294967295,\"status_code\":65535,\"flags\":1,\"trace_id_hex\":\"\"}";
    let mut ingestor = TelemetryIngestor::new(Limits::default());
    let mut tiles = TelemetryTiles::with_capacity(4).unwrap();
    assert_eq!(ingestor.ingest(input, &mut tiles).unwrap(), 2);
    assert_eq!(tiles.get(0).unwrap().trace_id_hex.as_str(), "ab");
    assert_eq!(tiles.get(0).unwrap().ts_unix_ms, -5);
    assert_eq!(tiles.get(1).unwrap().latency_us, u32::MAX);
    assert_eq!(tiles.get(1).unwrap().ts_unix_ms, 1);
}

#[test]
fn every_failure_is_typed_and_transactional() {
    let good = line(0);
    let cases: Vec<(String, TileErrorKind)> = vec![
        (
            good.replace("\"flags\":0", "\"flags\":65536"),
            TileErrorKind::FieldRange(TelemetryField::Flags),
        ),
        (
            good.replace("\"latency_us\":0", "\"latency_us\":-1"),
            TileErrorKind::FieldRange(TelemetryField::LatencyUs),
        ),
        (
            good.replace("\"status_code\":200", "\"status_code\":2.5"),
            TileErrorKind::FieldRange(TelemetryField::StatusCode),
        ),
        (
            good.replace("\"status_code\":200", "\"status_code\":\"200\""),
            TileErrorKind::FieldType(TelemetryField::StatusCode),
        ),
        (
            good.replace(",\"flags\":0", ""),
            TileErrorKind::MissingField(TelemetryField::Flags),
        ),
        (
            good.replace("\"trace_id_hex\":\"", "\"trace_id_hex\":\"ff"),
            TileErrorKind::FieldRange(TelemetryField::TraceIdHex),
        ),
        (
            good.replace("\"trace_id_hex\":\"", "\"trace_id_hex\":7,\"x\":\""),
            TileErrorKind::FieldType(TelemetryField::TraceIdHex),
        ),
        (
            good.replace("\"flags\"", "\"latency_us\""),
            TileErrorKind::Json(ErrorKind::DuplicateKey),
        ),
        (
            good.replace("null", "nul"),
            TileErrorKind::Json(ErrorKind::Syntax),
        ),
        ("[1]\n".into(), TileErrorKind::NotObject),
        ("\n".into(), TileErrorKind::BlankLine),
    ];
    for (bad, kind) in cases {
        let input = [line(1), line(2), bad.clone(), line(3)].concat();
        let mut ingestor = TelemetryIngestor::new(Limits::default());
        let mut tiles = TelemetryTiles::with_capacity(32).unwrap();
        ingestor.ingest(line(9).as_bytes(), &mut tiles).unwrap();
        let err = ingestor
            .ingest(input.as_bytes(), &mut tiles)
            .expect_err(&bad);
        assert_eq!(err.kind, kind, "{bad}");
        assert_eq!(err.line, 3);
        assert!(err.offset >= line(1).len() + line(2).len());
        // Earlier points from this call were rolled back; earlier calls kept.
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles.get(0).unwrap(), expected(9));
        assert_eq!(tiles.hot_tiles().len(), 1);
        assert_eq!(tiles.hot_tiles()[0].ts_unix_ms[1], 0);
    }
    let mut ingestor = TelemetryIngestor::new(Limits::default());
    let mut tiles = TelemetryTiles::with_capacity(2).unwrap();
    let three: String = (0..TILE_WIDTH + 1).map(line).collect();
    let err = ingestor.ingest(three.as_bytes(), &mut tiles).unwrap_err();
    assert_eq!(err.kind, TileErrorKind::Full);
    assert_eq!(err.line, TILE_WIDTH + 1);
    assert!(tiles.is_empty());
    let err = ingestor
        .ingest(b"{\"a\":\"\xff\"}", &mut tiles)
        .unwrap_err();
    assert_eq!(
        (err.kind, err.offset),
        (TileErrorKind::Json(ErrorKind::Utf8), 6)
    );
}
