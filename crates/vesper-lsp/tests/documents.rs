use std::sync::Arc;
use vesper_lsp::*;
fn store() -> DocumentStore {
    DocumentStore::new(StoreLimits::default()).unwrap()
}
fn pos(line: u32, character: u32) -> Position {
    Position { line, character }
}
fn edit(start: Position, end: Position, length: Option<u32>, text: &str) -> ContentChange<'_> {
    ContentChange {
        range: Some(Range { start, end }),
        range_length: length,
        text,
    }
}
fn full(text: &str) -> ContentChange<'_> {
    ContentChange {
        range: None,
        range_length: None,
        text,
    }
}
#[test]
fn sequential_unicode_utf16_edits_crlf_and_owner_snapshots() {
    let mut s = store();
    s.open("untitled:opaque/path", -3, "a😀b\r\ncafé\rZ\n")
        .unwrap();
    let old = s.get("untitled:opaque/path").unwrap().text_arc();
    s.change(
        "untitled:opaque/path",
        0,
        &[
            edit(pos(0, 1), pos(0, 3), Some(2), "☃"),
            edit(pos(1, 3), pos(1, 4), Some(1), "é"),
            edit(pos(2, 0), pos(3, 0), Some(2), "end"),
        ],
    )
    .unwrap();
    let d = s.get("untitled:opaque/path").unwrap();
    assert_eq!(d.text(), "a☃b\r\ncafé\rend");
    assert_eq!(d.version(), 0);
    assert_eq!(d.revision(), 2);
    assert_eq!(&*old, "a😀b\r\ncafé\rZ\n");
    assert_eq!(d.index().text(), d.text());
    assert_eq!(s.total_bytes(), d.text().len());
    s.change(
        "untitled:opaque/path",
        1,
        &[full("😀\r\ny"), edit(pos(0, 2), pos(1, 0), Some(2), "!")],
    )
    .unwrap();
    assert_eq!(s.get("untitled:opaque/path").unwrap().text(), "😀!y");
}
#[test]
fn invalid_edit_batches_preserve_all_public_state_and_exact_arc() {
    let mut s = store();
    s.open("opaque:x", 10, "a😀b\r\nc").unwrap();
    let before = s.get("opaque:x").unwrap().text_arc();
    let revision = s.revision();
    let total = s.total_bytes();
    for changes in [
        vec![edit(pos(0, 2), pos(0, 3), None, "")],
        vec![edit(pos(0, 3), pos(0, 1), None, "")],
        vec![edit(pos(9, 0), pos(9, 0), None, "")],
        vec![edit(pos(0, 5), pos(1, 0), None, "")],
        vec![edit(pos(0, 1), pos(0, 3), Some(1), "")],
        vec![ContentChange {
            range: None,
            range_length: Some(0),
            text: "new",
        }],
        vec![
            full("replacement"),
            edit(pos(99, 0), pos(99, 0), None, "bad"),
        ],
    ] {
        assert!(s.change("opaque:x", 11, &changes).is_err());
        let d = s.get("opaque:x").unwrap();
        assert_eq!(d.text(), &*before);
        assert_eq!(d.version(), 10);
        assert_eq!(d.revision(), revision);
        assert!(Arc::ptr_eq(&before, &d.text_arc()));
        assert_eq!(s.revision(), revision);
        assert_eq!(s.total_bytes(), total);
        assert_eq!(d.index().offset(pos(0, 3)).unwrap(), 5);
    }
}
#[test]
fn open_close_versions_and_no_phantom_documents() {
    let mut s = store();
    assert!(s.change("unknown", 1, &[]).is_err());
    assert!(s.close("unknown").is_err());
    assert!(s.is_empty());
    assert_eq!(s.revision(), 0);
    s.open("file:///does-not-exist", i32::MIN, "x").unwrap();
    assert_eq!(
        s.open("file:///does-not-exist", 0, "different")
            .unwrap_err(),
        StoreError::AlreadyOpen
    );
    assert_eq!(s.get("file:///does-not-exist").unwrap().text(), "x");
    assert_eq!(
        s.change("file:///does-not-exist", i32::MIN, &[full("y")])
            .unwrap_err(),
        StoreError::Version
    );
    s.change("file:///does-not-exist", i32::MAX, &[]).unwrap();
    assert_eq!(
        s.change("file:///does-not-exist", i32::MAX, &[])
            .unwrap_err(),
        StoreError::Version
    );
    assert_eq!(s.get("file:///does-not-exist").unwrap().version(), i32::MAX);
    assert_eq!(s.revision(), 2);
    let d = s.close("file:///does-not-exist").unwrap();
    assert_eq!(d.text(), "x");
    assert_eq!(d.revision(), 2);
    assert_eq!(s.revision(), 3);
    assert_eq!(s.total_bytes(), 0);
    assert!(s.is_empty());
    assert!(s.close("file:///does-not-exist").is_err());
    assert_eq!(s.revision(), 3);
    s.open("file:///does-not-exist", 1, "again").unwrap();
    assert_eq!(s.iter().count(), 1);
}
#[test]
fn exact_size_count_uri_and_total_caps_are_transactional() {
    let l = StoreLimits {
        max_document_bytes: 4,
        max_documents: 2,
        max_total_bytes: 6,
        max_uri_bytes: 3,
        ..StoreLimits::default()
    };
    let mut s = DocumentStore::new(l).unwrap();
    s.open("one", 1, "😀").unwrap();
    s.open("two", 1, "ab").unwrap();
    assert_eq!(s.open("tri", 1, "").unwrap_err(), StoreError::DocumentLimit);
    assert_eq!(
        s.change("two", 2, &[full("abc")]).unwrap_err(),
        StoreError::TotalLimit
    );
    assert_eq!(
        s.change("one", 2, &[full("abcde")]).unwrap_err(),
        StoreError::DocumentLimit
    );
    assert_eq!(
        s.change("two", 2, &[full("abc"), full("")]).unwrap_err(),
        StoreError::TotalLimit
    );
    assert_eq!(s.total_bytes(), 6);
    assert_eq!(s.revision(), 2);
    assert_eq!(s.get("two").unwrap().version(), 1);
    for uri in ["", "long", "a\nb"] {
        assert_eq!(s.close(uri).unwrap_err(), StoreError::InvalidUri);
        assert!(s.get(uri).is_none());
    }
    s.close("one").unwrap();
    s.change("two", 2, &[full("abcd")]).unwrap();
    assert_eq!(s.total_bytes(), 4);
}
#[test]
fn change_work_and_index_budgets_checked_without_mutation() {
    let mut s = DocumentStore::new(StoreLimits {
        max_changes: 1,
        ..StoreLimits::default()
    })
    .unwrap();
    s.open("x", 1, "text").unwrap();
    assert_eq!(
        s.change("x", 2, &[full("a"), full("b")]).unwrap_err(),
        StoreError::ChangeLimit
    );
    assert_eq!(s.get("x").unwrap().version(), 1);
    let mut s = DocumentStore::new(StoreLimits {
        max_work: 12,
        max_uri_bytes: 1,
        ..StoreLimits::default()
    })
    .unwrap();
    s.open("x", 1, "abc").unwrap();
    assert_eq!(
        s.change("x", 2, &[full("abc")]).unwrap_err(),
        StoreError::WorkLimit
    );
    assert_eq!(s.get("x").unwrap().text(), "abc");
    assert_eq!(s.revision(), 1);
    let mut l = StoreLimits::default();
    l.index_limits.max_lines = 1;
    let mut s = DocumentStore::new(l).unwrap();
    s.open("x", 1, "abc").unwrap();
    assert!(matches!(
        s.change("x", 2, &[full("\n")]),
        Err(StoreError::Index(_))
    ));
    assert_eq!(s.get("x").unwrap().version(), 1);
    assert!(s.open("y", 1, "\n").is_err());
    assert_eq!(s.len(), 1);
    let mut l = StoreLimits::default();
    l.index_limits.max_metadata_bytes = 0;
    let mut s = DocumentStore::new(l).unwrap();
    assert!(s.open("x", 1, "").is_err());
    assert!(s.is_empty());
}
#[test]
fn invalid_limits_and_empty_budget_edges() {
    for l in [
        StoreLimits {
            max_document_bytes: usize::MAX,
            ..StoreLimits::default()
        },
        StoreLimits {
            max_documents: 257,
            ..StoreLimits::default()
        },
        StoreLimits {
            max_changes: usize::MAX,
            ..StoreLimits::default()
        },
        StoreLimits {
            index_limits: vesper_span::Limits {
                max_source_bytes: 1,
                ..vesper_span::Limits::default()
            },
            ..StoreLimits::default()
        },
    ] {
        assert_eq!(
            DocumentStore::new(l).unwrap_err(),
            StoreError::InvalidLimits
        );
    }
    let mut s = DocumentStore::new(StoreLimits {
        max_document_bytes: 0,
        max_total_bytes: 0,
        ..StoreLimits::default()
    })
    .unwrap();
    s.open("x", 1, "").unwrap();
    assert_eq!(
        s.change("x", 2, &[full("a")]).unwrap_err(),
        StoreError::DocumentLimit
    );
    s.change("x", 2, &[]).unwrap();
    assert_eq!(s.get("x").unwrap().version(), 2);
    let mut s = DocumentStore::new(StoreLimits {
        max_documents: 0,
        ..StoreLimits::default()
    })
    .unwrap();
    assert_eq!(s.open("x", 1, "").unwrap_err(), StoreError::DocumentLimit);
}
