# Real vendor samples

Publicly published TDX and SEV-SNP evidence and collateral, used by
`tests/vendor_samples.rs` to verify against the pinned vendor roots in
`roots/` (see `src/roots.rs`). The vendor endpoints (Intel PCS, AMD KDS) are
not reachable from the build sandbox, so every file comes from a published
open-source package, downloaded from the package registry (crates.io or the
Go module proxy) on 2026-11-13. Files are byte-for-byte copies unless noted.

| File | Source (package, path) | Licence | SHA-256 |
| --- | --- | --- | --- |
| `tdx-dcap-qvl/tdx_quote` | crates.io `dcap-qvl` 0.3.4, `sample/tdx_quote` | MIT | `c42f9164325024bca2757bc8819b11879a0a369132ea4e2b7c85df4805ea72db` |
| `tdx-dcap-qvl/tdx_quote_collateral.json` | same crate, `sample/tdx_quote_collateral.json` (PCCS collateral: TCB info, QE identity, PCK and root CA CRLs as hex, issuer chains) | MIT | `b0a5f5fd620a8881b1eda45261fdf30dd930b49aff93231556645c81fcb4c0bc` |
| `tdx-go-tdx-guest/tdx_prod_quote_SPR_E4.dat` | Go module `github.com/google/go-tdx-guest` v0.3.1, `testing/testdata/` (a production Sapphire Rapids E4 quote; the file appends 39 bytes of test text after the quote) | Apache-2.0 | `6dde5548bec99147fef832643301f113df99931547be26df8ac376c4eaa5b5a7` |
| `tdx-go-tdx-guest/sample_tcbInfo_response` | same, PCS `tdx/certification/v4/tcb` response body | Apache-2.0 | `1b96f799643d045c076fbc505c094a4fb26f51f4851feb8ac95f3fc12408f2d6` |
| `tdx-go-tdx-guest/sample_qeIdentity_response` | same, PCS `tdx/certification/v4/qe/identity` response body | Apache-2.0 | `86239584ed47792d02cdfc051cde95cb5169a0b74dc9dcc61c1ebaeb5e59ade0` |
| `tdx-go-tdx-guest/pckcrl` | same, PCS `pckcrl?ca=platform&encoding=der` (DER) | Apache-2.0 | `381598fbab078ecfba7df257d4396d3428ffb20a491f7ddbc14c52ec9c27e06f` |
| `tdx-go-tdx-guest/rootcrl.der` | same, Intel SGX Root CA CRL (DER) | Apache-2.0 | `f64b58a120f2882219944f1c0e0e3d87e8a02313e39c5d0bc658bdbcf524c41c` |
| `tdx-go-tdx-guest/*_issuer_chain.pem` | same module, `testing/test_cases.go` (the PCS `*-Issuer-Chain` response headers, URL-decoded into PEM) | Apache-2.0 | see report |
| `snp-virtee-sev/report_milan.hex` | crates.io `sev` 6.0.0, `tests/certs_data/report_milan.hex` (a Milan v2 attestation report, hex) | Apache-2.0 | `3d956e9104969a038031a6b63dd6c4b60000619d37702065994f7da76f88ae7d` |
| `snp-virtee-sev/vcek_milan.der` | same, `tests/certs_data/vcek_milan.der` | Apache-2.0 | `3bbfb6ee259f75a95d13168cfdf2e034181bb93c7c016825731cbe8ea16c95e1` |
| `snp-virtee-sev/cert_chain_milan` | same, `tests/certs_data/cert_chain_milan` (ASK + ARK PEM, as KDS serves it) | Apache-2.0 | `22e62f8d2c21a156470145fc75f7b5a377cb053ced3e97f0bd3f8d8ca5941ce6` |
| `snp-go-sev-guest/attestation.bin` | Go module `github.com/google/go-sev-guest` v0.15.0, `verify/testdata/attestation.bin` (a Milan v2 report) | Apache-2.0 | `377e6241d3b373ab1df80c0f96978594e7e21f4797dd6ea95e2957e1c1e26060` |
| `snp-go-sev-guest/vcek.testcer` | same, `verify/testdata/vcek.testcer` | Apache-2.0 | `0d057f9b6e29a69eda9c0154b259567d291c1c08d73a11e9d31ace07c435b6d8` |
| `snp-go-sev-guest/milanvlek.testcer` | same, `verify/testdata/milanvlek.testcer` (ASVK + ARK PEM; used to cross-check the pinned Milan ASVK) | Apache-2.0 | `3098f7e90ee7049b8cf116448d1bdf33b08847b4250bd99507cf73b5c49d3467` |

Licence texts: MIT (dcap-qvl, Phala Network) and Apache-2.0 (go-tdx-guest
and go-sev-guest, Google LLC; sev, the VirTEE project). These files are
test fixtures redistributed under those licences; the vendor-signed bytes
inside them are Intel's and AMD's.

No real AMD KDS CRL and no real VLEK-signed report are bundled: none is
published in a package we could reach. AMD revocation and VLEK paths are
tested with generated chains (`tests/revocation.rs`).
