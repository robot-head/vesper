//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("tensor", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = DIVERGENT
        .iter()
        .fold(port, |port, (from, to)| port.edit(from, to));
    port.build();
}

/// One deliberately wrong edit per ported Rust module (BUILD.bazel builds
/// the same edits); `crates/vesper-tensor/port/test-port.sh` checks that each
/// fails its test and that an unchanged test still passes. Each text occurs
/// exactly once across the package's `.ves` files.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str)] = &[
    // formats.rs, Q4_K (vesper/src/qk.ves): the sub-block minimum keeps the
    // wrong high bits; reference_fixtures q4_k_bytes_equal_llama_cpp fails.
    (
        "s.set(288 + j, b | ((lm >> 4) << 6));",
        "s.set(288 + j, b | ((lm >> 3) << 6));",
    ),
    // formats.rs, FP8 (vesper/src/ocp.ves): normal codes get the next
    // exponent; reference_fixtures
    // fp8_channel_block_and_mxfp8_codes_equal_ml_dtypes fails.
    (
        "let field = wrapping_u8(e + bias(e5m2));",
        "let field = wrapping_u8(e + bias(e5m2) + 1);",
    ),
    // quant.rs (vesper/src/rows.ves): Q4_0 rounds with 8.25 instead of 8.5;
    // reference_fixtures q8_0_and_q4_0_weights_equal_gguf_py fails.
    (
        "saturating_i8(v * id + 8.5)",
        "saturating_i8(v * id + 8.25)",
    ),
    // sampling.rs (vesper/src/sampling.ves): the token that crosses `top_p`
    // leaves the nucleus; tokenizer_sampling
    // sampling_follows_the_distribution_and_the_nucleus fails.
    (
        "            last = i;\n            break;",
        "            last = if i > 0 { i - 1 } else { 0 };\n            break;",
    ),
    // tokenizer.rs (vesper/src/tokens.ves): byte-fallback pieces are looked
    // up as lowercase `<0xab>`; tokenizer_sampling
    // encoder_matches_run_c_and_round_trips fails.
    ("        55 + d\n", "        87 + d\n"),
    // gguf.rs (vesper/src/store.ves): the default alignment is 16; abba
    // both_builds_read_the_same_gguf_files fails.
    (
        "    let mut alignment: u64 = 32;",
        "    let mut alignment: u64 = 16;",
    ),
    // json.rs (vesper/src/text.ves): a leading zero is accepted before two
    // more digits only; abba both_builds_read_the_same_safetensors_headers
    // fails.
    (
        "if at - start > 1 && b[start] == 48 {",
        "if at - start > 2 && b[start] == 48 {",
    ),
    // ops.rs (vesper/src/lib.ves): softmax divides by twice the sum;
    // inference_ops silu_swiglu_softmax_argmax fails.
    (
        "let v = f(in x, i) / sum;",
        "let v = f(in x, i) / (sum + sum);",
    ),
];
