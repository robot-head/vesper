//! A planned Box drop reads only the values it drops (#1426). The native
//! lowering used to re-read every local variable at an instruction or
//! terminator with a Box drop, which made each local live across the whole
//! graph: a block parameter at every join, spilled around every loop. A
//! scalar loop in a program that uses `Box` anywhere compiled 2-3x larger.
use object::{Object, ObjectSymbol};
use vesper::native::export::{LibraryOptions, compile_source};

const KERNEL: &str =
    "pub fn k(inout a: Slice[u32], in b: Slice[u32], n: u64) eff [Fail[RuntimeError]] {
    for i in 0..n {
        a.set(i, to_bits_f32(from_bits_f32(a[i]) * 2.0 + from_bits_f32(b[i])));
    }
}
fn main() -> u64 { 0 }
";

const BOXY: &str = "trait P { fn go(inout self) -> u64; }
struct S { x: u64 }
impl P for S { fn go(inout self) -> u64 { self.x } }
pub fn boxy(n: u64) -> u64 {
    let mut b = Box::new[dyn P](S { x: n });
    b.go()
}
";

fn kernel_size(source: &str) -> u64 {
    let library = compile_source(
        "k.ves",
        source,
        &LibraryOptions {
            library: "t",
            target: Some("x86_64-unknown-linux-gnu"),
            optimize: true,
        },
    )
    .unwrap();
    let file = object::File::parse(&*library.object).unwrap();
    file.symbols()
        .find(|s| s.name() == Ok("vesper_t_k"))
        .expect("vesper_t_k")
        .size()
}

#[test]
fn a_box_elsewhere_does_not_grow_a_scalar_loop() {
    let alone = kernel_size(KERNEL);
    let with_box = kernel_size(&format!("{KERNEL}{BOXY}"));
    assert_eq!(
        alone, with_box,
        "vesper_t_k: {alone} bytes alone, {with_box} with a Box"
    );
}
