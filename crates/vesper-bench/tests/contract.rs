use vesper_bench::contract::{BenchContract, Decimal, ErrorKind, parse_attribute};

#[test]
fn documented_contracts_preserve_exact_values() {
    let contract = parse_attribute("@bench(\n zero_alloc = true, expect_vectorized = true,\n max_instructions_per_byte = 2.2, stabilizer_pages = 16,\n)").unwrap();
    assert_eq!(
        contract,
        BenchContract {
            zero_alloc: Some(true),
            expect_vectorized: Some(true),
            max_instructions_per_byte: Some(Decimal {
                coefficient: 22,
                scale: 1
            }),
            stabilizer_pages: Some(16),
            max_l1d_misses_per_op: None,
        }
    );
    let contract = parse_attribute("@bench(zero_alloc = true, max_l1d_misses_per_op = 4)").unwrap();
    assert_eq!(contract.max_l1d_misses_per_op, Some(4));
    assert_eq!(
        parse_attribute("@bench() ").unwrap(),
        BenchContract::default()
    );
    assert_eq!(parse_attribute("@bench").unwrap(), BenchContract::default());
    assert_eq!(
        parse_attribute("@bench(max_instructions_per_byte=0.000000000000000001)")
            .unwrap()
            .max_instructions_per_byte,
        Some(Decimal {
            coefficient: 1,
            scale: 18
        })
    );
}

#[test]
fn malformed_unknown_duplicate_and_overflow_fail_closed() {
    for input in [
        "bench()",
        "@bench(,)",
        "@bench(zero_alloc=true,,)",
        "@bench(zero_alloc=truth)",
        "@bench(stabilizer_pages=1.2)",
        "@bench(max_instructions_per_byte=2.)",
        "@bench() trailing",
        "@bench(zero_alloc=true // comment\n)",
        "@bench(max_instructions_per_byte=-1)",
    ] {
        assert_eq!(
            parse_attribute(input).unwrap_err().kind,
            ErrorKind::Syntax,
            "{input}"
        );
    }
    assert_eq!(
        parse_attribute("@bench(unknown=1)").unwrap_err().kind,
        ErrorKind::UnknownField
    );
    assert_eq!(
        parse_attribute("@bench(zero_alloc=true, zero_alloc=false)")
            .unwrap_err()
            .kind,
        ErrorKind::DuplicateField
    );
    assert_eq!(
        parse_attribute("@bench(stabilizer_pages=18446744073709551616)")
            .unwrap_err()
            .kind,
        ErrorKind::NumberOverflow
    );
    assert_eq!(
        parse_attribute("@bench(max_instructions_per_byte=0.0000000000000000001)")
            .unwrap_err()
            .kind,
        ErrorKind::NumberOverflow
    );
    assert_eq!(
        parse_attribute(&" ".repeat(4097)).unwrap_err().kind,
        ErrorKind::InputLimit
    );
}
