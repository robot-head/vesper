use serde_json::json;
use std::io::Cursor;
use vesper_lsp::*;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = ProtocolLimits::default();
    let request = Envelope::Request {
        id: Id::Integer(7),
        method: "example/echo".into(),
        params: Some(json!({"text":"😀"})),
    };
    let wire = encode_frame(&request, limits)?;
    let mut input = Cursor::new(wire);
    assert_eq!(read_message(&mut input, limits)?.unwrap(), request);
    assert!(read_message(&mut input, limits)?.is_none());
    let mut store = DocumentStore::new(StoreLimits::default())?;
    store.open("untitled:example", 1, "a😀b\r\n")?;
    let old = store.get("untitled:example").unwrap().text_arc();
    let change = ContentChange {
        range: Some(Range {
            start: Position {
                line: 0,
                character: 1,
            },
            end: Position {
                line: 0,
                character: 3,
            },
        }),
        range_length: Some(2),
        text: "☃",
    };
    store.change("untitled:example", 2, &[change])?;
    assert_eq!(store.get("untitled:example").unwrap().text(), "a☃b\r\n");
    assert_eq!(&*old, "a😀b\r\n");
    assert!(store.change("untitled:example", 2, &[]).is_err());
    assert_eq!(store.get("untitled:example").unwrap().version(), 2);
    let doc = store.close("untitled:example")?;
    assert_eq!(doc.version(), 2);
    assert!(store.is_empty());
    println!("bounded framing and transactional UTF-16 documents: ok");
    Ok(())
}
