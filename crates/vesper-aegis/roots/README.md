# Pinned vendor certificates

Compiled into the crate by `src/roots.rs`, which records each certificate's
canonical vendor URL and SHA-256 and re-checks the digest on use. Each file
was taken from two independent published copies with identical bytes (see
the module documentation and `docs/parallel-results/aegis-vendor-collateral.md`).

| File | Certificate | SHA-256 (DER) |
| --- | --- | --- |
| `intel_sgx_root_ca.der` | Intel SGX Root CA | `44a0196b2b99f889b8e149e95b807a350e7424964399e885a7cbb8ccfab674d3` |
| `amd/milan_ark.der` | AMD ARK-Milan | `69d063b45344d26a2e94e1f4210de49ef555308287d4c174445c95639a540bcd` |
| `amd/milan_ask.der` | AMD SEV-Milan (ASK) | `67d303bd3905fd38db8b20e0793699870e7fa612eaad5dec358293fd8c0bac1b` |
| `amd/milan_asvk.der` | AMD SEV-VLEK-Milan (ASVK) | `c5e081f59b7efab1fe2f8b505e159704e72f29cab7ef7cf628a05a42439082f5` |
| `amd/genoa_ark.der` | AMD ARK-Genoa | `4c6598d19c18719c5dfd4a7d335f674e5bfe1d8f800cea2cf270c10d103db2f1` |
| `amd/genoa_ask.der` | AMD SEV-Genoa (ASK) | `5464738c1546aed5f2cecf1dc98c5c960a92e8913238a61711bc90ec6e828521` |
| `amd/genoa_asvk.der` | AMD SEV-VLEK-Genoa (ASVK) | `197e610743a917d6b9bb982a5a9226ccc0a15b611be0619e626aca9151457372` |
| `amd/turin_ark.der` | AMD ARK-Turin | `1f084161a44bb6d93778a904877d4819cafa5d05ef4193b2ded9dd9c73dd3f6a` |
| `amd/turin_ask.der` | AMD SEV-Turin (ASK) | `5b77ef5fe7a7a004fd9032668fba9d0fda22f88c4442069a479636a6ae3b3185` |
| `amd/turin_asvk.der` | AMD SEV-VLEK-Turin (ASVK) | `104e10a8bd060a3c20a434261a57d0588fd65a88915b4f65b08bdecaf8df1a3c` |
