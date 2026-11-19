//! Fixed-size array fields in export ABI v3 mirrors (#1091, part 2). The
//! pilot's `arrays` library (crates/vesper-port-pilot) runs every form end
//! to end; these tests pin the interface, the cells, and that the committed
//! binding is what the compiler writes.
use vesper::native::export::{
    self, AbiType, BindingOptions, LibraryOptions, Mirror, compile_source, rust_binding,
};

const ARRAYS: &str = include_str!("../crates/vesper-port-pilot/vesper/arrays.ves");

fn options(library: &'static str) -> LibraryOptions<'static> {
    LibraryOptions {
        library,
        target: None,
        optimize: true,
    }
}

fn library() -> export::LibraryInterface {
    compile_source("arrays.ves", ARRAYS, &options("arrays"))
        .unwrap()
        .interface
}

fn binding(interface: &export::LibraryInterface, path: Option<&str>) -> String {
    rust_binding(
        interface,
        &BindingOptions {
            types_path: path.map(Into::into),
        },
    )
}

#[test]
fn array_field_structs_are_repr_c_mirrors() {
    let i = library();
    assert!(i.is_v3());
    for ty in ["Report", "Board", "Block"] {
        assert_eq!(i.types[ty].mirror, Some(Mirror::ReprC), "{ty}");
    }
    assert_eq!(i.types["Report"].words, 51);
    assert_eq!(i.types["Board"].words, 33);
    let f = |name: &str| i.functions.iter().find(|f| f.name == name).unwrap();
    assert!(f("report_bump").params[0].in_place);
    assert!(f("board_add").params[0].in_place);
    assert!(f("report_new").result_in_place);
    let xs = &f("words_total").params[0].ty;
    assert!(matches!(xs, AbiType::Tuple { items } if items.len() == 16));
}

#[test]
fn committed_binding_is_what_the_compiler_writes() {
    let text = binding(&library(), None);
    let committed = include_str!("../crates/vesper-port-pilot/port/arrays.rs");
    assert_eq!(text, committed.replace("\r\n", "\n"));
}

#[test]
fn array_fields_are_element_cells_and_rust_arrays() {
    let text = binding(&library(), None);
    for line in [
        // Rust values: arrays of more than eight elements are Rust arrays.
        "pub struct Report { pub id: u64, pub hash: [u8; 32], pub words: [u64; 16], pub pair: (u8, u8) }",
        "pub struct Board { pub len: u64, pub blocks: [Block; 16] }",
        // Mirror cells: narrow elements are `Word`s, plain words stay
        // words, mirrored elements are mirrors, short arrays stay tuples.
        "pub hash: [vesper_port_abi::Word<u8>; 32],",
        "pub words: [u64; 16],",
        "pub blocks: [Block; 16],",
        "pub pair: vesper_port_abi::Words<(u8, u8), 2>,",
        "assert!(core::mem::offset_of!(repr::Report, words) == 33 * 8);",
        "assert!(core::mem::offset_of!(repr::Report, pair) == 49 * 8);",
        // Whole arrays as parameters and results.
        "pub fn words_total(xs: [u64; 16]) -> u64 {",
        "pub fn words_reversed(xs: [u64; 16]) -> [u64; 16] {",
        "pub fn report_set(r: &mut repr::Report, at: u64, v: u64) -> Result<(), vesper_port_abi::RuntimeError> {",
    ] {
        assert!(text.contains(line), "missing {line}\n{text}");
    }
}

#[test]
fn rust_types_path_and_nested_arrays() {
    let source = "// @export abi 3
copy struct Grid {
    rows: [[u8; 4]; 12],
    tags: [i32; 9]
}
pub fn grid_sum(in g: Grid) -> i64 {
    let mut s: i64 = 0;
    for r in 0..12 {
        for c in 0..4 {
            s = wrapping_add(s, widen_i64(g.rows[r][c]));
        }
    }
    for t in 0..9 {
        s = wrapping_add(s, widen_i64(g.tags[t]));
    }
    s
}
";
    let lib = compile_source("grid.ves", source, &options("grid")).unwrap();
    assert_eq!(lib.interface.types["Grid"].mirror, Some(Mirror::ReprC));
    let text = binding(&lib.interface, Some("crate::grid"));
    for line in [
        "pub rows: [vesper_port_abi::Words<(u8, u8, u8, u8), 4>; 12],",
        "pub tags: [vesper_port_abi::Word<i32>; 9],",
        "assert!(core::mem::size_of::<repr::Grid>() == 57 * 8);",
        "impl Abi for crate::grid::Grid {",
    ] {
        assert!(text.contains(line), "missing {line}\n{text}");
    }
}

#[test]
fn eight_element_tuples_keep_their_binding() {
    // Eight is the longest Rust tuple with an `Abi` impl: a homogeneous
    // 8-tuple (or `[u64; 8]`) is still a tuple, as before arrays.
    let source = "// @export abi 3
copy struct Eight {
    xs: [u64; 8]
}
pub fn eight_first(in e: Eight) -> u64 {
    e.xs[0]
}
";
    let lib = compile_source("eight.ves", source, &options("eight")).unwrap();
    let text = binding(&lib.interface, None);
    assert!(
        text.contains("pub xs: (u64, u64, u64, u64, u64, u64, u64, u64)"),
        "{text}"
    );
    assert!(
        text.contains(
            "pub xs: vesper_port_abi::Words<(u64, u64, u64, u64, u64, u64, u64, u64), 8>,"
        ),
        "{text}"
    );
}
