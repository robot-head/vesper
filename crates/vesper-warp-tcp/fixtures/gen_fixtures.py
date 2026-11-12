#!/usr/bin/env python3
"""Generate tcp_fixtures.pcap: hand-constructed TCP/IP packets.

Checksums are computed here with an independent RFC 1071 implementation
(standard library only), not with the Rust code under test. The pcap uses
LINKTYPE_RAW (101): each record is a bare IPv4 or IPv6 datagram.

Run: python3 gen_fixtures.py  (writes tcp_fixtures.pcap next to this file)
"""
import os
import struct


def csum(data: bytes) -> int:
    if len(data) % 2:
        data += b"\0"
    s = sum(struct.unpack("!%dH" % (len(data) // 2), data))
    while s >> 16:
        s = (s & 0xFFFF) + (s >> 16)
    return (~s) & 0xFFFF


def tcp(sport, dport, seq, ack, flags, window, options=b"", payload=b"", urg=0):
    assert len(options) % 4 == 0
    off = (20 + len(options)) // 4
    hdr = struct.pack("!HHIIBBHHH", sport, dport, seq, ack, off << 4, flags, window, 0, urg)
    return hdr + options + payload


def with_tcp_csum(seg: bytes, pseudo: bytes) -> bytes:
    c = csum(pseudo + seg)
    return seg[:16] + struct.pack("!H", c) + seg[18:]


def ipv4(src, dst, seg, ident):
    s, d = bytes(src), bytes(dst)
    pseudo = s + d + struct.pack("!BBH", 0, 6, len(seg))
    seg = with_tcp_csum(seg, pseudo)
    total = 20 + len(seg)
    ip = struct.pack("!BBHHHBBH4s4s", 0x45, 0, total, ident, 0x4000, 64, 6, 0, s, d)
    ip = ip[:10] + struct.pack("!H", csum(ip)) + ip[12:]
    return ip + seg


def ipv6(src, dst, seg):
    s, d = bytes(src), bytes(dst)
    pseudo = s + d + struct.pack("!I3xB", len(seg), 6)
    seg = with_tcp_csum(seg, pseudo)
    ip = struct.pack("!IHBB", 6 << 28, len(seg), 6, 64) + s + d
    return ip + seg


FIN, SYN, RST, PSH, ACK, URG = 1, 2, 4, 8, 16, 32
A4, B4 = [192, 0, 2, 1], [198, 51, 100, 7]
A6 = [0x20, 0x01, 0x0D, 0xB8] + [0] * 11 + [1]
B6 = [0x20, 0x01, 0x0D, 0xB8] + [0] * 11 + [2]

syn_opts = (
    struct.pack("!BBH", 2, 4, 1460)          # MSS
    + bytes([4, 2])                          # SACK-permitted
    + struct.pack("!BBII", 8, 10, 4242, 0)   # timestamps
    + bytes([1])                             # NOP
    + bytes([3, 3, 7])                       # window scale 7
)
ts = lambda v, e: bytes([1, 1]) + struct.pack("!BBII", 8, 10, v, e)
sack = bytes([1, 1, 5, 18]) + struct.pack("!IIII", 0x10000100, 0x10000200, 0x10000300, 0x10000400)

packets = [
    ipv4(A4, B4, tcp(40000, 80, 0x01020304, 0, SYN, 64240, syn_opts), 1),
    ipv4(B4, A4, tcp(80, 40000, 0xA0B0C0D0, 0x01020305, SYN | ACK, 65160, syn_opts), 2),
    ipv4(A4, B4, tcp(40000, 80, 0x01020305, 0xA0B0C0D1, PSH | ACK, 502, ts(4243, 4242), b"GET / HTTP/1.1\r\n\r\n!"), 3),
    ipv4(B4, A4, tcp(80, 40000, 0xA0B0C0D1, 0x01020318, FIN | ACK, 509, ts(4250, 4243)), 4),
    ipv4(A4, B4, tcp(40000, 80, 0x01020318, 0xA0B0C0D1, URG | ACK, 502, b"", b"\xff\xf4\xff\xfd\x06", urg=5), 5),
    ipv4(A4, B4, tcp(40000, 80, 0xFFFFFFFE, 0, RST, 0), 6),
    ipv6(A6, B6, tcp(50000, 443, 0x10000000, 0x20000000, ACK, 1024, sack)),
    ipv6(B6, A6, tcp(443, 50000, 0x20000000, 0x10000100, PSH | ACK, 2048, b"", bytes(range(255)))),
]

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "tcp_fixtures.pcap")
with open(out, "wb") as f:
    f.write(struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, 101))
    for i, p in enumerate(packets):
        f.write(struct.pack("<IIII", 1_700_000_000 + i, 0, len(p), len(p)))
        f.write(p)
print(out, len(packets), "packets")
for p in packets:
    print(p.hex())
