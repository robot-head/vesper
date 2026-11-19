//! The Vesper port frees every sequence it allocates, on success and on
//! every error path (its own test binary: the allocation counter is
//! global). In the seed package the counter is always 0.

use std::path::PathBuf;
use vesper_aegis::sgx::SgxQuote;
use vesper_aegis::snp::SnpReport;
use vesper_aegis::tdx::TdxQuote;

fn data(path: &str) -> Vec<u8> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = if dir.ends_with("seed") {
        dir.parent().unwrap().to_path_buf()
    } else {
        dir
    };
    std::fs::read(dir.join("tests/data/vendor").join(path)).unwrap()
}

#[test]
fn parsers_do_not_leak() {
    let tdx = data("tdx-dcap-qvl/tdx_quote");
    let sgx = data("sgx-dcap-qvl/sgx_quote");
    let snp = data("snp-oak/milan_report.bin");
    let before = vesper_aegis::port_live_allocations();
    for n in [0, 10, 100, 700, tdx.len()] {
        let _ = TdxQuote::parse_prefix(&tdx[..n.min(tdx.len())]);
        let _ = TdxQuote::parse(&tdx[..n.min(tdx.len())]);
        let _ = SgxQuote::parse_padded(&sgx[..n.min(sgx.len())]);
        let _ = SnpReport::parse(&snp[..n.min(snp.len())]);
    }
    assert_eq!(vesper_aegis::port_live_allocations(), before);
}
