//! The checker refuses a leaked builder handle (E0302) and a builder
//! defined twice (E0303), through `@vesper/cranelift`'s linear `.vsys`
//! types.
//!
//! This is the one test here still on the Rust compiler: Stage 2 accepts
//! both programs (it does not check linear foreign handles yet, #2701).
//! Move it onto `stage2::Stage2::build` once Stage 2 refuses them.
mod support;

use support::*;

#[test]
fn the_checker_refuses_leaked_and_reused_handles() {
    let target = host_target().unwrap_or(("x86_64-unknown-linux-gnu", "sysv64"));
    let package = package_source();
    let mut imports = vesper::ffi_lowering::Imports::new();
    imports
        .add(
            &vsys(&package, target),
            vesper::ffi_lowering::Handler::Direct,
        )
        .unwrap();
    for (name, body, code) in [
        (
            "leaked builder",
            "pub fn f() -> u64 eff [Ffi[Cranelift]] {
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
            "pub fn f() -> u64 eff [Ffi[Cranelift]] {
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
        let source = format!("{package}\n{body}");
        let e = match vesper::ffi_lowering::check_source(&source, &imports) {
            Ok(_) => panic!("{name}: accepted"),
            Err(e) => e,
        };
        let codes: Vec<&str> = e.diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert!(
            codes.contains(&code),
            "{name}: want {code}, got {e} {codes:?}"
        );
    }
}
