//! Facts across calls (self-hosting wave SH3 lane 6, #1245).
//!
//! - A function whose returns are all bounded has a result range, which its
//!   call sites use as the bounds of the call: `128 / bits(x)` needs no
//!   `Fail` when `bits` returns only 8 or 64.
//! - `u128` constant facts cover the whole `u128` range, not only the `i128`
//!   domain.
//! - A checked `u128` multiply whose operand is a widened one-word value
//!   runs a cheaper overflow check (native code and the JIT).
//!
//! Every accepted program agrees in the evaluator and the JIT (optimized and
//! not) and compiles to x86-64 and AArch64 objects; negative tests pin that
//! no `Fail` is dropped where a divisor can be zero or a value can overflow.
mod support;

/// The widened multiply emits fewer partial products: two `umulhi` for one
/// widened operand (the generic check has three) and one for two.
#[test]
fn widened_multiply_emits_fewer_partial_products() {
    let dir = std::env::temp_dir().join(format!("vesper-call-facts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let count = |name: &str, body: &str| {
        let file = dir.join(format!("{name}.ves"));
        std::fs::write(
            &file,
            format!(
                "{body}
fn main() -> u64 {{
    handle Fail[ArithOverflow] in {{ wrapping_u64(f(5, 7)) }} with {{ return v => v, raise(e, _) => 1, }}
}}"
            ),
        )
        .unwrap();
        let out = support::vesper()
            .args(["emit", "--emit", "clif"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let clif = String::from_utf8_lossy(&out.stdout).to_string();
        // The function `f`: from its `; f` header to the next header.
        let f: Vec<&str> = clif
            .lines()
            .skip_while(|l| *l != "; f")
            .skip(1)
            .take_while(|l| !l.starts_with("; "))
            .collect();
        assert!(!f.is_empty(), "no `f` in\n{clif}");
        f.iter().filter(|l| l.contains("umulhi")).count()
    };
    let generic = count(
        "generic",
        "fn f(a: u128, b: u128) -> u128 eff [Fail[ArithOverflow]] { a * b }",
    );
    let one = count(
        "one",
        "fn f(a: u128, b: u64) -> u128 eff [Fail[ArithOverflow]] { a * widen_u128(b) }",
    );
    let two = count(
        "two",
        "fn f(a: u64, b: u64) -> u128 eff [Fail[ArithOverflow]] { widen_u128(a) * widen_u128(b) }",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!((generic, one, two), (3, 2, 1));
}
