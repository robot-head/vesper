# HTTP/3 interop probe

The `h3` crate client (0.0.8) over `quinn` (0.11.9, rustls 0.23.46 with
aws-lc-rs) on loopback UDP, against a Prism router served by
`vesper-prism-h3` on a Warp endpoint. The Warp software NIC is bridged to a
real UDP socket: received datagrams are delivered to the scripted `SoftNic`
and transmitted ones are sent back to the client's address.

This crate is deliberately outside the Vesper workspace, so its network
stack dependencies do not enter the main `Cargo.lock` or CI:

```sh
cd crates/vesper-prism-h3/interop
cp ../../../Cargo.lock .   # reuse the pinned versions; cargo adds the rest
cargo run
```

It prints one `INTEROP OK (path, status, headers, body)` line per request
or `INTEROP FAILED: ...`. Diagnostics from the Warp side go to stderr.
