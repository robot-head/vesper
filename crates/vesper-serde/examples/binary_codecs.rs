//! Run: cargo run -p vesper-serde --example binary_codecs
use vesper_serde::archive::{self, Archive, TreeNode};
use vesper_serde::streamvbyte;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let values = [1, 256, 65_536, 16_777_216, 42];
    let expected = [0xe4, 0, 1, 0, 1, 0, 0, 1, 0, 0, 0, 1, 42];
    let mut packed = [0; 32];
    let packed_len =
        streamvbyte::encode_into(&values, &mut packed, streamvbyte::Limits::default())?;
    assert_eq!(&packed[..packed_len], expected);
    let mut decoded = [0; 5];
    streamvbyte::decode_into(
        &packed[..packed_len],
        5,
        &mut decoded,
        streamvbyte::Limits::default(),
    )?;
    assert_eq!(decoded, values);
    let mut unchanged = [0xab; 3];
    assert!(
        streamvbyte::encode_into(&values, &mut unchanged, streamvbyte::Limits::default()).is_err()
    );
    assert_eq!(unchanged, [0xab; 3]);

    let nodes = [
        TreeNode {
            value: 42,
            label: "build",
            children: &[1],
        },
        TreeNode {
            value: 7,
            label: "complete 🦀",
            children: &[],
        },
    ];
    let mut bytes = [0; 256];
    let len = archive::encode_into(&nodes, &mut bytes, archive::Limits::default())?;
    let archive = Archive::validate(&bytes[..len], archive::Limits::default())?;
    assert_eq!(archive.root().label(), "build");
    let child = archive.root().children().next().unwrap();
    assert_eq!(child.value(), 7);
    assert_eq!(child.label(), "complete 🦀");
    assert!(Archive::validate(&bytes[..len - 1], archive::Limits::default()).is_err());
    println!(
        "StreamVByte: {} values in {} bytes; scalar reference",
        values.len(),
        packed_len
    );
    println!(
        "Archive v1/tree1: {} nodes in {} bytes; validated borrowed views",
        archive.len(),
        len
    );
    Ok(())
}
