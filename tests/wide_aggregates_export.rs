//! 128-bit values in export cells (self-hosting wave SH4 lane 3, #1277).
//!
//! `i128` and `u128` cross the export boundary as a two-word cell, low word
//! first: inside an aggregate, an `Option` or a v3 mirror as their two
//! words, and as a parameter or result through a pointer to those words,
//! never in registers (the C ABIs of the export targets disagree on 128-bit
//! integers). A C caller drives the object below; the bindings are checked
//! for the cells they use.
use vesper::native::export::{
    AbiType, BindingOptions, LibraryInterface, LibraryOptions, compile_source, rust_binding,
};

const LIBRARY: &str = "copy struct Cell { tag: u8, v: i128, w: u128 }
pub fn add(a: u128, b: u128) -> u128 { wrapping_add(a, b) }
pub fn neg(a: i128) -> i128 { wrapping_sub(0, a) }
pub fn mix(k: u64, a: i128, b: u64) -> u64 { wrapping_add(wrapping_add(k, wrapping_u64(a)), wrapping_mul(b, 2)) }
pub fn split(a: u128, k: u64) -> (u64, u64, u128) { (wrapping_u64(a), wrapping_u64(a >> 64), wrapping_add(a, widen_u128(k))) }
pub fn bump(inout a: u128) { a = wrapping_add(a, 1); }
pub fn positive(a: i128) -> Option[i128] { if a < 0 { Option::None } else { Option::Some(a) } }
pub fn cell(tag: u8, v: i128) -> Cell { Cell { tag: tag, v: v, w: wrapping_u128(v) } }
pub fn swap(in c: Cell) -> Cell { Cell { tag: c.tag, v: wrapping_i128(c.w), w: wrapping_u128(c.v) } }
pub fn ratio(a: u128, s: f64) -> f64 { if a > 4 { s } else { 0.5 } }
pub fn quot(a: u128, b: u128) -> u128 eff [Fail[RuntimeError]] { if b == 0 { Fail::raise(RuntimeError::DivisionByZero) } a / b }
";

const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

fn options(library: &'static str, target: Option<&'static str>) -> LibraryOptions<'static> {
    LibraryOptions {
        library,
        target,
        optimize: true,
    }
}

fn interface(source: &str, library: &'static str) -> LibraryInterface {
    for target in TARGETS {
        for optimize in [true, false] {
            let lib = compile_source(
                "wide.ves",
                source,
                &LibraryOptions {
                    optimize,
                    ..options(library, Some(target))
                },
            )
            .unwrap_or_else(|e| panic!("{target} (optimize {optimize}): {e}"));
            assert!(!lib.object.is_empty());
        }
    }
    compile_source("wide.ves", source, &options(library, None))
        .unwrap()
        .interface
}

#[test]
fn wide_values_cross_through_pointers() {
    let i = interface(LIBRARY, "wide");
    let f = |name: &str| i.functions.iter().find(|f| f.name == name).unwrap();
    let wide = |name: &str| AbiType::Scalar { name: name.into() };
    let add = f("add");
    assert!(add.result_pointer && add.result_words == 2, "{add:?}");
    assert!(add.params.iter().all(|p| p.pointer && p.ty == wide("u128")));
    let mix = f("mix");
    assert_eq!(
        mix.params.iter().map(|p| p.pointer).collect::<Vec<_>>(),
        [false, true, false]
    );
    assert!(!mix.result_pointer);
    assert_eq!(f("split").result_words, 4);
    assert_eq!(f("positive").result_words, 3);
    assert_eq!(f("cell").result_words, 5);
    let binding = rust_binding(&i, &BindingOptions::default());
    assert!(
        binding.contains("pub fn add(a: u128, b: u128) -> u128"),
        "{binding}"
    );
    assert!(binding.contains("<u128 as Abi>::WORDS"), "{binding}");
}

#[test]
fn v3_mirrors_hold_wide_fields_as_two_words() {
    let i = interface(&format!("// @export abi 3\n{LIBRARY}"), "widemirror");
    let binding = rust_binding(&i, &BindingOptions::default());
    assert!(
        binding.contains("pub v: vesper_port_abi::Words<i128, 2>,")
            && binding.contains("pub w: vesper_port_abi::Words<u128, 2>,"),
        "{binding}"
    );
}

/// A C caller passes and reads the two-word cells.
#[cfg(unix)]
#[test]
fn a_c_caller_reads_the_cells() {
    use std::process::Command;
    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let library = compile_source("wide.ves", LIBRARY, &options("wide", None)).unwrap();
    let s = |name: &str| {
        library
            .interface
            .functions
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.symbol.clone())
            .unwrap()
    };
    let dir = std::env::temp_dir().join(format!("vesper-wide-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lib.o"), &library.object).unwrap();
    let main = format!(
        r#"#include <stdio.h>
#include <stdint.h>
typedef uint64_t w;
extern int64_t {add}(w *r, w *a, w *b);
extern int64_t {neg}(w *r, w *a);
extern int64_t {mix}(int64_t k, w *a, int64_t b);
extern int64_t {split}(w *r, w *a, int64_t k);
extern int64_t {bump}(w *a);
extern int64_t {positive}(w *r, w *a);
extern int64_t {cell}(w *r, int64_t tag, w *v);
extern int64_t {swap}(w *r, w *c);
extern int64_t {quot}(w *r, w *a, w *b, w *fail);
extern double {ratio}(w *a, double s);
static void show(const char *name, w *v, int n) {{
  printf("%s", name);
  for (int i = 0; i < n; i++) printf(" %llx", (unsigned long long)v[i]);
  printf("\n");
}}
int main(void) {{
  w r[5] = {{0}};
  w a[2] = {{0xffffffffffffffffull, 1}}, b[2] = {{1, 2}};
  {add}(r, a, b); show("add", r, 2);
  {neg}(r, b); show("neg", r, 2);
  printf("mix %llx\n", (unsigned long long){mix}(10, b, 3));
  {split}(r, a, 1); show("split", r, 4);
  {bump}(a); show("bump", a, 2);
  w m[2] = {{5, 0xffffffffffffffffull}};
  {positive}(r, m); printf("positive %llx\n", (unsigned long long)r[0]);
  {positive}(r, b); show("positive", r, 3);
  {cell}(r, 7, m); show("cell", r, 5);
  w c[5] = {{9, 1, 2, 3, 4}};
  {swap}(r, c); show("swap", r, 5);
  w x[2] = {{0, 6}}, y[2] = {{3, 0}}, z[2] = {{0, 0}}, fail[2] = {{0, 0}};
  {quot}(r, x, y, fail); show("quot", r, 2);
  {quot}(r, x, z, fail); printf("quot fail %llu\n", (unsigned long long)fail[0]);
  printf("ratio %g %g\n", {ratio}(x, 2.5), {ratio}(y, 2.5));
  return 0;
}}
"#,
        add = s("add"),
        neg = s("neg"),
        mix = s("mix"),
        split = s("split"),
        bump = s("bump"),
        positive = s("positive"),
        cell = s("cell"),
        swap = s("swap"),
        quot = s("quot"),
        ratio = s("ratio"),
    );
    std::fs::write(dir.join("main.c"), main).unwrap();
    let built = Command::new("cc")
        .arg(dir.join("main.c"))
        .arg(dir.join("lib.o"))
        .arg("-o")
        .arg(dir.join("main"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(dir.join("main")).output().unwrap();
    assert!(run.status.success());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "add 0 4\n\
         neg ffffffffffffffff fffffffffffffffd\n\
         mix 11\n\
         split ffffffffffffffff 1 0 2\n\
         bump 0 2\n\
         positive 0\n\
         positive 1 1 2\n\
         cell 7 5 ffffffffffffffff 5 ffffffffffffffff\n\
         swap 9 3 4 1 2\n\
         quot 0 2\n\
         quot fail 1\n\
         ratio 2.5 0.5\n"
    );
}
