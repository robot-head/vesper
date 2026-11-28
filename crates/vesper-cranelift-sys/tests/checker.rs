//! Stage 2 refuses a leaked builder handle (E0302) and a builder defined
//! twice (E0303), through `@vesper/cranelift`'s linear `.vsys` types
//! (#2701), as the Rust checker does.
mod support;

use support::stage2;

#[test]
fn the_checker_refuses_leaked_and_reused_handles() {
    let Some(stage2) = stage2::stage2() else {
        return;
    };
    for (name, body, code) in [
        (
            "leaked builder",
            "pub fn main() -> u64 eff [Ffi[Cranelift]] {
                match module_new(in \"\".as_bytes(), in \"\".as_bytes(), in \"\".as_bytes()) {
                    Some(m) => {
                        let mut m = m;
                        let s = sig_new(inout m, 0);
                        let b = builder_new(in m, s);
                        module_free(m);
                        0
                    },
                    None => 1,
                }
            }",
            "E0302",
        ),
        (
            "builder defined twice",
            "pub fn main() -> u64 eff [Ffi[Cranelift]] {
                match module_new(in \"\".as_bytes(), in \"\".as_bytes(), in \"\".as_bytes()) {
                    Some(m) => {
                        let mut m = m;
                        let s = sig_new(inout m, 0);
                        let f = declare_function(inout m, in \"f\".as_bytes(), 4, s);
                        match builder_new(in m, s) {
                            Some(b) => {
                                define_function(inout m, f, b);
                                define_function(inout m, f, b);
                            },
                            None => {},
                        }
                        module_free(m);
                        0
                    },
                    None => 1,
                }
            }",
            "E0303",
        ),
    ] {
        let e = match stage2.build(body) {
            Ok(_) => panic!("{name}: accepted"),
            Err(e) => e,
        };
        assert!(
            e.contains(&format!("{code}: linear value")),
            "{name}: want {code}, got:\n{e}"
        );
    }
}
