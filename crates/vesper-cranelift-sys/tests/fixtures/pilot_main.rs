// Host program for the SH0 lane 4 pilot: calls the functions Vesper built.
unsafe extern "C" {
    fn pilot_arith(a: i64, b: i64) -> i64;
    fn pilot_sum(n: i64) -> i64;
    fn pilot_call(x: i64) -> i64;
}

fn main() {
    // SAFETY: the pilot object defines these with the C calling convention.
    let v = unsafe {
        [
            pilot_arith(7, 5),
            pilot_arith(-9, 1 << 40),
            pilot_sum(0),
            pilot_sum(10),
            pilot_sum(1000),
            pilot_call(4),
            pilot_call(7),
        ]
    };
    let s: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    println!("{}", s.join(" "));
}
