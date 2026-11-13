//! Validated NDJSON to caller-owned columns, then actual scalar JSON records.
use std::io::{Read, Write};
use vesper_serde::event_columns::{EventColumns, ProjectionLimits, project_into};
use vesper_serde::ndjson::{BatchLimits, NdjsonBatch};
use vesper_serde::{
    EncodeLimits, IndexedJson, JsonString, Limits, Node, Value, encode_into, encoded_size,
};

const MAX_INPUT: usize = 64 * 1024;
const MAX_ROWS: usize = 128;
const INPUT: &[u8] = concat!(
    "{\"id\":18446744073709551615,\"topic\":\"é🦀\",\"raw_meta\":{\"unknown\":[true,null]}}\r\n",
    "{\"\\u0069d\":9007199254740993,\"to\\u0070ic\":\"line\\n\\uD83E\\uDD80\",\"raw_meta\":[1,2,3]}\n",
    "{\"id\":0,\"topic\":\"empty metadata\",\"raw_meta\":null}"
).as_bytes();

fn row_size(
    id: u64,
    topic: JsonString<'_>,
    meta: Node<'_, '_>,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut scratch = [0; 4096];
    let topic = topic.decode(&mut scratch)?;
    let fields = [
        ("id", Value::U64(id)),
        ("topic", Value::String(topic.as_str())),
        ("raw_meta", Value::Raw(meta)),
    ];
    Ok(encoded_size(
        &Value::Object(&fields),
        EncodeLimits::default(),
    )?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let input = if args.is_empty() {
        INPUT.to_vec()
    } else if args.len() == 1 && args[0] == "--stdin" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(MAX_INPUT as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_INPUT {
            return Err("example input exceeds 64 KiB".into());
        }
        bytes
    } else {
        return Err("Usage: ndjson_columns [--stdin]".into());
    };
    let batch = NdjsonBatch::parse(
        &input,
        BatchLimits {
            max_input_bytes: MAX_INPUT,
            max_records: MAX_ROWS,
            ..BatchLimits::default()
        },
    )?;
    let mut cursor = batch.cursor();
    let mut ids = [0xada5; MAX_ROWS];
    let mut topics = [None; MAX_ROWS];
    let mut metadata = [None; MAX_ROWS];
    let count = project_into(
        &mut cursor,
        EventColumns {
            ids: &mut ids,
            topics: &mut topics,
            raw_meta: &mut metadata,
        },
        ProjectionLimits {
            max_rows: MAX_ROWS,
            ..ProjectionLimits::default()
        },
    )?;
    assert_eq!(count, batch.len());
    assert_eq!(cursor.remaining(), 0);
    assert!(ids[count..].iter().all(|id| *id == 0xada5));
    assert!(topics[count..].iter().all(Option::is_none));
    assert!(metadata[count..].iter().all(Option::is_none));
    let mut typed = batch.cursor();
    for i in 0..count {
        let expected = typed.read_event(ProjectionLimits::default())?;
        assert_eq!(expected.id, ids[i]);
        assert_eq!(
            expected.topic.raw_content(),
            topics[i].unwrap().raw_content()
        );
        assert_eq!(expected.raw_meta.raw(), metadata[i].unwrap().raw());
        if row_size(ids[i], topics[i].unwrap(), metadata[i].unwrap())? > 8192 {
            return Err("example encoded row exceeds 8192 bytes".into());
        }
    }
    let mut output = std::io::stdout().lock();
    for i in 0..count {
        let mut scratch = [0; 4096];
        let topic = topics[i].unwrap().decode(&mut scratch)?;
        let fields = [
            ("id", Value::U64(ids[i])),
            ("topic", Value::String(topic.as_str())),
            ("raw_meta", Value::Raw(metadata[i].unwrap())),
        ];
        let mut bytes = [0xa5; 8192];
        let length = encode_into(&Value::Object(&fields), &mut bytes, EncodeLimits::default())?;
        assert!(bytes[length..].iter().all(|byte| *byte == 0xa5));
        let checked = IndexedJson::parse(&bytes[..length], Limits::default())?;
        assert_eq!(checked.root().field("id", 4096)?.unwrap().as_u64()?, ids[i]);
        assert!(
            checked
                .root()
                .field("topic", 4096)?
                .unwrap()
                .string()?
                .equals(topic.as_str())
        );
        assert_eq!(
            checked.root().field("raw_meta", 4096)?.unwrap().raw(),
            metadata[i].unwrap().raw()
        );
        output.write_all(&bytes[..length])?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    Ok(())
}
