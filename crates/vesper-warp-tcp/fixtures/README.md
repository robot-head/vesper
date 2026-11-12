# TCP checksum fixtures

`tcp_fixtures.pcap` is a classic pcap (`LINKTYPE_RAW` = 101, microsecond
timestamps) of eight **hand-constructed** packets. Nothing here was captured
from a real network or a real TCP stack, so it shows nothing about
interoperability.

`gen_fixtures.py` builds the file using only the Python standard library.
It computes the IPv4 header checksums and the TCP checksums over the IPv4/IPv6
pseudo-headers with its own RFC 1071 one's-complement routine, separate from
the Rust code under test. Run `python3 gen_fixtures.py` to regenerate it; the
output is deterministic.

| # | Family | Segment | What it covers |
| --- | --- | --- | --- |
| 0 | IPv4 | SYN with MSS 1460, SACK-permitted, timestamps, NOP, window scale 7 | Linux-style SYN option order |
| 1 | IPv4 | SYN,ACK with the same options | |
| 2 | IPv4 | PSH,ACK with timestamps and a 19-byte payload | Odd length, so the checksum pads the last byte |
| 3 | IPv4 | FIN,ACK with timestamps | |
| 4 | IPv4 | URG,ACK, urgent pointer 5, Telnet IAC payload | |
| 5 | IPv4 | Bare RST, sequence number 0xfffffffe | |
| 6 | IPv6 | ACK with two SACK blocks | IPv6 pseudo-header |
| 7 | IPv6 | PSH,ACK with a 255-byte payload | |

`tests/fixtures.rs` checks the following:
- every IPv4 header checksum and every TCP checksum verifies;
- recomputing each checksum gives the stored value;
- flipping one bit, or using the wrong pseudo-header, is detected;
- the fields and options decode as listed above;
- packets whose options use this crate's own layout (2, 3, 5, 6, 7) reserialize byte for byte.

The RFC 1071 §3 worked example (`00 01 f2 03 f4 f5 f6 f7` sums to `ddf2`) is a unit test in `src/wire.rs`.
